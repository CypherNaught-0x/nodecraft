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
use std::collections::{BTreeMap, VecDeque};

/// The report's format; bump it when a field changes meaning.
pub const REPORT_SCHEMA: &str = "nodecraft.perf/1";
/// Samples kept per metric.
pub const MAX_SAMPLES: usize = 4096;
/// One frame at 60 Hz, in microseconds.
pub const FRAME_BUDGET_US: u64 = 16_667;
/// Longer samples are dropped as invalid (a suspended machine or a clock jump, not a frame or
/// a command): one hour.
pub const MAX_SAMPLE_US: u64 = 3_600_000_000;

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

    fn is_frame(self) -> bool {
        matches!(self, Metric::FramePrepare | Metric::FrameInterval)
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
    samples: BTreeMap<Metric, VecDeque<u64>>,
    /// Samples ever recorded, and dropped as invalid, per metric (since the last clear).
    recorded: BTreeMap<Metric, u64>,
    invalid: BTreeMap<Metric, u64>,
    workload: Option<Workload>,
}

impl Recorder {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
    pub fn has_clock(&self) -> bool {
        self.clock.is_some()
    }
    pub fn set_clock(&mut self, clock: Option<Clock>) {
        self.clock = clock;
    }
    /// Drops every sample and count.
    pub fn clear(&mut self) {
        self.samples.clear();
        self.recorded.clear();
        self.invalid.clear();
        self.workload = None;
    }
    /// The clock's time, while recording and if there is a clock.
    pub fn now(&self) -> Option<u64> {
        self.clock.as_ref().filter(|_| self.enabled).map(|clock| clock())
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
                    supported: self.clock.is_some(),
                    samples: sorted.len(),
                    recorded: self.recorded.get(&metric).copied().unwrap_or(0),
                    invalid: self.invalid.get(&metric).copied().unwrap_or(0),
                    p50: at(50),
                    p95: at(95),
                    p99: at(99),
                    max: sorted.last().copied(),
                    over_frame_budget: metric.is_frame().then(|| sorted.iter().filter(|us| **us > FRAME_BUDGET_US).count()),
                }
            })
            .collect();
        Report {
            schema: REPORT_SCHEMA,
            recording: self.enabled,
            clock: self.clock.is_some(),
            max_samples: MAX_SAMPLES,
            frame_budget_us: FRAME_BUDGET_US,
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
            },
        }
    }
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
    /// Whether the host gave a clock; without one, no timing is supported.
    pub clock: bool,
    pub max_samples: usize,
    pub frame_budget_us: u64,
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
    pub p50: Option<u64>,
    pub p95: Option<u64>,
    pub p99: Option<u64>,
    pub max: Option<u64>,
    /// Frames over [`FRAME_BUDGET_US`]; only for frame metrics.
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
}

#[cfg(test)]
mod tests;
