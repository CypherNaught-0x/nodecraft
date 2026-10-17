//! Opt-in deterministic validation work counters. No paths, data or timing are collected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ValidationMetrics {
    pub full_asset_checks: usize,
    pub full_v1_checks: usize,
    pub asset_node_visits: usize,
    pub sniffs: usize,
    pub image_probes: usize,
    pub replay_preflights: usize,
}
thread_local! {
    static METRICS: std::cell::Cell<ValidationMetrics> = const { std::cell::Cell::new(ValidationMetrics {
        full_asset_checks: 0, full_v1_checks: 0, asset_node_visits: 0, sniffs: 0, image_probes: 0, replay_preflights: 0,
    }) };
}
pub(crate) fn record(update: impl FnOnce(&mut ValidationMetrics)) {
    METRICS.with(|metrics| {
        let mut value = metrics.get();
        update(&mut value);
        metrics.set(value);
    });
}
/// Return and reset this thread's counters, excluding work done on other threads.
pub fn take_validation_metrics() -> ValidationMetrics {
    METRICS.with(|metrics| metrics.replace(ValidationMetrics::default()))
}
