//! Frame timings for performance capture (#151): how long the shell takes to prepare a frame,
//! the interval between frames, and how much was on screen. Recorded into the session's
//! recorder only while a capture runs; the report comes from `perf.report`.
use super::NodeCraftApp;
use nodecraft_engine::{
    Session,
    perf::{Metric, PresentationSource},
};

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
        self.mesh_render = Some(render.clone());
    }

    /// Times presented frames (#526): while a capture runs, every frame asks for the next at
    /// once, and the interval between two such frames is a presented interval. `source` says
    /// how the host presents; give it only when presenting waits for vsync, so a back-to-back
    /// frame starts once the previous one was shown. `None` stops the pump.
    pub fn set_frame_pump(&mut self, source: Option<PresentationSource>) {
        self.frame_pump = source.is_some();
        self.pumped = false;
        self.session.perf_mut().set_presentation_source(source);
    }

    /// The frame's start time while a capture runs, recording the interval since the last one.
    /// Only a frame's first pass counts as presented: egui runs another pass after a discard
    /// within the same presented frame.
    pub(super) fn frame_started(&mut self, first_pass: bool) -> Option<u64> {
        self.frame_workload = None;
        let now = self.session.perf().now();
        if let (Some(_), Some(last)) = (now, self.last_frame) {
            self.session.perf_mut().record_since(Metric::FrameInterval, last);
        }
        if first_pass {
            if let (Some(_), Some(last), true) = (now, self.last_presented, self.pumped) {
                self.session.perf_mut().record_since(Metric::Presentation, last);
            }
            self.last_presented = now;
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
