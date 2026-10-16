use nodecraft_types::{CanvasId, CommentId, EdgeId, Error, ErrorKind, FieldId, GenerationId, NodeId, TagId, Value};
use serde::{Deserialize, Serialize};

use crate::{Canvas, Comment, Edge, Field, Generation, Node, Position, Project, Tag, validate};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    SetProjectTitle {
        title: String,
    },
    AddCanvas {
        canvas: Canvas,
    },
    RemoveCanvas {
        id: CanvasId,
    },
    SetCanvasTitle {
        canvas: CanvasId,
        title: String,
    },
    AddNode {
        canvas: CanvasId,
        node: Node,
    },
    /// Undo of node removal, kept as one operation even for a node with many incident edges.
    RestoreNode {
        canvas: CanvasId,
        node: Node,
        edges: Vec<Edge>,
    },
    RemoveNode {
        canvas: CanvasId,
        id: NodeId,
    },
    /// Replace node data (including ports/history), preserving its identity and incident edges.
    SetNode {
        canvas: CanvasId,
        node: Node,
    },
    MoveNode {
        canvas: CanvasId,
        id: NodeId,
        pos: Position,
    },
    SetNodeTitle {
        canvas: CanvasId,
        id: NodeId,
        title: String,
    },
    SetParam {
        canvas: CanvasId,
        id: NodeId,
        name: String,
        value: Option<Value>,
    },
    /// Insert at a stable history index; undo of removal restores the original ordering.
    AddGeneration {
        canvas: CanvasId,
        id: NodeId,
        index: usize,
        generation: Generation,
    },
    /// Clear or switch the active generation before removing it.
    RemoveGeneration {
        canvas: CanvasId,
        id: NodeId,
        generation: GenerationId,
    },
    SetActiveGeneration {
        canvas: CanvasId,
        id: NodeId,
        generation: Option<GenerationId>,
    },
    Connect {
        canvas: CanvasId,
        edge: Edge,
    },
    Disconnect {
        canvas: CanvasId,
        id: EdgeId,
    },
    /// Used for rerouting and input reordering; validation checks the resulting connection.
    SetEdge {
        canvas: CanvasId,
        edge: Edge,
    },
    PutField {
        canvas: CanvasId,
        field: Field,
    },
    RemoveField {
        canvas: CanvasId,
        id: FieldId,
    },
    PutTag {
        canvas: CanvasId,
        tag: Tag,
    },
    RemoveTag {
        canvas: CanvasId,
        id: TagId,
    },
    PutComment {
        canvas: CanvasId,
        comment: Comment,
    },
    RemoveComment {
        canvas: CanvasId,
        id: CommentId,
    },
}

/// An ordered, atomic edit and the serializable unit of undo/autosave/collaboration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Changeset {
    pub changes: Vec<Change>,
}

impl Changeset {
    pub fn new(changes: Vec<Change>) -> Self {
        Self { changes }
    }

    /// Check the same ordered operations as apply without changing the original document.
    pub fn validate(&self, project: &Project) -> Result<(), Error> {
        Self::prepare(&self.changes, project).map(|_| ())
    }

    /// Either commit all changes and return their inverse, or leave the document untouched.
    pub fn apply(&self, project: &mut Project) -> Result<Self, Error> {
        let (updated, inverse) = Self::prepare(&self.changes, project)?;
        *project = updated;
        Ok(inverse)
    }

    fn prepare(changes: &[Change], project: &Project) -> Result<(Project, Self), Error> {
        Self::prepare_with_budget(changes, project, validate::MAX_CHANGESET_BYTES)
    }

    fn prepare_with_budget(changes: &[Change], project: &Project, byte_limit: usize) -> Result<(Project, Self), Error> {
        if changes.len() > validate::MAX_CHANGES {
            return Err(validate::invalid("changeset exceeds operation limit"));
        }
        // Project construction/deserialization and this sole mutation path establish validity.
        // Check arbitrary programmatic payloads before cloning any recursive data.
        for change in changes {
            change.validate_payload()?;
        }
        changes_byte_budget(changes, byte_limit)?;
        let mut draft = project.clone();
        let mut inverse = Vec::new();
        for change in changes {
            inverse.push(change.apply_one(&mut draft)?);
            change.validate_counts(&draft)?;
        }
        // Structural relationships (including DAG/order/tag/field constraints) are checked
        // once, after the complete batch. No intermediate document is published.
        draft.validate()?;
        inverse.reverse();
        let inverse = Self::new(inverse);
        // A successful edit must always return a replayable, bounded undo payload.
        for change in &inverse.changes {
            change.validate_payload()?;
        }
        changes_byte_budget(&inverse.changes, byte_limit)?;
        Ok((draft, inverse))
    }
}

// Undoing AddNode produces RemoveNode; redoing that removal produces RestoreNode. Charge
// the canonical restore form up front so its extra `edges: []` cannot make undo fail at
// the byte boundary. Every other operation's replay shape is stable after inversion.
fn changes_byte_budget(changes: &[Change], limit: usize) -> Result<(), Error> {
    struct BudgetChange<'a>(&'a Change);
    impl Serialize for BudgetChange<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if let Change::AddNode { canvas, node } = self.0 {
                #[derive(Serialize)]
                struct Restore<'a> {
                    r#type: &'static str,
                    canvas: &'a CanvasId,
                    node: &'a Node,
                    edges: &'a [Edge],
                }
                Restore { r#type: "restore_node", canvas, node, edges: &[] }.serialize(serializer)
            } else {
                self.0.serialize(serializer)
            }
        }
    }
    // Only borrowed wrappers are allocated; recursive caller-owned values are never cloned.
    let canonical: Vec<_> = changes.iter().map(BudgetChange).collect();
    validate::byte_budget(&canonical, limit)
}

fn missing() -> Error {
    Error::new(ErrorKind::NotFound, "document entity does not exist")
}
fn duplicate() -> Error {
    Error::new(ErrorKind::Conflict, "document entity already exists")
}
fn canvas_mut(project: &mut Project, id: CanvasId) -> Result<&mut Canvas, Error> {
    project.canvases.get_mut(&id).ok_or_else(missing)
}
fn node_mut(project: &mut Project, canvas: CanvasId, id: NodeId) -> Result<&mut Node, Error> {
    canvas_mut(project, canvas)?.nodes.get_mut(&id).ok_or_else(missing)
}

impl Change {
    pub fn validate(&self, project: &Project) -> Result<(), Error> {
        Changeset::prepare(std::slice::from_ref(self), project).map(|_| ())
    }

    pub fn apply(&self, project: &mut Project) -> Result<Changeset, Error> {
        let (updated, inverse) = Changeset::prepare(std::slice::from_ref(self), project)?;
        *project = updated;
        Ok(inverse)
    }

    fn validate_payload(&self) -> Result<(), Error> {
        match self {
            Self::AddCanvas { canvas } => canvas.validate(),
            Self::AddNode { node, .. } | Self::SetNode { node, .. } => validate::node(node),
            Self::RestoreNode { node, edges, .. } => {
                validate::node(node)?;
                if edges.len() > validate::MAX_EDGES {
                    return Err(validate::invalid("restored node exceeds edge limit"));
                }
                for edge in edges {
                    validate::text(&edge.from.port, 256)?;
                    validate::text(&edge.to.port, 256)?;
                    if edge.from.node != node.id && edge.to.node != node.id {
                        return Err(validate::invalid("restored edge must touch the restored node"));
                    }
                }
                Ok(())
            }
            Self::AddGeneration { generation, .. } => validate::generation(generation),
            Self::MoveNode { pos, .. } => validate::position(*pos),
            Self::SetProjectTitle { title } | Self::SetCanvasTitle { title, .. } | Self::SetNodeTitle { title, .. } => validate::text(title, 4096),
            Self::SetParam { name, value, .. } => {
                validate::text(name, 256)?;
                if name.is_empty() {
                    return Err(validate::invalid("parameter name must not be empty"));
                }
                if let Some(value) = value {
                    validate::value(value)?;
                }
                Ok(())
            }
            Self::Connect { edge, .. } | Self::SetEdge { edge, .. } => {
                validate::text(&edge.from.port, 256)?;
                validate::text(&edge.to.port, 256)
            }
            Self::PutField { field, .. } => validate::text(&field.title, 4096),
            Self::PutTag { tag, .. } => validate::text(&tag.title, 4096),
            Self::PutComment { comment, .. } => {
                validate::position(comment.pos)?;
                validate::text(&comment.text, 1024 * 1024)
            }
            _ => Ok(()),
        }
    }

    fn validate_counts(&self, project: &Project) -> Result<(), Error> {
        if project.canvases.len() > validate::MAX_CANVASES {
            return Err(validate::invalid("project exceeds canvas limit"));
        }
        // O(number of canvases), independent of node/edge/history payload size.
        let mut counts = [0usize; 5];
        for canvas in project.canvases.values() {
            for (count, len) in counts.iter_mut().zip([canvas.nodes.len(), canvas.edges.len(), canvas.fields.len(), canvas.tags.len(), canvas.comments.len()]) {
                *count = count.saturating_add(len);
            }
        }
        for (count, max) in counts.into_iter().zip([validate::MAX_NODES, validate::MAX_EDGES, validate::MAX_NODES, validate::MAX_NODES, validate::MAX_NODES]) {
            if count > max {
                return Err(validate::invalid("project exceeds entity limits"));
            }
        }
        Ok(())
    }

    fn apply_one(&self, project: &mut Project) -> Result<Self, Error> {
        let inverse = match self {
            Self::SetProjectTitle { title } => Self::SetProjectTitle { title: std::mem::replace(&mut project.title, title.clone()) },
            Self::AddCanvas { canvas } => {
                if project.canvases.contains_key(&canvas.id) {
                    return Err(duplicate());
                }
                project.canvases.insert(canvas.id, canvas.clone());
                Self::RemoveCanvas { id: canvas.id }
            }
            Self::RemoveCanvas { id } => Self::AddCanvas { canvas: project.canvases.remove(id).ok_or_else(missing)? },
            Self::SetCanvasTitle { canvas, title } => {
                let target = canvas_mut(project, *canvas)?;
                Self::SetCanvasTitle { canvas: *canvas, title: std::mem::replace(&mut target.title, title.clone()) }
            }
            Self::AddNode { canvas, node } => {
                let target = canvas_mut(project, *canvas)?;
                if target.nodes.contains_key(&node.id) {
                    return Err(duplicate());
                }
                target.nodes.insert(node.id, node.clone());
                Self::RemoveNode { canvas: *canvas, id: node.id }
            }
            Self::RemoveNode { canvas, id } => {
                let target = canvas_mut(project, *canvas)?;
                let node = target.nodes.remove(id).ok_or_else(missing)?;
                let mut edges = Vec::new();
                // Removing a node also removes its incident edges; undo restores both exactly.
                target.edges.retain(|_, edge| {
                    if edge.from.node == *id || edge.to.node == *id {
                        edges.push(edge.clone());
                        false
                    } else {
                        true
                    }
                });
                Self::RestoreNode { canvas: *canvas, node, edges }
            }
            Self::RestoreNode { canvas, node, edges } => {
                let target = canvas_mut(project, *canvas)?;
                if target.nodes.contains_key(&node.id) {
                    return Err(duplicate());
                }
                let mut ids = std::collections::BTreeSet::new();
                for edge in edges {
                    if target.edges.contains_key(&edge.id) || !ids.insert(edge.id) {
                        return Err(duplicate());
                    }
                    for endpoint in [&edge.from, &edge.to] {
                        if endpoint.node != node.id && !target.nodes.contains_key(&endpoint.node) {
                            return Err(missing());
                        }
                    }
                }
                target.nodes.insert(node.id, node.clone());
                for edge in edges {
                    target.edges.insert(edge.id, edge.clone());
                }
                Self::RemoveNode { canvas: *canvas, id: node.id }
            }
            Self::SetNode { canvas, node } => {
                let target = node_mut(project, *canvas, node.id)?;
                Self::SetNode { canvas: *canvas, node: std::mem::replace(target, node.clone()) }
            }
            Self::MoveNode { canvas, id, pos } => {
                let target = node_mut(project, *canvas, *id)?;
                Self::MoveNode { canvas: *canvas, id: *id, pos: std::mem::replace(&mut target.pos, *pos) }
            }
            Self::SetNodeTitle { canvas, id, title } => {
                let target = node_mut(project, *canvas, *id)?;
                Self::SetNodeTitle { canvas: *canvas, id: *id, title: std::mem::replace(&mut target.title, title.clone()) }
            }
            Self::SetParam { canvas, id, name, value } => {
                let target = node_mut(project, *canvas, *id)?;
                let old = match value {
                    Some(value) => target.params.insert(name.clone(), value.clone()),
                    None => target.params.remove(name),
                };
                Self::SetParam { canvas: *canvas, id: *id, name: name.clone(), value: old }
            }
            Self::AddGeneration { canvas, id, index, generation } => {
                let target = node_mut(project, *canvas, *id)?;
                if *index > target.history.len() || target.history.len() >= 1024 {
                    return Err(validate::invalid("generation index or history length exceeds limits"));
                }
                if target.history.iter().any(|entry| entry.id == generation.id) {
                    return Err(duplicate());
                }
                target.history.insert(*index, generation.clone());
                Self::RemoveGeneration { canvas: *canvas, id: *id, generation: generation.id }
            }
            Self::RemoveGeneration { canvas, id, generation } => {
                let target = node_mut(project, *canvas, *id)?;
                if target.active == Some(*generation) {
                    return Err(validate::invalid("switch or clear the active generation before removing it"));
                }
                let index = target.history.iter().position(|entry| entry.id == *generation).ok_or_else(missing)?;
                // The index was found in this unchanged vector, so removal is bounded.
                let generation = target.history.remove(index);
                Self::AddGeneration { canvas: *canvas, id: *id, index, generation }
            }
            Self::SetActiveGeneration { canvas, id, generation } => {
                let target = node_mut(project, *canvas, *id)?;
                if generation.is_some_and(|id| !target.history.iter().any(|entry| entry.id == id)) {
                    return Err(missing());
                }
                Self::SetActiveGeneration { canvas: *canvas, id: *id, generation: std::mem::replace(&mut target.active, *generation) }
            }
            Self::Connect { canvas, edge } => {
                let target = canvas_mut(project, *canvas)?;
                require_endpoints(target, edge)?;
                if target.edges.contains_key(&edge.id) {
                    return Err(duplicate());
                }
                target.edges.insert(edge.id, edge.clone());
                Self::Disconnect { canvas: *canvas, id: edge.id }
            }
            Self::Disconnect { canvas, id } => Self::Connect { canvas: *canvas, edge: canvas_mut(project, *canvas)?.edges.remove(id).ok_or_else(missing)? },
            Self::SetEdge { canvas, edge } => {
                let target = canvas_mut(project, *canvas)?;
                require_endpoints(target, edge)?;
                let target = target.edges.get_mut(&edge.id).ok_or_else(missing)?;
                Self::SetEdge { canvas: *canvas, edge: std::mem::replace(target, edge.clone()) }
            }
            Self::PutField { canvas, field } => match canvas_mut(project, *canvas)?.fields.insert(field.id, field.clone()) {
                Some(field) => Self::PutField { canvas: *canvas, field },
                None => Self::RemoveField { canvas: *canvas, id: field.id },
            },
            Self::RemoveField { canvas, id } => Self::PutField { canvas: *canvas, field: canvas_mut(project, *canvas)?.fields.remove(id).ok_or_else(missing)? },
            Self::PutTag { canvas, tag } => match canvas_mut(project, *canvas)?.tags.insert(tag.id, tag.clone()) {
                Some(tag) => Self::PutTag { canvas: *canvas, tag },
                None => Self::RemoveTag { canvas: *canvas, id: tag.id },
            },
            Self::RemoveTag { canvas, id } => Self::PutTag { canvas: *canvas, tag: canvas_mut(project, *canvas)?.tags.remove(id).ok_or_else(missing)? },
            Self::PutComment { canvas, comment } => match canvas_mut(project, *canvas)?.comments.insert(comment.id, comment.clone()) {
                Some(comment) => Self::PutComment { canvas: *canvas, comment },
                None => Self::RemoveComment { canvas: *canvas, id: comment.id },
            },
            Self::RemoveComment { canvas, id } => {
                Self::PutComment { canvas: *canvas, comment: canvas_mut(project, *canvas)?.comments.remove(id).ok_or_else(missing)? }
            }
        };
        Ok(inverse)
    }
}

// New edges may temporarily violate ordering/type/DAG constraints, but their nodes must
// already exist. This makes node-removal inverses independent of preceding edge operations.
fn require_endpoints(canvas: &Canvas, edge: &Edge) -> Result<(), Error> {
    if canvas.nodes.contains_key(&edge.from.node) && canvas.nodes.contains_key(&edge.to.node) { Ok(()) } else { Err(missing()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::ProjectId;

    #[test]
    fn structural_batch_validates_the_complete_project_once() {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let changes =
            Changeset::new((1u128..=1000).map(|n| Change::AddNode { canvas, node: Node::new(NodeId::from_bytes(n.to_be_bytes()), "test.text") }).collect());
        validate::PROJECT_VALIDATIONS.with(|count| count.set(0));
        let undo = changes.apply(&mut project).unwrap();
        assert_eq!(validate::PROJECT_VALIDATIONS.with(|count| count.get()), 1);
        validate::PROJECT_VALIDATIONS.with(|count| count.set(0));
        undo.apply(&mut project).unwrap();
        assert_eq!(validate::PROJECT_VALIDATIONS.with(|count| count.get()), 1);
    }

    #[test]
    fn a_small_edit_with_an_oversized_inverse_is_rejected_before_commit() {
        let project = Project::new(ProjectId::from_bytes([0; 16]), "x".repeat(4096)).unwrap();
        let before = project.clone();
        let change = Change::SetProjectTitle { title: "small".into() };
        assert!(Changeset::prepare_with_budget(std::slice::from_ref(&change), &project, 1024).is_err());
        assert_eq!(project, before);
        let (mut after, undo) = Changeset::prepare_with_budget(std::slice::from_ref(&change), &project, 8192).unwrap();
        undo.apply(&mut after).unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn node_addition_reserves_the_larger_restore_form_at_the_byte_boundary() {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(NodeId::from_bytes([2; 16]), "test.text");
        node.title = "\"\n☃".repeat(20);
        let add = Change::AddNode { canvas, node: node.clone() };
        let restore = Change::RestoreNode { canvas, node, edges: vec![] };
        let add_size = serde_json::to_vec(&[&add]).unwrap().len();
        let replay_size = serde_json::to_vec(&[restore]).unwrap().len();
        assert!(replay_size > add_size);
        assert!(Changeset::prepare_with_budget(std::slice::from_ref(&add), &project, add_size).is_err());
        assert!(Changeset::prepare_with_budget(std::slice::from_ref(&add), &project, replay_size - 1).is_err());
        let (after, undo) = Changeset::prepare_with_budget(&[add], &project, replay_size).unwrap();
        let (restored, redo) = Changeset::prepare_with_budget(&undo.changes, &after, replay_size).unwrap();
        assert_eq!(restored, project);
        let (redone, _) = Changeset::prepare_with_budget(&redo.changes, &restored, replay_size).unwrap();
        assert_eq!(redone, after);
    }
}
