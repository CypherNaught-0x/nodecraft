//! Media a model call couldn't send, found before its cost is accepted (#675). A job reads at
//! most [`MAX_MEDIA_INPUT_BYTES`] of media, enforced when it reads its inputs; this checks the
//! stored sizes of the inputs each planned job already knows, so `run.quote` shows the run as
//! blocked and `run.start` refuses it, before anything is sent or charged.

use crate::generate::{MAX_MEDIA_INPUT_BYTES, MediaSource};
use nodecraft_exec::planner::RunPlan;
use nodecraft_nodes::JobSpec;
use nodecraft_types::{NodeId, Value};

/// Lists nested deeper than this aren't followed; the job's own admission refuses them.
const MAX_DEPTH: usize = 32;
/// Media values counted for one job. Past this the count stops: a total already over the limit is
/// reported, anything else is left to admission, whose item limit refuses the job.
const MAX_MEDIA_VALUES: usize = 4096;

/// A step whose job sends more media than one model call allows: its (owning) node, the role of
/// its largest input entry and the job's total, in bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Oversized {
    pub node: NodeId,
    pub role: String,
    pub bytes: u64,
}

impl Oversized {
    /// A fixed-form reason naming the input and the limit; no file names or titles.
    pub fn reason(&self, owner: NodeId) -> String {
        format!(
            "node {owner} [media_too_large]: its media inputs total {} MiB, more than the {} MiB one model call can send (largest: {}); use smaller or shorter files",
            mib(self.bytes),
            MAX_MEDIA_INPUT_BYTES / (1024 * 1024),
            self.role
        )
    }
}

/// Rounded up, so a total just over the limit never reads as equal to it.
fn mib(bytes: u64) -> u64 {
    bytes.div_ceil(1024 * 1024)
}

/// The steps of `plan` whose known jobs send more media than allowed, at most one per step.
/// An input whose size can't be found is left to the job's own admission, which refuses it.
pub(super) fn oversized(plan: &RunPlan, source: &dyn MediaSource) -> Vec<Oversized> {
    let mut found = Vec::new();
    for step in &plan.steps {
        let Some(jobs) = &step.jobs else { continue };
        if let Some((role, bytes)) = jobs.iter().filter_map(|job| over(job, source)).next() {
            found.push(Oversized { node: step.node, role, bytes });
        }
    }
    found
}

/// The largest input entry's role and the total, when `job`'s media is over the limit.
fn over(job: &JobSpec, source: &dyn MediaSource) -> Option<(String, u64)> {
    let JobSpec::Generate { inputs, .. } = job else { return None };
    let mut total = 0u64;
    let mut counted = 0usize;
    let mut largest: Option<(&str, u64)> = None;
    for input in inputs {
        let mut role_bytes = 0u64;
        let mut stack = vec![(&input.value, 0usize)];
        while let Some((value, depth)) = stack.pop() {
            match value {
                Value::List(items) if depth < MAX_DEPTH => stack.extend(items.iter().map(|item| (item, depth + 1))),
                Value::Media(asset) => {
                    counted = counted.saturating_add(1);
                    let size = source.size(asset).ok()?;
                    role_bytes = role_bytes.saturating_add(size);
                    total = total.saturating_add(size);
                }
                _ => {}
            }
            if counted > MAX_MEDIA_VALUES {
                break;
            }
        }
        if largest.is_none_or(|(_, bytes)| role_bytes > bytes) {
            largest = Some((input.role.as_str(), role_bytes));
        }
        if counted > MAX_MEDIA_VALUES {
            break;
        }
    }
    let (role, _) = largest?;
    (total > MAX_MEDIA_INPUT_BYTES).then(|| (role.to_owned(), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_nodes::GenerateInput;
    use nodecraft_types::{AssetRef, Blake3, Error, MediaMeta};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// Sizes by the first byte of each asset's hash; an unknown asset has no size.
    struct Sizes(BTreeMap<u8, u64>);
    impl MediaSource for Sizes {
        fn size(&self, asset: &AssetRef) -> Result<u64, Error> {
            self.0.get(&asset.hash.as_bytes()[0]).copied().ok_or_else(|| Error::new(nodecraft_types::ErrorKind::NotFound, "no such asset"))
        }
        fn read(&self, _: &AssetRef, _: u64) -> Result<Arc<[u8]>, Error> {
            Err(Error::new(nodecraft_types::ErrorKind::Internal, "sizes are checked without reading"))
        }
    }
    fn asset(n: u8) -> Value {
        Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "video/mp4".parse().unwrap(), meta: MediaMeta::default() })
    }
    fn job(inputs: Vec<(&str, Value)>) -> JobSpec {
        JobSpec::Generate {
            node: NodeId::from_bytes([1; 16]),
            provider: "p".into(),
            model: "m".into(),
            params: Default::default(),
            inputs: inputs.into_iter().map(|(role, value)| GenerateInput { role: role.into(), value }).collect(),
            outputs: vec![],
            instance: None,
        }
    }
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn a_job_over_the_limit_names_its_largest_input_and_its_total() {
        let sizes = Sizes([(1, 60 * MIB), (2, 3 * MIB), (3, 2 * MIB)].into());
        let job = job(vec![("source", asset(1)), ("image", Value::List(vec![asset(2), asset(3)]))]);
        assert_eq!(over(&job, &sizes), Some(("source".to_owned(), 65 * MIB)));
        let reason = Oversized { node: NodeId::from_bytes([1; 16]), role: "source".into(), bytes: 65 * MIB }.reason(NodeId::from_bytes([1; 16]));
        assert!(reason.contains("[media_too_large]") && reason.contains("65 MiB") && reason.contains("64 MiB") && reason.contains("source"), "{reason}");
    }

    #[test]
    fn a_job_at_the_limit_or_with_an_unknown_input_is_left_to_admission() {
        let sizes = Sizes([(1, 32 * MIB), (2, 32 * MIB)].into());
        assert_eq!(over(&job(vec![("a", asset(1)), ("b", asset(2))]), &sizes), None, "exactly the limit is allowed");
        let sizes = Sizes([(1, 100 * MIB)].into());
        assert_eq!(over(&job(vec![("a", asset(1)), ("b", asset(9))]), &sizes), None, "an unknown asset is refused by the job itself");
        assert_eq!(over(&job(vec![("prompt", Value::Text("x".into()))]), &sizes), None);
        let many = Value::List(vec![asset(1); MAX_MEDIA_VALUES + 1]);
        assert_eq!(over(&job(vec![("a", many.clone())]), &Sizes([(1, 1)].into())), None, "too many values are refused by admission");
        assert_eq!(
            over(&job(vec![("a", many), ("b", asset(2))]), &Sizes([(1, MIB)].into())),
            Some(("a".to_owned(), 4097 * MIB)),
            "what is counted is reported"
        );
    }

    #[test]
    fn a_just_over_total_never_reads_as_the_limit() {
        assert_eq!(mib(MAX_MEDIA_INPUT_BYTES + 1), 65);
        assert_eq!(mib(0), 0);
    }
}
