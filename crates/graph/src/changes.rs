use nodecraft_types::{CanvasId, CommentId, EdgeId, Error, ErrorKind, FieldId, GenerationId, NodeId, SkillId, TagId, Value};
use serde::{Deserialize, Serialize};

use crate::{Canvas, Comment, CommentPin, CommentReply, Edge, Field, Generation, Node, Position, Project, Skill, Tag, validate};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    /// Compact membership-only edits; relationships are validated after the entire batch.
    SetNodeFields {
        canvas: CanvasId,
        assignments: Vec<NodeFieldAssignment>,
    },
    /// Absolute positions keep inverse/redo exact and a 10k-member move compact.
    MoveNodes {
        canvas: CanvasId,
        positions: Vec<NodePosition>,
    },
    /// Compact, independently bounded tag assignments; never copies node payloads.
    SetNodeTags {
        canvas: CanvasId,
        assignments: Vec<NodeTagAssignment>,
    },
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
    /// Keeps a canvas out of a project shared live, or shares it again (#121).
    SetCanvasPrivate {
        canvas: CanvasId,
        private: bool,
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
        #[serde(default)]
        pins: Vec<CommentPin>,
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
    SetCommentText {
        canvas: CanvasId,
        id: CommentId,
        text: String,
    },
    SetCommentPin {
        canvas: CanvasId,
        id: CommentId,
        node: Option<NodeId>,
        pos: Position,
    },
    SetCommentResolved {
        canvas: CanvasId,
        id: CommentId,
        resolved: bool,
    },
    InsertCommentReply {
        canvas: CanvasId,
        id: CommentId,
        index: usize,
        reply: CommentReply,
    },
    RemoveCommentReply {
        canvas: CanvasId,
        id: CommentId,
        reply: CommentId,
    },
    SetCommentReplyText {
        canvas: CanvasId,
        id: CommentId,
        reply: CommentId,
        text: String,
    },
    RemoveComment {
        canvas: CanvasId,
        id: CommentId,
    },
    /// Adds a project skill, or replaces the one with its id (#168).
    PutSkill {
        skill: Skill,
    },
    RemoveSkill {
        id: SkillId,
    },
}

/// A tag-only node edit, also used to retain each previous value in the inverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeTagAssignment {
    pub node: NodeId,
    #[serde(deserialize_with = "required_membership")]
    pub tag: Option<TagId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeFieldAssignment {
    pub node: NodeId,
    #[serde(deserialize_with = "required_membership")]
    pub field: Option<FieldId>,
}
// An absent key must not silently clear membership. Explicit JSON null still clears it.
fn required_membership<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodePosition {
    pub node: NodeId,
    pub pos: Position,
}

/// An ordered, atomic edit and the serializable unit of undo/autosave/collaboration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Changeset {
    pub changes: Vec<Change>,
}

/// Prepared atomic document and certified replay payloads. No source document is mutated.
pub struct StagedChanges {
    pub document: Project,
    pub inverse: Changeset,
    pub actual_redo: Option<Changeset>,
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

    /// Stage one validated transaction without mutating the source project. The returned draft
    /// and inverse can be published together by the owning command session.
    pub fn stage(&self, project: &Project) -> Result<(Project, Self), Error> {
        Self::prepare(&self.changes, project)
    }

    /// Validate programmatic payloads before serialization, including the canonical replay size.
    /// References and ordered transaction relationships require `validate`, `stage` or `apply`.
    pub fn validate_payload(&self) -> Result<(), Error> {
        self.validate_replay_budget(validate::MAX_CHANGESET_BYTES)
    }

    /// Apply a stricter caller budget while retaining the graph's canonical replay accounting.
    pub fn validate_replay_budget(&self, limit: usize) -> Result<(), Error> {
        validate_changes_payload(&self.changes, limit.min(validate::MAX_CHANGESET_BYTES))
    }

    fn prepare(changes: &[Change], project: &Project) -> Result<(Project, Self), Error> {
        Self::prepare_with_budget(changes, project, validate::MAX_CHANGESET_BYTES)
    }

    fn prepare_with_budget(changes: &[Change], project: &Project, byte_limit: usize) -> Result<(Project, Self), Error> {
        let staged = Self::prepare_certified(changes, project, byte_limit)?;
        Ok((staged.document, staged.inverse))
    }

    pub fn stage_certified(&self, project: &Project) -> Result<StagedChanges, Error> {
        Self::prepare_certified(&self.changes, project, validate::MAX_CHANGESET_BYTES)
    }

    fn prepare_certified(changes: &[Change], project: &Project, limit: usize) -> Result<StagedChanges, Error> {
        validate_changes_payload(changes, limit)?;
        let certify_pins = needs_pin_certification(changes, project);
        let mut draft = project.clone();
        let inverse = stage_once(changes, &mut draft, limit)?;
        let actual_redo = if certify_pins {
            #[cfg(test)]
            PROBE_CLONES.with(|count| count.set(count.get() + 1));
            let mut probe = draft.clone();
            let redo = stage_once(&inverse.changes, &mut probe, limit)?;
            if probe != *project {
                return Err(validate::invalid("structural inverse does not restore the document"));
            }
            let inverse_again = stage_once(&redo.changes, &mut probe, limit)?;
            if probe != draft || inverse_again != inverse {
                return Err(validate::invalid("structural replay is not stable"));
            }
            Some(redo)
        } else {
            None
        };
        Ok(StagedChanges { document: draft, inverse, actual_redo })
    }
}

/// Only node lifetime changes can grow the canonical replay's pin metadata. Include
/// original and incoming relationships, even ones repaired later in a valid-final batch.
/// MoveNode and SetNode still receive normal final world-position validation in stage_once.
fn needs_pin_certification(changes: &[Change], project: &Project) -> bool {
    let mut nodes = std::collections::BTreeSet::new();
    for change in changes {
        match change {
            Change::AddNode { canvas, node } | Change::RestoreNode { canvas, node, .. } => {
                nodes.insert((*canvas, node.id));
            }
            Change::RemoveNode { canvas, id } => {
                nodes.insert((*canvas, *id));
            }
            _ => {}
        }
    }
    if nodes.is_empty() {
        return false;
    }
    let touches = |canvas, node: Option<NodeId>| node.is_some_and(|node| nodes.contains(&(canvas, node)));
    project.canvases.values().any(|canvas| canvas.comments.values().any(|root| touches(canvas.id, root.node)))
        || changes.iter().any(|change| match change {
            Change::PutComment { canvas, comment } => touches(*canvas, comment.node),
            Change::SetCommentPin { canvas, node, .. } => touches(*canvas, *node),
            Change::AddCanvas { canvas } => canvas.comments.values().any(|root| touches(canvas.id, root.node)),
            // Restoration may replace a root's former anchor, so its nonempty pin
            // metadata requires certification even without an incoming comment change.
            Change::RestoreNode { pins, .. } => !pins.is_empty(),
            _ => false,
        })
}

#[cfg(test)]
thread_local! {
    static STAGE_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PROBE_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static ANCHOR_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn stage_once(changes: &[Change], draft: &mut Project, byte_limit: usize) -> Result<Changeset, Error> {
    // Project construction/deserialization and this sole mutation path establish validity.
    // Check arbitrary programmatic payloads before cloning any recursive data.
    validate_changes_payload(changes, byte_limit)?;
    #[cfg(test)]
    STAGE_PASSES.with(|count| count.set(count.get() + 1));
    let mut anchors = Anchors::new(draft);
    let mut inverse = Vec::new();
    let mut budget = byte_limit.checked_sub(2).ok_or_else(|| validate::invalid("changeset byte limit"))?;
    let mut pins = 0usize;
    for change in changes {
        preflight_inverse(change, draft, &anchors, budget, validate::MAX_CHANGES.saturating_sub(pins))?;
        let item = change.apply_one(draft, &mut anchors)?;
        item.validate_payload()?;
        pins = pins.saturating_add(pin_count(&item));
        if pins > validate::MAX_CHANGES {
            return Err(validate::invalid("aggregate restored pin metadata exceeds limit"));
        }
        let bytes = validate::encoded_bytes(&BudgetChange(&item), budget)?;
        budget = budget.checked_sub(bytes.saturating_add(usize::from(!inverse.is_empty()))).ok_or_else(|| validate::invalid("changeset byte limit"))?;
        inverse.push(item);
        change.validate_counts(draft)?;
        if anchors.messages > validate::MAX_NODES {
            return Err(validate::invalid("project exceeds message limit"));
        }
    }
    // Structural relationships (including DAG/order/tag/field constraints) are checked
    // once, after the complete batch. No intermediate document is published.
    draft.validate()?;
    inverse.reverse();
    let inverse = Changeset::new(inverse);
    // A successful edit must always return a replayable, bounded undo payload.
    validate_changes_payload(&inverse.changes, byte_limit)?;
    Ok(inverse)
}

fn validate_changes_payload(changes: &[Change], limit: usize) -> Result<(), Error> {
    if changes.len() > validate::MAX_CHANGES {
        return Err(validate::invalid("changeset exceeds operation limit"));
    }
    let mut assignments = 0usize;
    let mut pins = 0usize;
    // Charge aggregate work before building any per-operation ID set or copying payloads.
    for change in changes {
        assignments = assignments.saturating_add(match change {
            Change::SetNodeTags { assignments, .. } => assignments.len(),
            Change::SetNodeFields { assignments, .. } => assignments.len(),
            Change::MoveNodes { positions, .. } => positions.len(),
            _ => 0,
        });
        pins = pins.saturating_add(pin_count(change));
        if assignments > validate::MAX_NODES {
            return Err(validate::invalid("changeset exceeds aggregate bulk node edit limit"));
        }
        if pins > validate::MAX_CHANGES {
            return Err(validate::invalid("aggregate restored pin metadata exceeds limit"));
        }
    }
    for change in changes {
        change.validate_payload()?;
    }
    changes_byte_budget(changes, limit)
}

// Undoing AddNode produces RemoveNode; redoing that removal produces RestoreNode. Charge
// the canonical restore form up front so its extra `edges: []` cannot make undo fail at
// the byte boundary. Every other operation's replay shape is stable after inversion.
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
                pins: &'a [CommentPin],
            }
            Restore { r#type: "restore_node", canvas, node, edges: &[], pins: &[] }.serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}
fn changes_byte_budget(changes: &[Change], limit: usize) -> Result<(), Error> {
    // Only borrowed wrappers are allocated; recursive caller-owned values are never cloned.
    let canonical: Vec<_> = changes.iter().map(BudgetChange).collect();
    validate::byte_budget(&canonical, limit)
}

fn validate_bulk_ids(ids: impl Iterator<Item = NodeId>, count: usize) -> Result<(), Error> {
    if count > validate::MAX_NODES {
        return Err(validate::invalid("bulk edit exceeds node limit"));
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in ids {
        if !seen.insert(id) {
            return Err(duplicate());
        }
    }
    Ok(())
}

fn missing() -> Error {
    Error::new(ErrorKind::NotFound, "document entity does not exist")
}
fn duplicate() -> Error {
    Error::new(ErrorKind::Conflict, "document entity already exists")
}
/// Every change to a canvas's contents comes through here, so a published technique version is
/// refused here; adding or removing a whole version doesn't, and stays undoable.
fn canvas_mut(project: &mut Project, id: CanvasId) -> Result<&mut Canvas, Error> {
    let canvas = project.canvases.get_mut(&id).ok_or_else(missing)?;
    if canvas.kind == crate::CanvasKind::Technique {
        return Err(Error::new(ErrorKind::Conflict, "a published technique version can't be edited; publish a new version instead"));
    }
    Ok(canvas)
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
            Self::RestoreNode { node, edges, pins, .. } => {
                validate_pins(pins)?;
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
            Self::PutField { field, .. } => field.validate(),
            Self::PutTag { tag, .. } => tag.validate(),
            Self::PutSkill { skill } => skill.validate(),
            Self::SetNodeFields { assignments, .. } => validate_bulk_ids(assignments.iter().map(|a| a.node), assignments.len()),
            Self::MoveNodes { positions, .. } => {
                validate_bulk_ids(positions.iter().map(|a| a.node), positions.len())?;
                for position in positions {
                    validate::position(position.pos)?;
                }
                Ok(())
            }
            Self::SetNodeTags { assignments, .. } => validate_bulk_ids(assignments.iter().map(|a| a.node), assignments.len()),
            Self::PutComment { comment, .. } => validate::comment(comment),
            Self::SetCommentText { text, .. } => validate::text(text, validate::MAX_COMMENT_BYTES),
            Self::SetCommentReplyText { text, .. } => validate::text(text, validate::MAX_REPLY_BYTES),
            Self::InsertCommentReply { index, reply, .. } => {
                if *index > validate::MAX_COMMENT_REPLIES {
                    return Err(validate::invalid("reply index exceeds limit"));
                }
                validate::text(&reply.text, validate::MAX_REPLY_BYTES)?;
                validate::author(reply.author.as_deref())
            }
            Self::SetCommentPin { pos, .. } => validate::position(*pos),
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

    fn apply_one(&self, project: &mut Project, anchors: &mut Anchors) -> Result<Self, Error> {
        let inverse = match self {
            Self::SetNodeFields { canvas, assignments } => {
                let target = canvas_mut(project, *canvas)?;
                let mut inverse = Vec::with_capacity(assignments.len());
                for assignment in assignments {
                    let node = target.nodes.get_mut(&assignment.node).ok_or_else(missing)?;
                    inverse.push(NodeFieldAssignment { node: assignment.node, field: std::mem::replace(&mut node.field, assignment.field) });
                }
                Self::SetNodeFields { canvas: *canvas, assignments: inverse }
            }
            Self::MoveNodes { canvas, positions } => {
                let target = canvas_mut(project, *canvas)?;
                let mut inverse = Vec::with_capacity(positions.len());
                for position in positions {
                    let node = target.nodes.get_mut(&position.node).ok_or_else(missing)?;
                    inverse.push(NodePosition { node: position.node, pos: std::mem::replace(&mut node.pos, position.pos) });
                }
                Self::MoveNodes { canvas: *canvas, positions: inverse }
            }
            Self::SetNodeTags { canvas, assignments } => {
                let target = canvas_mut(project, *canvas)?;
                let mut inverse = Vec::with_capacity(assignments.len());
                for assignment in assignments {
                    let node = target.nodes.get_mut(&assignment.node).ok_or_else(missing)?;
                    inverse.push(NodeTagAssignment { node: assignment.node, tag: std::mem::replace(&mut node.tag, assignment.tag) });
                }
                Self::SetNodeTags { canvas: *canvas, assignments: inverse }
            }
            Self::SetProjectTitle { title } => Self::SetProjectTitle { title: std::mem::replace(&mut project.title, title.clone()) },
            Self::AddCanvas { canvas } => {
                if project.canvases.contains_key(&canvas.id) {
                    return Err(duplicate());
                }
                anchors.add_canvas(canvas);
                project.canvases.insert(canvas.id, canvas.clone());
                Self::RemoveCanvas { id: canvas.id }
            }
            Self::RemoveCanvas { id } => {
                let canvas = project.canvases.remove(id).ok_or_else(missing)?;
                anchors.remove_canvas(&canvas);
                Self::AddCanvas { canvas }
            }
            Self::SetCanvasTitle { canvas, title } => {
                let target = canvas_mut(project, *canvas)?;
                Self::SetCanvasTitle { canvas: *canvas, title: std::mem::replace(&mut target.title, title.clone()) }
            }
            Self::SetCanvasPrivate { canvas, private } => {
                let target = canvas_mut(project, *canvas)?;
                Self::SetCanvasPrivate { canvas: *canvas, private: std::mem::replace(&mut target.private, *private) }
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
                let mut pins = Vec::new();
                if let Some(ids) = anchors.pins.remove(&(*canvas, *id)) {
                    if ids.len() > validate::MAX_CHANGES {
                        return Err(validate::invalid("restored pin metadata exceeds limit"));
                    }
                    for comment_id in ids {
                        let root = target.comments.get(&comment_id).ok_or_else(missing)?;
                        let world = root.world_position(target)?;
                        let pin = CommentPin { id: comment_id, offset: root.pos };
                        let root = target.comments.get_mut(&comment_id).ok_or_else(missing)?;
                        root.node = None;
                        root.pos = world;
                        pins.push(pin);
                    }
                }
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
                Self::RestoreNode { canvas: *canvas, node, edges, pins }
            }
            Self::RestoreNode { canvas, node, edges, pins } => {
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
                for pin in pins {
                    let root = target.comments.get(&pin.id).ok_or_else(missing)?;
                    anchors.unpin(*canvas, root);
                }
                target.nodes.insert(node.id, node.clone());
                for pin in pins {
                    let root = target.comments.get_mut(&pin.id).ok_or_else(missing)?;
                    root.node = Some(node.id);
                    root.pos = pin.offset;
                    anchors.pin(*canvas, root);
                }
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
                if *index > target.history.len() || target.history.len() >= validate::MAX_HISTORY {
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
            Self::PutSkill { skill } => match project.skills.insert(skill.id, skill.clone()) {
                Some(skill) => Self::PutSkill { skill },
                None => Self::RemoveSkill { id: skill.id },
            },
            Self::RemoveSkill { id } => Self::PutSkill { skill: project.skills.remove(id).ok_or_else(missing)? },
            Self::PutComment { canvas, comment } => {
                let target = canvas_mut(project, *canvas)?;
                if let Some(old) = target.comments.get(&comment.id) {
                    anchors.unpin(*canvas, old);
                    anchors.messages = anchors.messages.saturating_sub(1 + old.replies.len());
                }
                anchors.messages = anchors.messages.saturating_add(1 + comment.replies.len());
                if anchors.messages > validate::MAX_NODES {
                    return Err(validate::invalid("project exceeds message limit"));
                }
                anchors.pin(*canvas, comment);
                match target.comments.insert(comment.id, comment.clone()) {
                    Some(comment) => Self::PutComment { canvas: *canvas, comment },
                    None => Self::RemoveComment { canvas: *canvas, id: comment.id },
                }
            }
            Self::RemoveComment { canvas, id } => {
                let comment = canvas_mut(project, *canvas)?.comments.remove(id).ok_or_else(missing)?;
                anchors.unpin(*canvas, &comment);
                anchors.messages = anchors.messages.saturating_sub(1 + comment.replies.len());
                Self::PutComment { canvas: *canvas, comment }
            }
            Self::SetCommentText { canvas, id, text } => {
                let root = comment_mut(project, *canvas, *id)?;
                Self::SetCommentText { canvas: *canvas, id: *id, text: std::mem::replace(&mut root.text, text.clone()) }
            }
            Self::SetCommentPin { canvas, id, node, pos } => {
                let root = comment_mut(project, *canvas, *id)?;
                anchors.unpin(*canvas, root);
                let inverse = Self::SetCommentPin { canvas: *canvas, id: *id, node: root.node, pos: root.pos };
                root.node = *node;
                root.pos = *pos;
                anchors.pin(*canvas, root);
                inverse
            }
            Self::SetCommentResolved { canvas, id, resolved } => {
                let root = comment_mut(project, *canvas, *id)?;
                Self::SetCommentResolved { canvas: *canvas, id: *id, resolved: std::mem::replace(&mut root.resolved, *resolved) }
            }
            Self::InsertCommentReply { canvas, id, index, reply } => {
                let root = comment_mut(project, *canvas, *id)?;
                if *index > root.replies.len() || root.replies.len() >= validate::MAX_COMMENT_REPLIES {
                    return Err(validate::invalid("reply index or count exceeds limit"));
                }
                if root.id == reply.id || root.replies.iter().any(|item| item.id == reply.id) {
                    return Err(duplicate());
                }
                anchors.messages = anchors.messages.saturating_add(1);
                if anchors.messages > validate::MAX_NODES {
                    return Err(validate::invalid("project exceeds message limit"));
                }
                root.replies.insert(*index, reply.clone());
                Self::RemoveCommentReply { canvas: *canvas, id: *id, reply: reply.id }
            }
            Self::RemoveCommentReply { canvas, id, reply } => {
                let root = comment_mut(project, *canvas, *id)?;
                let index = root.replies.iter().position(|item| item.id == *reply).ok_or_else(missing)?;
                let reply = root.replies.remove(index);
                anchors.messages = anchors.messages.saturating_sub(1);
                Self::InsertCommentReply { canvas: *canvas, id: *id, index, reply }
            }
            Self::SetCommentReplyText { canvas, id, reply, text } => {
                let root = comment_mut(project, *canvas, *id)?;
                let item = root.replies.iter_mut().find(|item| item.id == *reply).ok_or_else(missing)?;
                Self::SetCommentReplyText { canvas: *canvas, id: *id, reply: *reply, text: std::mem::replace(&mut item.text, text.clone()) }
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

fn comment_mut(project: &mut Project, canvas: CanvasId, id: CommentId) -> Result<&mut Comment, Error> {
    canvas_mut(project, canvas)?.comments.get_mut(&id).ok_or_else(missing)
}

fn pin_count(change: &Change) -> usize {
    match change {
        Change::RestoreNode { pins, .. } => pins.len(),
        _ => 0,
    }
}

fn validate_pins(pins: &[CommentPin]) -> Result<(), Error> {
    if pins.len() > validate::MAX_CHANGES {
        return Err(validate::invalid("restored pin metadata exceeds limit"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for pin in pins {
        validate::position(pin.offset)?;
        if !ids.insert(pin.id) {
            return Err(duplicate());
        }
    }
    Ok(())
}

/// One bounded anchor index per stage; node removal never scans all discussions repeatedly.
struct Anchors {
    pins: std::collections::BTreeMap<(CanvasId, NodeId), std::collections::BTreeSet<CommentId>>,
    messages: usize,
}
impl Anchors {
    fn new(project: &Project) -> Self {
        let mut index = Self { pins: Default::default(), messages: 0 };
        for canvas in project.canvases.values() {
            index.add_canvas(canvas);
        }
        index
    }
    fn add_canvas(&mut self, canvas: &Canvas) {
        for root in canvas.comments.values() {
            #[cfg(test)]
            ANCHOR_VISITS.with(|count| count.set(count.get() + 1));
            self.messages = self.messages.saturating_add(1 + root.replies.len());
            self.pin(canvas.id, root);
        }
    }
    fn remove_canvas(&mut self, canvas: &Canvas) {
        for root in canvas.comments.values() {
            self.messages = self.messages.saturating_sub(1 + root.replies.len());
            self.unpin(canvas.id, root);
        }
    }
    fn pin(&mut self, canvas: CanvasId, root: &Comment) {
        if let Some(node) = root.node {
            self.pins.entry((canvas, node)).or_default().insert(root.id);
        }
    }
    fn unpin(&mut self, canvas: CanvasId, root: &Comment) {
        if let Some(node) = root.node {
            let key = (canvas, node);
            if let Some(ids) = self.pins.get_mut(&key) {
                ids.remove(&root.id);
                if ids.is_empty() {
                    self.pins.remove(&key);
                }
            }
        }
    }
}

/// Borrow large inverse bodies before copying them. Small inverse variants contain only bounded
/// IDs/positions/ports/titles; every completed variant is also incrementally charged by stage_once.
fn preflight_inverse(change: &Change, project: &Project, anchors: &Anchors, budget: usize, pin_budget: usize) -> Result<(), Error> {
    #[derive(Serialize)]
    #[serde(tag = "type", rename_all = "snake_case")]
    enum Borrowed<'a> {
        AddCanvas { canvas: &'a Canvas },
        SetNode { canvas: CanvasId, node: &'a Node },
        RestoreNode { canvas: CanvasId, node: &'a Node, edges: Vec<&'a Edge>, pins: Vec<CommentPin> },
        SetParam { canvas: CanvasId, id: NodeId, name: &'a str, value: Option<&'a Value> },
        AddGeneration { canvas: CanvasId, id: NodeId, index: usize, generation: &'a Generation },
        PutComment { canvas: CanvasId, comment: &'a Comment },
        SetCommentText { canvas: CanvasId, id: CommentId, text: &'a str },
        SetCommentReplyText { canvas: CanvasId, id: CommentId, reply: CommentId, text: &'a str },
        InsertCommentReply { canvas: CanvasId, id: CommentId, index: usize, reply: &'a CommentReply },
        PutSkill { skill: &'a Skill },
    }
    let canvas = |id| project.canvases.get(&id).ok_or_else(missing);
    let node = |c, id| canvas(c)?.nodes.get(&id).ok_or_else(missing);
    let comment = |c, id| canvas(c)?.comments.get(&id).ok_or_else(missing);
    let inverse = match change {
        Change::RemoveCanvas { id } => Some(Borrowed::AddCanvas { canvas: canvas(*id)? }),
        Change::SetNode { canvas, node: incoming } => Some(Borrowed::SetNode { canvas: *canvas, node: node(*canvas, incoming.id)? }),
        Change::RemoveNode { canvas: c, id } => {
            let target = canvas(*c)?;
            let ids = anchors.pins.get(&(*c, *id));
            if ids.is_some_and(|ids| ids.len() > pin_budget) {
                return Err(validate::invalid("aggregate restored pin metadata exceeds limit"));
            }
            let mut pins = Vec::new();
            if let Some(ids) = ids {
                for id in ids {
                    let root = target.comments.get(id).ok_or_else(missing)?;
                    pins.push(CommentPin { id: *id, offset: root.pos });
                }
            }
            Some(Borrowed::RestoreNode {
                canvas: *c,
                node: node(*c, *id)?,
                edges: target.edges.values().filter(|edge| edge.from.node == *id || edge.to.node == *id).collect(),
                pins,
            })
        }
        Change::SetParam { canvas, id, name, .. } => Some(Borrowed::SetParam { canvas: *canvas, id: *id, name, value: node(*canvas, *id)?.params.get(name) }),
        Change::RemoveGeneration { canvas, id, generation } => {
            let target = node(*canvas, *id)?;
            let index = target.history.iter().position(|item| item.id == *generation).ok_or_else(missing)?;
            Some(Borrowed::AddGeneration { canvas: *canvas, id: *id, index, generation: target.history.get(index).ok_or_else(missing)? })
        }
        Change::RemoveComment { canvas, id } => Some(Borrowed::PutComment { canvas: *canvas, comment: comment(*canvas, *id)? }),
        Change::PutComment { canvas, comment: incoming } => {
            project.canvases.get(canvas).and_then(|c| c.comments.get(&incoming.id)).map(|comment| Borrowed::PutComment { canvas: *canvas, comment })
        }
        Change::SetCommentText { canvas, id, .. } => Some(Borrowed::SetCommentText { canvas: *canvas, id: *id, text: &comment(*canvas, *id)?.text }),
        Change::SetCommentReplyText { canvas, id, reply, .. } => {
            let item = comment(*canvas, *id)?.replies.iter().find(|item| item.id == *reply).ok_or_else(missing)?;
            Some(Borrowed::SetCommentReplyText { canvas: *canvas, id: *id, reply: *reply, text: &item.text })
        }
        Change::RemoveCommentReply { canvas, id, reply } => {
            let root = comment(*canvas, *id)?;
            let index = root.replies.iter().position(|item| item.id == *reply).ok_or_else(missing)?;
            Some(Borrowed::InsertCommentReply { canvas: *canvas, id: *id, index, reply: root.replies.get(index).ok_or_else(missing)? })
        }
        Change::RemoveSkill { id } => Some(Borrowed::PutSkill { skill: project.skills.get(id).ok_or_else(missing)? }),
        Change::PutSkill { skill: incoming } => project.skills.get(&incoming.id).map(|skill| Borrowed::PutSkill { skill }),
        _ => None,
    };
    if let Some(inverse) = inverse {
        validate::encoded_bytes(&inverse, budget)?;
    }
    Ok(())
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
    fn structural_pin_preflight_has_three_stages_one_extra_clone_and_linear_anchor_visits() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Stress").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut changes = Vec::new();
        for n in 1u128..=1000 {
            let node = Node::new(NodeId::from_bytes(n.to_be_bytes()), "source.text");
            let mut root = Comment::new(CommentId::from_bytes(n.to_be_bytes()), "Thread", Position { x: 1.0, y: 2.0 });
            root.node = Some(node.id);
            changes.push(Change::AddNode { canvas, node });
            changes.push(Change::PutComment { canvas, comment: root });
        }
        Changeset::new(changes).apply(&mut project).unwrap();
        let reset = || {
            STAGE_PASSES.with(|c| c.set(0));
            PROBE_CLONES.with(|c| c.set(0));
            ANCHOR_VISITS.with(|c| c.set(0));
        };
        reset();
        let started = std::time::Instant::now();
        Changeset::new(
            project.canvases.get(&canvas).unwrap().nodes.keys().map(|id| Change::MoveNode { canvas, id: *id, pos: Position { x: 3.0, y: 4.0 } }).collect(),
        )
        .stage(&project)
        .unwrap();
        let move_time = started.elapsed();
        assert_eq!(STAGE_PASSES.with(|c| c.get()), 1);
        assert_eq!(PROBE_CLONES.with(|c| c.get()), 0);
        assert_eq!(ANCHOR_VISITS.with(|c| c.get()), 1000);
        reset();
        let started = std::time::Instant::now();
        Changeset::new(project.canvases.get(&canvas).unwrap().nodes.keys().map(|id| Change::RemoveNode { canvas, id: *id }).collect()).stage(&project).unwrap();
        let structural_time = started.elapsed();
        assert_eq!(STAGE_PASSES.with(|c| c.get()), 3);
        assert_eq!(PROBE_CLONES.with(|c| c.get()), 1);
        assert_eq!(ANCHOR_VISITS.with(|c| c.get()), 3000);
        eprintln!(
            "1000-node/1000-pin debug measurement: moves={move_time:?}; certified removals={structural_time:?}; stage passes1/3, extra probe clones0/1, anchor visits1000/3000"
        );
    }

    #[test]
    fn ten_thousand_nodes_with_unrelated_comments_keep_one_stage_for_structural_edits() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Stress").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        Changeset::new((1u128..=10_000).map(|n| Change::AddNode { canvas, node: Node::new(NodeId::from_bytes(n.to_be_bytes()), "source.text") }).collect())
            .apply(&mut project)
            .unwrap();
        let mut root = Comment::new(CommentId::from_bytes([3; 16]), "Unrelated", Position::default());
        // An existing node pin elsewhere in the same canvas must be unrelated too.
        root.node = Some(NodeId::from_bytes(10_000u128.to_be_bytes()));
        Change::PutComment { canvas, comment: root }.apply(&mut project).unwrap();
        let remove = Change::RemoveNode { canvas, id: NodeId::from_bytes(1u128.to_be_bytes()) };
        for changes in
            [vec![remove.clone()], vec![remove, Change::AddNode { canvas, node: Node::new(NodeId::from_bytes(10_001u128.to_be_bytes()), "source.text") }]]
        {
            STAGE_PASSES.with(|c| c.set(0));
            PROBE_CLONES.with(|c| c.set(0));
            let staged = Changeset::new(changes).stage_certified(&project).unwrap();
            assert_eq!(STAGE_PASSES.with(|c| c.get()), 1);
            assert_eq!(PROBE_CLONES.with(|c| c.get()), 0);
            assert!(staged.actual_redo.is_none());
            let mut restored = staged.document;
            staged.inverse.apply(&mut restored).unwrap();
            assert_eq!(restored, project);
        }
    }

    #[test]
    fn pin_certification_tracks_original_incoming_and_cross_canvas_relationships() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let other = CanvasId::from_bytes([2; 16]);
        let id = NodeId::from_bytes([3; 16]);
        let root_id = CommentId::from_bytes([4; 16]);
        let project = Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap();
        let add = Change::AddNode { canvas, node: Node::new(id, "source.text") };
        let mut root = Comment::new(root_id, "Future pin", Position::default());
        root.node = Some(id);
        let pin = Change::SetCommentPin { canvas, id: root_id, node: Some(id), pos: Position::default() };
        assert!(needs_pin_certification(&[pin.clone(), add.clone()], &project));
        assert!(needs_pin_certification(&[Change::PutComment { canvas, comment: root.clone() }, add.clone()], &project));
        assert!(!needs_pin_certification(&[Change::PutComment { canvas: other, comment: root.clone() }, add.clone()], &project));
        let mut snapshot = Canvas::new(canvas, "Incoming");
        snapshot.comments.insert(root_id, root);
        assert!(needs_pin_certification(&[Change::AddCanvas { canvas: snapshot.clone() }, add.clone()], &project));
        let mut original = project;
        original.canvases.insert(canvas, snapshot);
        // Deleting/reanchoring the root later cannot hide the original relationship.
        assert!(needs_pin_certification(&[add.clone(), Change::RemoveComment { canvas, id: root_id }], &original));
        assert!(!needs_pin_certification(&[Change::SetNode { canvas, node: Node::new(id, "source.text") }, pin], &original));
        assert!(needs_pin_certification(
            &[Change::RestoreNode {
                canvas: other,
                node: Node::new(id, "source.text"),
                edges: vec![],
                pins: vec![CommentPin { id: root_id, offset: Position::default() }],
            }],
            &Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap()
        ));
    }

    #[test]
    fn future_pin_redo_growth_is_charged_at_the_exact_byte_boundary() {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let root = CommentId::from_bytes([3; 16]);
        Change::PutComment { canvas, comment: Comment::new(root, "Root", Position::default()) }.apply(&mut project).unwrap();
        let node = Node::new(NodeId::from_bytes([2; 16]), "source.text");
        let changes = vec![Change::SetCommentPin { canvas, id: root, node: Some(node.id), pos: Position { x: 1.0, y: 2.0 } }, Change::AddNode { canvas, node }];
        let staged = Changeset::prepare_certified(&changes, &project, validate::MAX_CHANGESET_BYTES).unwrap();
        let redo = staged.actual_redo.unwrap();
        let exact = serde_json::to_vec(&redo.changes).unwrap().len();
        assert!(serde_json::to_vec(&changes).unwrap().len() < exact);
        assert!(Changeset::prepare_with_budget(&changes, &project, exact - 1).is_err());
        assert!(Changeset::prepare_with_budget(&changes, &project, exact).is_ok());
    }

    #[test]
    fn node_addition_reserves_the_larger_restore_form_at_the_byte_boundary() {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Test").unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(NodeId::from_bytes([2; 16]), "test.text");
        node.title = "\"\n☃".repeat(20);
        let add = Change::AddNode { canvas, node: node.clone() };
        let restore = Change::RestoreNode { canvas, node, edges: vec![], pins: vec![] };
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
