//! Frame timings for performance capture (#151): how long the shell takes to prepare a frame,
//! the interval between frames, and how much was on screen. Recorded into the session's
//! recorder only while a capture runs; the report comes from `perf.report`.
use super::NodeCraftApp;
use nodecraft_engine::{
    Session,
    perf::{Metric, Workload},
};

/// Gives the session a monotonic microsecond clock where the platform has one. Browsers get
/// none for now, so their timings are reported as unsupported rather than wrong.
pub(super) fn install_clock(session: &mut Session) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let epoch = std::time::Instant::now();
        session.perf_mut().set_clock(Some(Box::new(move || u64::try_from(epoch.elapsed().as_micros()).unwrap_or(u64::MAX))));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = session;
}

impl NodeCraftApp {
    /// The frame's start time while a capture runs, recording the interval since the last one.
    pub(super) fn frame_started(&mut self) -> Option<u64> {
        let now = self.session.perf().now();
        if let (Some(_), Some(last)) = (now, self.last_frame) {
            self.session.perf_mut().record_since(Metric::FrameInterval, last);
        }
        self.last_frame = now;
        now
    }

    /// Records the frame's preparation time and what was on screen.
    pub(super) fn frame_finished(&mut self, started: Option<u64>) {
        let Some(started) = started else {
            return;
        };
        self.session.perf_mut().record_since(Metric::FramePrepare, started);
        let workload = self.session.document().canvases().get(&self.canvas).map(|canvas| {
            let painted = &self.painted;
            Workload {
                visible_nodes: painted.len(),
                total_nodes: canvas.nodes().len(),
                // A noodle is on screen when either of its cards is.
                visible_edges: canvas.edges().values().filter(|e| painted.contains_key(&e.from.node) || painted.contains_key(&e.to.node)).count(),
                total_edges: canvas.edges().len(),
            }
        });
        if let Some(workload) = workload {
            self.session.perf_mut().set_workload(workload);
        }
    }
}

#[cfg(test)]
mod tests;
