//! Edits another peer made, taken in from a shared document (#119).
use super::{Direction, Session, guard};
use crate::commands::Effect;
use nodecraft_graph::Changeset;
use nodecraft_types::{Error, ErrorKind};
use serde_json::Value;

impl Session {
    /// Applies `changes`, which came from another peer, through the one mutation path. The edit
    /// joins no undo step: undo and redo stay this peer's own, and an undo after it puts back
    /// what this peer had for each part it touches. Every asset the changes name must be in the
    /// session first (`import_asset`).
    pub fn apply_remote(&mut self, changes: Changeset) -> Result<(), Error> {
        self.settle_for_edit()?;
        let before = self.file_status();
        let result = guard(|| {
            if nodecraft_format::assets_in_changes(&changes)?.iter().any(|hash| !self.assets.contains(hash)) {
                return Err(Error::new(ErrorKind::NotFound, "a shared card names an asset that hasn't arrived"));
            }
            let mut prepared = self.prepare_remote(Effect::Edit { changes, result: Value::Null, gesture: None })?;
            prepared.direction = Direction::Remote;
            self.commit_prepared(prepared, "sync.remote").map(|_| ())
        });
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Node, Project};
    use nodecraft_types::{CanvasId, NodeId, ProjectId};
    use serde_json::json;

    fn session() -> Session {
        let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Shared").unwrap();
        Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([2; 16]), "Main") }]).apply(&mut project).unwrap();
        Session::new(project).unwrap()
    }

    fn add(n: u8) -> Changeset {
        Changeset::new(vec![Change::AddNode { canvas: CanvasId::from_bytes([2; 16]), node: Node::new(NodeId::from_bytes([n; 16]), "text") }])
    }

    #[test]
    fn a_remote_edit_changes_the_document_but_not_the_history() {
        let mut session = session();
        session.apply_remote(add(1)).unwrap();
        assert_eq!(session.revision(), 1);
        assert!(!session.can_undo());
        assert_eq!(session.events_since(0).unwrap().last().map(|e| e.command), Some("sync.remote"));
        // A local edit after it undoes on its own, leaving the remote card.
        let canvas = CanvasId::from_bytes([2; 16]);
        session.dispatch("node.add", &json!({"canvas": canvas, "node": Node::new(NodeId::from_bytes([3; 16]), "text")})).unwrap();
        session.dispatch("edit.undo", &json!({})).unwrap();
        assert!(session.document().canvases()[&canvas].nodes().contains_key(&NodeId::from_bytes([1; 16])));
        assert!(!session.can_undo());
    }

    #[test]
    fn a_remote_edit_that_doesnt_fit_changes_nothing() {
        let mut session = session();
        session.apply_remote(add(1)).unwrap();
        assert!(session.apply_remote(add(1)).is_err());
        assert!(session.apply_remote(Changeset::new(Vec::new())).is_err());
        assert_eq!(session.revision(), 1);
    }
}
