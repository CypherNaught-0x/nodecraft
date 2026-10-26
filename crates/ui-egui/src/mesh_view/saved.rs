//! The local preview never becomes document authority until the app dispatches mesh.view_set.
use super::{MeshView, Orbit, Style};
use nodecraft_graph::Node;
use nodecraft_nodes::mesh::view::{self, State};
use nodecraft_types::{CanvasId, ProjectId};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Saved {
    binding: Option<(ProjectId, CanvasId, State)>,
    pending: bool,
    dragging: bool,
    scroll_until: Option<f64>,
}

impl MeshView {
    pub(crate) fn bind(&mut self, project: ProjectId, canvas: CanvasId, node: &Node) {
        let state = view::supports(&node.kind).then(|| State::read(&node.params).ok()).flatten();
        let binding = state.map(|state| (project, canvas, state));
        if self.saved.binding != binding {
            self.saved = Saved { binding, ..Default::default() };
            if let Some(state) = state {
                self.restore(state);
            }
        }
    }

    pub(crate) fn owner(&self) -> Option<(ProjectId, CanvasId)> {
        self.saved.binding.map(|(project, canvas, _)| (project, canvas))
    }

    fn restore(&mut self, state: State) {
        self.orbit = Orbit { yaw: state.yaw.to_radians() as f32, pitch: state.pitch.to_radians() as f32, zoom: state.zoom as f32 };
        self.style = match state.style {
            view::Style::Shaded => Style::Shaded,
            view::Style::Unlit => Style::Unlit,
            view::Style::Wireframe => Style::Wireframe,
        };
    }

    pub(crate) fn discard(&mut self) {
        if let Some((_, _, state)) = self.saved.binding {
            self.restore(state);
        }
        self.saved.pending = false;
        self.saved.dragging = false;
        self.saved.scroll_until = None;
    }

    pub(super) fn scrolling(&mut self, now: f64) {
        self.saved.scroll_until = now.is_finite().then_some(now + 0.25);
    }

    pub(super) fn changed(&mut self, dragging: bool) {
        self.saved.pending = self.saved.binding.is_some();
        self.saved.dragging = dragging;
    }

    pub(crate) fn take_ready(&mut self, now: f64, pointer_down: bool, force: bool) -> Option<State> {
        if !self.saved.pending || (!force && ((self.saved.dragging && pointer_down) || self.saved.scroll_until.is_some_and(|until| now < until))) {
            return None;
        }
        self.saved.pending = false;
        self.saved.dragging = false;
        self.saved.scroll_until = None;
        Some(State {
            yaw: f64::from(self.orbit.yaw).to_degrees().rem_euclid(360.0),
            // f32 renderer bounds round slightly beyond the saved f64 bounds.
            pitch: f64::from(self.orbit.pitch).to_degrees().clamp(-view::MAX_PITCH, view::MAX_PITCH),
            zoom: f64::from(self.orbit.zoom),
            style: match self.style {
                Style::Shaded => view::Style::Shaded,
                Style::Unlit => view::Style::Unlit,
                Style::Wireframe => view::Style::Wireframe,
            },
        })
    }
}
