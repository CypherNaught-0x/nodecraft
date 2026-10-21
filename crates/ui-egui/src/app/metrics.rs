//! Frame timings for performance capture (#151): how long the shell takes to prepare a frame,
//! the interval between frames, and how much was on screen. Recorded into the session's
//! recorder only while a capture runs; the report comes from `perf.report`.
use super::NodeCraftApp;
use nodecraft_engine::{Session, perf::Metric};

/// Gives the session a monotonic microsecond clock where the platform has one. Browsers get
/// none for now, so their timings are reported as unsupported rather than wrong.
pub(super) fn install_clock(session: &mut Session) {
    session.perf_mut().set_clock(nodecraft_engine::perf::native_clock());
}

impl NodeCraftApp {
    /// Names the renderer (GPU adapter and backend) in performance reports.
    pub fn set_renderer(&mut self, renderer: String) {
        self.session.perf_mut().set_renderer(Some(renderer));
    }

    /// Lets cards draw 3D models with eframe's wgpu renderer (#111). Call once at start-up.
    pub fn install_mesh_renderer(&mut self, render: &eframe::egui_wgpu::RenderState) {
        crate::mesh_view::install(render);
        self.mesh_gpu = true;
    }

    /// The frame's start time while a capture runs, recording the interval since the last one.
    pub(super) fn frame_started(&mut self) -> Option<u64> {
        self.frame_workload = None;
        let now = self.session.perf().now();
        if let (Some(_), Some(last)) = (now, self.last_frame) {
            self.session.perf_mut().record_since(Metric::FrameInterval, last);
        }
        self.last_frame = now;
        now
    }

    /// Records the frame's preparation time and what was on screen.
    pub(super) fn frame_finished(&mut self, started: Option<u64>) {
        if let Some(workload) = self.frame_workload.take() {
            self.session.perf_mut().set_workload(workload);
        }
        if let Some(started) = started {
            self.session.perf_mut().record_since(Metric::FramePrepare, started);
        }
    }
}

#[cfg(test)]
mod tests;

pub(super) mod visibility;
