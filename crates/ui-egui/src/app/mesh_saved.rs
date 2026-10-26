//! Commit a completed viewer gesture once, including one that moved off screen.
use super::NodeCraftApp;
use serde_json::json;

impl NodeCraftApp {
    /// `force` finishes a pending scroll before a file operation takes its snapshot.
    pub(super) fn commit_mesh_views(&mut self, ctx: &egui::Context, force: bool) -> bool {
        let project = self.session.document().id();
        let (now, down) = ctx.input(|i| (i.time, i.pointer.primary_down()));
        let mut pending = Vec::new();
        for (id, view) in &mut self.mesh_views {
            let Some((owner, canvas)) = view.owner() else { continue };
            if owner != project {
                view.discard();
                continue;
            }
            let Some(node) = self.session.document().canvases().get(&canvas).and_then(|c| c.nodes().get(id)) else {
                view.discard();
                continue;
            };
            // Undo, inspector edits and remote commands win over an old local preview.
            view.bind(project, canvas, node);
            if let Some(state) = view.take_ready(now, down, force) {
                pending.push((*id, canvas, state));
            }
        }
        let mut success = true;
        for (id, canvas, state) in pending {
            if let Err(error) = self.session.dispatch("mesh.view_set", &json!({"canvas":canvas,"id":id,"view":state})) {
                self.notice = Some(error.message);
                success = false;
                if let Some(view) = self.mesh_views.get_mut(&id) {
                    view.discard();
                }
            }
        }
        success
    }
}
