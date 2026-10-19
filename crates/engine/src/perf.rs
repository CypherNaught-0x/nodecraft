//! Local performance measurement (#151): a bounded recorder for frame and command timings, and a
//! versioned, machine-readable report for benchmarks (#65), the regression suite (#152) and
//! diagnostics.
//!
//! The host supplies the clock (microseconds, monotonic), since `std::time::Instant` isn't
//! available on every platform. Without one, timings are reported as unsupported, never as zero.
//! The recorder is off until started, keeps at most [`MAX_SAMPLES`] samples per metric (oldest
//! dropped first), and never does I/O. Reports hold only numbers, metric names and build and
//! platform labels: no document contents, prompts, assets, secrets or paths. Nothing is uploaded.
use serde::Serialize;
use std::{
    cell::Cell,
    collections::{BTreeMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
};

/// The report's format; bump it when a field changes meaning.
pub const REPORT_SCHEMA: &str = "nodecraft.perf/1";
/// Samples kept per metric.
pub const MAX_SAMPLES: usize = 4096;
/// One frame at 60 Hz, in microseconds.
pub const FRAME_BUDGET_US: u64 = 16_667;
/// Longer samples are dropped as invalid (a suspended machine or a clock jump, not a frame or
/// a command): one hour.
pub const MAX_SAMPLE_US: u64 = 3_600_000_000;
/// A frame interval longer than this is the app idling (it redraws only on input), not a slow
/// frame: it is counted as idle, not kept.
pub const IDLE_GAP_US: u64 = 250_000;
/// An interval this much over [`FRAME_BUDGET_US`] counts as a missed frame (one and a half
/// budgets): a frame on time at 60 Hz lands around 16,667 µs, give or take scheduling jitter.
pub const INTERVAL_BUDGET_US: u64 = FRAME_BUDGET_US * 3 / 2;

/// A monotonic microsecond clock from the platform, where it has one (not in the browser yet).
pub fn native_clock() -> Option<Clock> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let epoch = std::time::Instant::now();
        Some(Box::new(move || u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX)))
    }
    #[cfg(target_arch = "wasm32")]
    None
}

/// What is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    /// CPU time the shell spends preparing a frame (laying out and painting the canvas). Not
    /// end-to-end: GPU work and presentation aren't in it.
    FramePrepare,
    /// Time between the starts of consecutive frames, as the shell sees them; with a busy
    /// canvas this approaches the real frame rate, but it isn't a presentation timestamp.
    FrameInterval,
    /// Any command, start to finish (`Session::dispatch`).
    Dispatch,
    Undo,
    Redo,
    /// Opening a project (`file.open`).
    Load,
    /// Saving a project (`file.save`, `file.save_as`).
    Save,
}

impl Metric {
    pub const ALL: [Metric; 7] = [Metric::FramePrepare, Metric::FrameInterval, Metric::Dispatch, Metric::Undo, Metric::Redo, Metric::Load, Metric::Save];

    /// The time a frame metric's sample may take before it counts as over budget.
    fn budget(self) -> Option<u64> {
        match self {
            Metric::FramePrepare => Some(FRAME_BUDGET_US),
            Metric::FrameInterval => Some(INTERVAL_BUDGET_US),
            _ => None,
        }
    }

    /// The metric a command's dispatch also counts as, besides [`Metric::Dispatch`].
    pub fn of_command(id: &str) -> Option<Metric> {
        match id {
            "edit.undo" => Some(Metric::Undo),
            "edit.redo" => Some(Metric::Redo),
            "file.open" => Some(Metric::Load),
            "file.save" | "file.save_as" => Some(Metric::Save),
            _ => None,
        }
    }
}

/// How much was on the canvas in the last measured frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Workload {
    pub visible_nodes: usize,
    pub total_nodes: usize,
    pub visible_edges: usize,
    pub total_edges: usize,
}

/// A monotonic clock in microseconds, supplied by the host.
pub type Clock = Box<dyn Fn() -> u64>;

/// Bounded samples per metric. Off until started.
#[derive(Default)]
pub struct Recorder {
    enabled: bool,
    clock: Option<Clock>,
    clock_failed: Cell<bool>,
    samples: BTreeMap<Metric, VecDeque<u64>>,
    /// Samples ever recorded, dropped as invalid, and frame intervals skipped as idle, per
    /// metric (since the last clear).
    recorded: BTreeMap<Metric, u64>,
    invalid: BTreeMap<Metric, u64>,
    idle: BTreeMap<Metric, u64>,
    workload: Option<Workload>,
    /// The renderer the host draws with (GPU adapter and backend), if it said.
    renderer: Option<String>,
}

impl Recorder {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    pub fn has_clock(&self) -> bool {
        self.clock.is_some() && !self.clock_failed.get()
    }
    pub fn set_clock(&mut self, clock: Option<Clock>) {
        self.clock = clock;
        self.clock_failed.set(false);
    }
    /// Drops every sample and count.
    pub fn clear(&mut self) {
        self.samples.clear();
        self.recorded.clear();
        self.invalid.clear();
        self.idle.clear();
        self.workload = None;
    }
    /// What the host draws with, for the report (e.g. "Apple M2 (Metal)"); at most 128 chars.
    pub fn set_renderer(&mut self, renderer: Option<String>) {
        self.renderer = renderer.map(|name| name.chars().filter(|c| !c.is_control()).take(128).collect());
    }
    /// The clock's time while recording and available. An unwinding callback failure
    /// disables this clock until set_clock replaces it; historical samples survive.
    /// Abort-only targets cannot contain a panic that aborts instead of unwinding.
    pub fn now(&self) -> Option<u64> {
        if !self.enabled || !self.has_clock() {
            return None;
        }
        let clock = self.clock.as_ref()?;
        match catch_unwind(AssertUnwindSafe(clock)) {
            Ok(time) => Some(time),
            Err(payload) => {
                self.clock_failed.set(true);
                // A host's panic payload may itself panic on Drop. Try cleanup once;
                // never recursively drop another hostile payload. The exceptional
                // retained bytes depend on that host payload, not a bounded-memory
                // guarantee. Replacing the clock is the only way to rearm it.
                if let Err(cleanup_payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
                    std::mem::forget(cleanup_payload);
                }
                None
            }
        }
    }
    /// The time since `start` (from [`Self::now`]) recorded as one sample of `metric`.
    pub fn record_since(&mut self, metric: Metric, start: u64) {
        if let Some(end) = self.now() {
            match end.checked_sub(start) {
                Some(elapsed) => self.record(metric, elapsed),
                // A clock that ran backwards gives no sample.
                None => self.count_invalid(metric),
            }
        }
    }
    /// One sample of `metric`, in microseconds; ignored while not recording.
    pub fn record(&mut self, metric: Metric, micros: u64) {
        if !self.enabled {
            return;
        }
        if micros > MAX_SAMPLE_US {
            self.count_invalid(metric);
            return;
        }
        if metric == Metric::FrameInterval && micros > IDLE_GAP_US {
            let idle = self.idle.entry(metric).or_default();
            *idle = idle.saturating_add(1);
            return;
        }
        let samples = self.samples.entry(metric).or_default();
        if samples.len() >= MAX_SAMPLES {
            samples.pop_front();
        }
        samples.push_back(micros);
        let recorded = self.recorded.entry(metric).or_default();
        *recorded = recorded.saturating_add(1);
    }
    fn count_invalid(&mut self, metric: Metric) {
        let invalid = self.invalid.entry(metric).or_default();
        *invalid = invalid.saturating_add(1);
    }
    /// What the last measured frame showed; ignored while not recording.
    pub fn set_workload(&mut self, workload: Workload) {
        if self.enabled {
            self.workload = Some(workload);
        }
    }

    /// The report of what is recorded now.
    pub fn report(&self) -> Report {
        let metrics = Metric::ALL
            .into_iter()
            .map(|metric| {
                let samples = self.samples.get(&metric);
                let mut sorted: Vec<u64> = samples.map(|s| s.iter().copied().collect()).unwrap_or_default();
                sorted.sort_unstable();
                let at = |p: u64| percentile(&sorted, p);
                MetricReport {
                    metric,
                    unit: "us",
                    supported: self.has_clock(),
                    samples: sorted.len(),
                    recorded: self.recorded.get(&metric).copied().unwrap_or(0),
                    invalid: self.invalid.get(&metric).copied().unwrap_or(0),
                    idle: (metric == Metric::FrameInterval).then(|| self.idle.get(&metric).copied().unwrap_or(0)),
                    p50: at(50),
                    p95: at(95),
                    p99: at(99),
                    max: sorted.last().copied(),
                    over_frame_budget: metric.budget().map(|budget| sorted.iter().filter(|us| **us > budget).count()),
                }
            })
            .collect();
        Report {
            schema: REPORT_SCHEMA,
            recording: self.enabled,
            clock: self.has_clock(),
            max_samples: MAX_SAMPLES,
            frame_budget_us: FRAME_BUDGET_US,
            interval_budget_us: INTERVAL_BUDGET_US,
            idle_gap_us: IDLE_GAP_US,
            warm_up: "none: every sample since the last clear is kept; clear after warming up",
            note: "frame_prepare is CPU time in the shell, not end-to-end; it alone doesn't show 60 fps",
            metrics,
            workload: self.workload,
            environment: Environment {
                version: crate::build_info::VERSION,
                revision: crate::build_info::short_sha(),
                profile: if cfg!(debug_assertions) { "debug" } else { "release" },
                os: std::env::consts::OS,
                arch: std::env::consts::ARCH,
                cpus: cpus(),
                renderer: self.renderer.clone(),
            },
        }
    }
}

/// Logical CPUs, where the platform says.
fn cpus() -> Option<usize> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::available_parallelism().ok().map(std::num::NonZeroUsize::get)
    }
    #[cfg(target_arch = "wasm32")]
    None
}

/// The nearest-rank `p`th percentile of `sorted` (ascending), or `None` when empty.
fn percentile(sorted: &[u64], p: u64) -> Option<u64> {
    let n = u64::try_from(sorted.len()).ok().filter(|n| *n > 0)?;
    // Rank ceil(p/100 × n), at least 1.
    let rank = (p.saturating_mul(n).saturating_add(99) / 100).clamp(1, n);
    sorted.get(usize::try_from(rank - 1).ok()?).copied()
}

/// A versioned performance report. Absent values (`null`) mean "not measured", never zero.
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub recording: bool,
    /// Whether a host clock is available (present and not failed).
    pub clock: bool,
    pub max_samples: usize,
    pub frame_budget_us: u64,
    pub interval_budget_us: u64,
    pub idle_gap_us: u64,
    pub warm_up: &'static str,
    pub note: &'static str,
    pub metrics: Vec<MetricReport>,
    pub workload: Option<Workload>,
    pub environment: Environment,
}

#[derive(Debug, Serialize)]
pub struct MetricReport {
    pub metric: Metric,
    pub unit: &'static str,
    pub supported: bool,
    /// Samples kept (at most [`MAX_SAMPLES`]); percentiles are over these.
    pub samples: usize,
    /// Samples recorded since the last clear, including ones since dropped for space.
    pub recorded: u64,
    /// Samples refused as invalid (clock ran backwards, or longer than [`MAX_SAMPLE_US`]).
    pub invalid: u64,
    /// Frame intervals over [`IDLE_GAP_US`], skipped as the app idling; only for the interval.
    pub idle: Option<u64>,
    pub p50: Option<u64>,
    pub p95: Option<u64>,
    pub p99: Option<u64>,
    pub max: Option<u64>,
    /// Frames over budget ([`FRAME_BUDGET_US`] to prepare, [`INTERVAL_BUDGET_US`] between
    /// frames); only for frame metrics.
    pub over_frame_budget: Option<usize>,
}

/// Build and platform labels: nothing about the person or their files.
#[derive(Debug, Serialize)]
pub struct Environment {
    pub version: &'static str,
    pub revision: Option<&'static str>,
    pub profile: &'static str,
    pub os: &'static str,
    pub arch: &'static str,
    pub cpus: Option<usize>,
    /// GPU adapter and backend, as the host reported them.
    pub renderer: Option<String>,
}

#[cfg(test)]
mod tests;
