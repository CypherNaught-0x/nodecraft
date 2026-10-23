//! 3D captures from a card (#112): the viewer's Photo and Views buttons add a 3D photo or 3D
//! view sheet card wired to the model, its camera and style taken from the card's current view.
//! The new card is one `mesh.capture`, so one undo step; it runs like any other card.
use super::NodeCraftApp;
use crate::ids;
use crate::mesh_view::Capture;
use nodecraft_engine::mask_cmds;
use nodecraft_gpu::mesh::Style;
use nodecraft_graph::{Endpoint, Node};
use nodecraft_types::{Modality, NodeId};
use serde_json::json;

/// The output a capture takes its model from: the node's one 3D output.
fn mesh_output(node: &Node) -> Option<&str> {
    node.outputs.iter().find(|p| p.port_type.modality == Modality::Mesh && !p.port_type.many).map(|p| p.name.as_str())
}

impl NodeCraftApp {
    /// Adds the cards the viewers' capture buttons asked for this frame.
    pub(super) fn take_mesh_captures(&mut self) {
        let pending: Vec<(NodeId, Capture)> = self.mesh_views.iter_mut().filter_map(|(id, view)| view.capture.take().map(|capture| (*id, capture))).collect();
        for (node, capture) in pending {
            self.capture_mesh(node, capture);
        }
    }

    fn capture_mesh(&mut self, node: NodeId, capture: Capture) {
        let Some(view) = self.mesh_views.get(&node).copied() else {
            return;
        };
        let document = self.session.document();
        let Some(source) = document.canvases().get(&self.canvas).and_then(|c| c.nodes().get(&node)) else {
            return;
        };
        let Some(port) = mesh_output(source) else {
            self.notice = Some("This card has no single 3D model to capture.".into());
            return;
        };
        let revision = self.session.revision();
        let (Some(id), Some(edge)) = (ids::node_id(document, ("mesh-capture", node, revision)), ids::edge_id(document, ("mesh-capture-edge", node, revision)))
        else {
            self.notice = Some("Couldn't allocate node ids; try again.".into());
            return;
        };
        let style = match view.style {
            Style::Shaded => "shaded",
            Style::Unlit => "unlit",
            Style::Wireframe => "wireframe",
        };
        let params = json!({
            "canvas": self.canvas,
            "from": Endpoint { node, port: port.into() },
            "shot": match capture { Capture::Photo => "photo", Capture::Views => "views", Capture::Turntable => "turntable" },
            "id": id,
            "edge": edge,
            "pos": mask_cmds::beside(source.pos, 1.0),
            "yaw": f64::from(view.orbit.yaw.to_degrees()),
            "pitch": f64::from(view.orbit.pitch.to_degrees()),
            "zoom": f64::from(view.orbit.zoom),
            "style": style,
        });
        match self.session.dispatch("mesh.capture", &params) {
            Ok(_) => {
                self.notice = None;
                self.selection.only(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}
