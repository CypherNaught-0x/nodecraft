use nodecraft_types::{CanvasId, CommentId, EdgeId, Error, ErrorKind, FieldId, NodeId, TagId, Value};
use serde::{Deserialize, Serialize};

use crate::{Canvas, Comment, Edge, Field, Node, Position, Project, Tag, validate};

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
        self.prepare(project).map(|_| ())
    }

    /// Either commit all changes and return their inverse, or leave the document untouched.
    pub fn apply(&self, project: &mut Project) -> Result<Self, Error> {
        let (updated, inverse) = self.prepare(project)?;
        *project = updated;
        Ok(inverse)
    }

    fn prepare(&self, project: &Project) -> Result<(Project, Self), Error> {
        if self.changes.len() > validate::MAX_CHANGES {
            return Err(validate::invalid("changeset exceeds operation limit"));
        }
        // Check arbitrary programmatic payloads before cloning any recursive data.
        project.validate()?;
        for change in &self.changes {
            change.validate_payload()?;
        }
        let mut draft = project.clone();
        let mut inverse = Vec::new();
        for change in &self.changes {
            let undo = change.apply_one(&mut draft)?;
            // Preserve step validity without rescanning the graph for scalar edits or deletion.
            change.validate_effect(&draft)?;
            inverse.push(undo);
        }
        draft.validate()?;
        inverse.reverse();
        Ok((draft, Self::new(inverse)))
    }
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
        self.validate_payload()?;
        Changeset::new(vec![self.clone()]).validate(project)
    }

    pub fn apply(&self, project: &mut Project) -> Result<Changeset, Error> {
        self.validate_payload()?;
        Changeset::new(vec![self.clone()]).apply(project)
    }

    fn validate_payload(&self) -> Result<(), Error> {
        match self {
            Self::AddCanvas { canvas } => canvas.validate(),
            Self::AddNode { node, .. } | Self::SetNode { node, .. } => validate::node(node),
            Self::RestoreNode { node, edges, .. } => {
                validate::node(node)?;
                if edges.len() > 50_000 {
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

    fn validate_effect(&self, project: &Project) -> Result<(), Error> {
        match self {
            // Payload checks already validate the new scalar. Deletions remove all incident
            // edges, so they cannot introduce a cycle, duplicate, or dangling reference.
            Self::SetProjectTitle { .. }
            | Self::SetCanvasTitle { .. }
            | Self::SetNodeTitle { .. }
            | Self::MoveNode { .. }
            | Self::Disconnect { .. }
            | Self::RemoveNode { .. }
            | Self::RemoveCanvas { .. } => Ok(()),
            Self::SetParam { canvas, id, .. } => {
                let node = project.canvases.get(canvas).and_then(|canvas| canvas.nodes.get(id)).ok_or_else(missing)?;
                if node.params.len() > 256 { Err(validate::invalid("node exceeds parameter limit")) } else { Ok(()) }
            }
            _ => project.validate(),
        }
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
                target.nodes.insert(node.id, node.clone());
                for edge in edges {
                    if target.edges.contains_key(&edge.id) {
                        return Err(duplicate());
                    }
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
            Self::Connect { canvas, edge } => {
                let target = canvas_mut(project, *canvas)?;
                if target.edges.contains_key(&edge.id) {
                    return Err(duplicate());
                }
                target.edges.insert(edge.id, edge.clone());
                Self::Disconnect { canvas: *canvas, id: edge.id }
            }
            Self::Disconnect { canvas, id } => Self::Connect { canvas: *canvas, edge: canvas_mut(project, *canvas)?.edges.remove(id).ok_or_else(missing)? },
            Self::SetEdge { canvas, edge } => {
                let target = canvas_mut(project, *canvas)?.edges.get_mut(&edge.id).ok_or_else(missing)?;
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
