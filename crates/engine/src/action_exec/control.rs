//! Caller-owned live work, shared by every action and PDF page in one round.
use super::LocalActions;
use nodecraft_types::{ParamValues, Value};
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

/// CPU reference pixel visits, input bytes, and weighted interpretation checkpoints.
/// Explicit runs retain their existing operation limits instead of this live-round quota.
pub(crate) const LIVE_WORK_UNITS: usize = 100_000_000;
const PDF_POLL_UNITS: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stop {
    Cancelled,
    Budget,
}
impl fmt::Display for Stop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cancelled => "live action was cancelled",
            Self::Budget => "Live action work budget reached; reduce the input, effect strength or PDF page range, or run the node explicitly.",
        })
    }
}

pub(super) struct Work {
    cancelled: Arc<AtomicBool>,
    remaining: AtomicUsize,
    exhausted: AtomicBool,
}
impl Work {
    fn new(cancelled: Arc<AtomicBool>, units: usize) -> Self {
        Self { cancelled, remaining: AtomicUsize::new(units), exhausted: AtomicBool::new(false) }
    }
    fn check(&self) -> Result<(), Stop> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(Stop::Cancelled)
        } else if self.exhausted.load(Ordering::Acquire) {
            Err(Stop::Budget)
        } else {
            Ok(())
        }
    }
    fn charge(&self, units: u64) -> Result<(), Stop> {
        self.check()?;
        if units == 0 {
            return Ok(());
        }
        let charged = if let Ok(units) = usize::try_from(units) {
            let mut remaining = self.remaining.load(Ordering::Acquire);
            loop {
                self.check()?;
                let Some(next) = remaining.checked_sub(units) else { break false };
                match self.remaining.compare_exchange_weak(remaining, next, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => break true,
                    Err(actual) => remaining = actual,
                }
            }
        } else {
            false
        };
        if !charged {
            self.exhausted.store(true, Ordering::Release);
        }
        self.check()
    }
}

fn value_units(value: &Value, depth: usize) -> Result<u64, String> {
    if depth > 16 {
        return Err("live action inputs are nested too deeply".into());
    }
    match value {
        Value::Text(text) => Ok(u64::try_from(text.len()).unwrap_or(u64::MAX)),
        Value::List(values) => values.iter().try_fold(1u64, |total, value| Ok(total.saturating_add(value_units(value, depth.saturating_add(1))?))),
        _ => Ok(1),
    }
}

impl LocalActions {
    pub(crate) fn with_live_control(mut self, cancelled: Arc<AtomicBool>, units: usize) -> Self {
        self.capture_cancel = Arc::clone(&cancelled);
        self.work = Some(Arc::new(Work::new(cancelled, units)));
        self
    }
    pub(crate) fn admit_live_action(&self, params: &ParamValues, inputs: &[Value]) -> Result<(), String> {
        let mut units = 1024u64;
        for value in params.values().chain(inputs) {
            units = units.saturating_add(value_units(value, 0)?);
        }
        self.charge_work(units)
    }
    pub(crate) fn live_interrupted(&self) -> bool {
        self.capture_cancel.load(Ordering::Acquire) || self.work.as_ref().is_some_and(|work| work.charge(PDF_POLL_UNITS).is_err())
    }
    pub(super) fn check_work(&self) -> Result<(), String> {
        if self.capture_cancel.load(Ordering::Acquire) {
            return Err(Stop::Cancelled.to_string());
        }
        self.work.as_ref().map_or(Ok(()), |work| work.check().map_err(|error| error.to_string()))
    }
    pub(super) fn charge_work(&self, units: u64) -> Result<(), String> {
        self.check_work()?;
        self.work.as_ref().map_or(Ok(()), |work| work.charge(units).map_err(|error| error.to_string()))
    }
    pub(crate) fn control_error(&self, fallback: String) -> String {
        self.check_work().err().unwrap_or(fallback)
    }
    pub(super) fn work_check(&self) -> Arc<dyn Fn(u64) -> bool + Send + Sync> {
        let cancelled = Arc::clone(&self.capture_cancel);
        let work = self.work.clone();
        Arc::new(move |units| cancelled.load(Ordering::Acquire) || work.as_ref().is_some_and(|work| work.charge(units).is_err()))
    }
    pub(super) fn pdf_check(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let check = self.work_check();
        Arc::new(move || check(PDF_POLL_UNITS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quota_is_shared_and_exhaustion_is_sticky_without_underflow() {
        let flag = Arc::new(AtomicBool::new(false));
        let work = Arc::new(Work::new(flag.clone(), 10));
        let other = work.clone();
        assert_eq!(work.charge(4), Ok(()));
        assert_eq!(other.charge(6), Ok(()));
        assert_eq!(work.charge(1), Err(Stop::Budget));
        assert_eq!(other.charge(0), Err(Stop::Budget));
        assert_eq!(work.remaining.load(Ordering::Acquire), 0);
        flag.store(true, Ordering::Release);
        assert_eq!(work.check(), Err(Stop::Cancelled));
        assert_eq!(Work::new(Arc::new(AtomicBool::new(false)), 1).charge(u64::MAX), Err(Stop::Budget));
    }

    #[test]
    fn independent_rounds_do_not_share_a_quota_or_cancel_flag() {
        let first = Work::new(Arc::new(AtomicBool::new(true)), 0);
        let second = Work::new(Arc::new(AtomicBool::new(false)), 100);
        assert_eq!(first.charge(1), Err(Stop::Cancelled));
        assert_eq!(second.charge(100), Ok(()));
    }
    fn pdf_fixture(pages: &[&str]) -> (nodecraft_format::AssetStore, ParamValues, Value, usize) {
        let bytes = nodecraft_media::pdf_fixture::pdf(pages);
        let mut assets = nodecraft_format::AssetStore::default();
        let hash = assets.insert(&bytes).unwrap();
        let input = Value::Media(nodecraft_types::AssetRef { hash, mime: "application/pdf".parse().unwrap(), meta: Default::default() });
        let params = [
            ("first".into(), Value::Unsigned(nodecraft_types::Unsigned(1))),
            ("last".into(), Value::Unsigned(nodecraft_types::Unsigned(0))),
            ("separator".into(), Value::Text("blank".into())),
        ]
        .into();
        (assets, params, input, bytes.len())
    }

    #[test]
    fn another_action_cannot_reset_the_rounds_work_budget() {
        use nodecraft_exec::run::ActionExecutor;
        let (assets, params, input, _) = pdf_fixture(&["BT /F1 12 Tf 20 100 Td (Hello) Tj ET"]);
        let probe = LocalActions::new(assets.clone()).with_live_control(Arc::new(AtomicBool::new(false)), 100_000);
        assert_eq!(probe.execute(nodecraft_nodes::document::TEXT, &params, std::slice::from_ref(&input)).unwrap(), Value::Text("Hello".into()));
        let cost = 100_000 - probe.work.as_ref().unwrap().remaining.load(Ordering::Acquire);
        assert!(cost > 0);
        let actions = LocalActions::new(assets).with_live_control(Arc::new(AtomicBool::new(false)), cost * 2 - 1);
        actions.execute(nodecraft_nodes::document::TEXT, &params, std::slice::from_ref(&input)).unwrap();
        let error = actions.execute(nodecraft_nodes::document::TEXT, &params, &[input]).unwrap_err();
        assert!(error.contains("work budget") && error.contains("run the node explicitly"), "{error}");
    }

    #[test]
    fn another_pdf_page_cannot_reset_the_actions_work_budget_or_return_partial_text() {
        use nodecraft_exec::run::ActionExecutor;
        let content = "BT /F1 12 Tf 20 100 Td (Hello) Tj ET";
        let (assets, params, input, _) = pdf_fixture(&[content, content]);
        let mut first = params.clone();
        first.insert("last".into(), Value::Unsigned(nodecraft_types::Unsigned(1)));
        let probe = LocalActions::new(assets.clone()).with_live_control(Arc::new(AtomicBool::new(false)), 100_000);
        probe.execute(nodecraft_nodes::document::TEXT, &first, std::slice::from_ref(&input)).unwrap();
        let first_cost = 100_000 - probe.work.as_ref().unwrap().remaining.load(Ordering::Acquire);
        let actions = LocalActions::new(assets).with_live_control(Arc::new(AtomicBool::new(false)), first_cost + 64);
        let error = actions.execute(nodecraft_nodes::document::TEXT, &params, &[input]).unwrap_err();
        assert!(error.contains("work budget"), "the first page must not escape as partial successful text: {error}");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn a_superseded_slow_pdf_action_stops_before_finishing_its_text_operator() {
        use nodecraft_exec::run::ActionExecutor;
        let glyphs = 900_000usize;
        let content = format!("BT /F1 12 Tf 20 100 Td ({}) Tj ET", "A".repeat(glyphs));
        let (assets, params, input, bytes) = pdf_fixture(&[&content]);
        let flag = Arc::new(AtomicBool::new(false));
        let actions = Arc::new(LocalActions::new(assets).with_live_control(flag.clone(), LIVE_WORK_UNITS));
        let work = actions.work.as_ref().unwrap().clone();
        let worker = std::thread::spawn(move || actions.execute(nodecraft_nodes::document::TEXT, &params, &[input]));
        // Wait for interpretation checkpoints, not merely the encoded-file charge. This
        // observes real action progress without sleeping or relying on a speed comparison.
        let threshold = LIVE_WORK_UNITS.saturating_sub(bytes).saturating_sub(16 * 64);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while work.remaining.load(Ordering::Acquire) >= threshold && !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        let started = work.remaining.load(Ordering::Acquire) < threshold;
        flag.store(true, Ordering::Release);
        let result = worker.join().unwrap();
        assert!(started, "the slow action must have entered interpretation before it is superseded");
        let error = result.unwrap_err();
        assert!(error.contains("cancelled"), "{error}");
        let spent = LIVE_WORK_UNITS - work.remaining.load(Ordering::Acquire);
        assert!(spent < bytes.saturating_add(glyphs.saturating_mul(64)), "cancelled work interpreted the whole text operator");
    }
}
