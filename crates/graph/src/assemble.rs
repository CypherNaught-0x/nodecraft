//! Building a valid project from loose parts (#119). A shared document merges what several people
//! did at once, and two edits that are each valid can together be invalid: two edges into one
//! single input, edges that close a cycle, a node in a field someone else removed. Every peer
//! assembles the same parts into the same project: parts are taken in id order, and one that
//! would make the project invalid is repaired (a missing field or tag, a comment's missing
//! anchor) or left out, and reported so the shared document can drop it too.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use nodecraft_types::{CanvasId, CommentId, EdgeId, Error, FieldId, NodeId, ProjectId, SkillId, TagId};

use crate::validate::{self, MAX_CANVASES, MAX_EDGES, MAX_NODES};
use crate::{Canvas, CanvasKind, Comment, Edge, Field, Node, Project, Skill, Tag};

/// How many cycle-breaking passes run, each dropping one edge, before every edge left among the
/// nodes in cycles goes at once. Honest concurrent edits close a few cycles; this bounds a hostile
/// document's cost.
const CYCLE_PASSES: usize = 64;

/// A canvas's own part: an editable canvas's header, or a technique canvas whole (it is frozen,
/// so it is shared as one piece).
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasPart {
    Header { id: CanvasId, title: String, kind: CanvasKind },
    Frozen(Box<Canvas>),
}

impl CanvasPart {
    pub fn id(&self) -> CanvasId {
        match self {
            Self::Header { id, .. } => *id,
            Self::Frozen(canvas) => canvas.id,
        }
    }
}

/// A project in pieces, as a shared document holds it. Order doesn't matter.
#[derive(Clone, Debug, Default)]
pub struct Parts {
    pub title: String,
    pub canvases: Vec<CanvasPart>,
    pub nodes: Vec<(CanvasId, Node)>,
    pub edges: Vec<(CanvasId, Edge)>,
    pub fields: Vec<(CanvasId, Field)>,
    pub tags: Vec<(CanvasId, Tag)>,
    pub comments: Vec<(CanvasId, Comment)>,
    pub skills: Vec<Skill>,
}

impl Parts {
    /// `project` in pieces: assembling them gives `project` back.
    pub fn of(project: &Project) -> Self {
        let mut parts = Self { title: project.title.clone(), skills: project.skills.values().cloned().collect(), ..Self::default() };
        for (id, canvas) in &project.canvases {
            if canvas.kind == CanvasKind::Technique {
                parts.canvases.push(CanvasPart::Frozen(Box::new(canvas.clone())));
                continue;
            }
            parts.canvases.push(CanvasPart::Header { id: *id, title: canvas.title.clone(), kind: canvas.kind });
            parts.nodes.extend(canvas.nodes.values().map(|n| (*id, n.clone())));
            parts.edges.extend(canvas.edges.values().map(|e| (*id, e.clone())));
            parts.fields.extend(canvas.fields.values().map(|f| (*id, f.clone())));
            parts.tags.extend(canvas.tags.values().map(|t| (*id, t.clone())));
            parts.comments.extend(canvas.comments.values().map(|c| (*id, c.clone())));
        }
        parts
    }
}

/// One part, by what it is and where.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Title,
    Canvas(CanvasId),
    Node(CanvasId, NodeId),
    Edge(CanvasId, EdgeId),
    Field(CanvasId, FieldId),
    Tag(CanvasId, TagId),
    Comment(CanvasId, CommentId),
    Skill(SkillId),
}

/// The project, and the parts it left out or changed to fit.
#[derive(Clone, Debug)]
pub struct Assembled {
    pub project: Project,
    pub dropped: BTreeSet<Part>,
    pub repaired: BTreeSet<Part>,
}

/// Assembles `parts` into a valid project, the same way on every peer. Fails only when the whole
/// project is still over a limit after its parts were checked (its total size).
pub fn assemble(id: ProjectId, parts: Parts) -> Result<Assembled, Error> {
    let mut dropped = BTreeSet::new();
    let mut repaired = BTreeSet::new();
    let title = if validate::text(&parts.title, 4096).is_ok() {
        parts.title
    } else {
        repaired.insert(Part::Title);
        String::new()
    };
    let mut project = Project { id, title, canvases: BTreeMap::new(), skills: BTreeMap::new() };
    let mut seen = Seen::default();

    // Canvases, in id order. A frozen canvas comes whole; its contents can't change, so other
    // parts naming it are left out.
    let mut canvases: BTreeMap<CanvasId, CanvasPart> = BTreeMap::new();
    for part in parts.canvases {
        let id = part.id();
        match canvases.entry(id) {
            std::collections::btree_map::Entry::Occupied(_) => {
                dropped.insert(Part::Canvas(id));
            }
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(part);
            }
        }
    }
    let mut frozen = BTreeSet::new();
    for (id, part) in canvases {
        if project.canvases.len() >= MAX_CANVASES {
            dropped.insert(Part::Canvas(id));
            continue;
        }
        let canvas = match part {
            CanvasPart::Header { title, kind, .. } => {
                if kind == CanvasKind::Technique || validate::text(&title, 4096).is_err() {
                    dropped.insert(Part::Canvas(id));
                    continue;
                }
                Canvas { kind, ..Canvas::new(id, title) }
            }
            CanvasPart::Frozen(canvas) => {
                if canvas.kind != CanvasKind::Technique || canvas.validate().is_err() || !seen.admit(&canvas) {
                    dropped.insert(Part::Canvas(id));
                    continue;
                }
                frozen.insert(id);
                *canvas
            }
        };
        project.canvases.insert(id, canvas);
    }
    let open: BTreeSet<CanvasId> = project.canvases.keys().copied().filter(|id| !frozen.contains(id)).collect();
    let mut place = |canvas: CanvasId, part: Part| {
        if open.contains(&canvas) {
            true
        } else {
            dropped.insert(part);
            false
        }
    };

    // Fields and tags first: nodes refer to them.
    let mut fields = sorted(parts.fields, |f| f.id);
    let mut tags = sorted(parts.tags, |t| t.id);
    let mut nodes = sorted(parts.nodes, |n| n.id);
    let mut edges = sorted(parts.edges, |e| e.id);
    let mut comments = sorted(parts.comments, |c| c.id);
    fields.retain(|(canvas, field)| place(*canvas, Part::Field(*canvas, field.id)));
    tags.retain(|(canvas, tag)| place(*canvas, Part::Tag(*canvas, tag.id)));
    nodes.retain(|(canvas, node)| place(*canvas, Part::Node(*canvas, node.id)));
    edges.retain(|(canvas, edge)| place(*canvas, Part::Edge(*canvas, edge.id)));
    comments.retain(|(canvas, comment)| place(*canvas, Part::Comment(*canvas, comment.id)));

    for (canvas_id, field) in fields {
        let part = Part::Field(canvas_id, field.id);
        let Some(canvas) = project.canvases.get_mut(&canvas_id) else { continue };
        if field.validate().is_err() || seen.fields.len() >= MAX_NODES || !seen.fields.insert(field.id) {
            dropped.insert(part);
            continue;
        }
        canvas.fields.insert(field.id, field);
    }
    for (canvas_id, tag) in tags {
        let part = Part::Tag(canvas_id, tag.id);
        let Some(canvas) = project.canvases.get_mut(&canvas_id) else { continue };
        if tag.validate().is_err() || seen.tags.len() >= MAX_NODES || !seen.tags.insert(tag.id) {
            dropped.insert(part);
            continue;
        }
        canvas.tags.insert(tag.id, tag);
    }
    for (canvas_id, mut node) in nodes {
        let part = Part::Node(canvas_id, node.id);
        let Some(canvas) = project.canvases.get_mut(&canvas_id) else { continue };
        if validate::node(&node).is_err() || seen.nodes.len() >= MAX_NODES || seen.nodes.contains(&node.id) || !seen.generations_free(&node) {
            dropped.insert(part);
            continue;
        }
        // A field or tag someone removed meanwhile: the node stays, out of it.
        if node.field.is_some_and(|f| !canvas.fields.contains_key(&f)) {
            node.field = None;
            repaired.insert(part);
        }
        if node.tag.is_some_and(|t| !canvas.tags.contains_key(&t)) {
            node.tag = None;
            repaired.insert(part);
        }
        seen.take(&node);
        canvas.nodes.insert(node.id, node);
    }
    for (canvas_id, edge) in edges {
        let part = Part::Edge(canvas_id, edge.id);
        let Some(canvas) = project.canvases.get_mut(&canvas_id) else { continue };
        if seen.edges.len() >= MAX_EDGES || seen.edges.contains(&edge.id) || !fits(canvas, &edge) {
            dropped.insert(part);
            continue;
        }
        seen.edges.insert(edge.id);
        canvas.edges.insert(edge.id, edge);
    }
    for id in open.iter().copied() {
        if let Some(canvas) = project.canvases.get_mut(&id) {
            for edge in break_cycles(canvas) {
                seen.edges.remove(&edge);
                dropped.insert(Part::Edge(id, edge));
            }
        }
    }
    let mut messages: BTreeMap<CanvasId, usize> = BTreeMap::new();
    for (canvas_id, mut comment) in comments {
        let part = Part::Comment(canvas_id, comment.id);
        let Some(canvas) = project.canvases.get_mut(&canvas_id) else { continue };
        if comment.node.is_some_and(|n| !canvas.nodes.contains_key(&n)) {
            // Its card went: the comment stays, unpinned, with its offset read as a place on the
            // canvas.
            comment.node = None;
            repaired.insert(part);
        }
        let ids: Vec<CommentId> = std::iter::once(comment.id).chain(comment.replies.iter().map(|r| r.id)).collect();
        let count = messages.entry(canvas_id).or_insert(0usize);
        if validate::comment(&comment).is_err()
            || comment.world_position(canvas).is_err()
            || count.saturating_add(ids.len()) > MAX_NODES
            || seen.messages.len().saturating_add(ids.len()) > MAX_NODES
            || ids.iter().any(|id| seen.messages.contains(id))
            || ids.iter().collect::<BTreeSet<_>>().len() != ids.len()
        {
            repaired.remove(&part);
            dropped.insert(part);
            continue;
        }
        *count = count.saturating_add(ids.len());
        seen.messages.extend(ids);
        canvas.comments.insert(comment.id, comment);
    }

    let mut skills: BTreeMap<SkillId, Skill> = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut parts_skills = parts.skills;
    parts_skills.sort_by_key(|skill| skill.id);
    for skill in parts_skills {
        let part = Part::Skill(skill.id);
        if skills.len() >= crate::MAX_SKILLS || skills.contains_key(&skill.id) || skill.validate().is_err() || !names.insert(skill.key()) {
            dropped.insert(part);
            continue;
        }
        skills.insert(skill.id, skill);
    }
    project.skills = skills;
    project.validate()?;
    Ok(Assembled { project, dropped, repaired })
}

/// Parts in id order (a stable sort: of two with one id, the first given is taken).
fn sorted<T, K: Ord>(mut parts: Vec<(CanvasId, T)>, key: impl Fn(&T) -> K) -> Vec<(CanvasId, T)> {
    parts.sort_by_key(|(_, part)| key(part));
    parts
}

/// The ids taken so far across the project.
#[derive(Default)]
struct Seen {
    nodes: BTreeSet<NodeId>,
    edges: BTreeSet<EdgeId>,
    fields: BTreeSet<FieldId>,
    tags: BTreeSet<TagId>,
    messages: BTreeSet<CommentId>,
    generations: BTreeSet<nodecraft_types::GenerationId>,
    versions: BTreeSet<(nodecraft_types::TechniqueId, u32)>,
}

impl Seen {
    fn generations_free(&self, node: &Node) -> bool {
        node.history.iter().all(|g| !self.generations.contains(&g.id))
    }
    fn take(&mut self, node: &Node) {
        self.nodes.insert(node.id);
        self.generations.extend(node.history.iter().map(|g| g.id));
    }
    /// Takes a frozen canvas's ids, unless one is taken already.
    fn admit(&mut self, canvas: &Canvas) -> bool {
        let messages = canvas.comments.values().flat_map(|c| std::iter::once(c.id).chain(c.replies.iter().map(|r| r.id)));
        let version = canvas.technique.as_ref().map(|t| (t.id, t.version));
        let free = canvas.nodes.values().all(|n| !self.nodes.contains(&n.id) && self.generations_free(n))
            && canvas.edges.keys().all(|id| !self.edges.contains(id))
            && canvas.fields.keys().all(|id| !self.fields.contains(id))
            && canvas.tags.keys().all(|id| !self.tags.contains(id))
            && messages.clone().all(|id| !self.messages.contains(&id))
            && version.is_none_or(|v| !self.versions.contains(&v))
            && self.nodes.len().saturating_add(canvas.nodes.len()) <= MAX_NODES
            && self.edges.len().saturating_add(canvas.edges.len()) <= MAX_EDGES
            && self.fields.len().saturating_add(canvas.fields.len()) <= MAX_NODES
            && self.tags.len().saturating_add(canvas.tags.len()) <= MAX_NODES
            && self.messages.len().saturating_add(messages.clone().count()) <= MAX_NODES;
        if free {
            for node in canvas.nodes.values() {
                self.take(node);
            }
            self.edges.extend(canvas.edges.keys().copied());
            self.fields.extend(canvas.fields.keys().copied());
            self.tags.extend(canvas.tags.keys().copied());
            self.messages.extend(messages);
            self.versions.extend(version);
        }
        free
    }
}

/// Whether `edge` joins two of the canvas's nodes on matching ports, into a free input slot.
fn fits(canvas: &Canvas, edge: &Edge) -> bool {
    let (Some(source), Some(target)) = (canvas.nodes.get(&edge.from.node), canvas.nodes.get(&edge.to.node)) else { return false };
    let (Some(output), Some(input)) =
        (source.outputs.iter().find(|p| p.name == edge.from.port), target.inputs.iter().find(|p| p.name == edge.to.port))
    else {
        return false;
    };
    if output.port_type.modality != input.port_type.modality || (output.port_type.many && !input.port_type.many) {
        return false;
    }
    if !input.port_type.many && edge.order != 0 {
        return false;
    }
    !canvas.edges.values().any(|e| e.to.node == edge.to.node && e.to.port == edge.to.port && (e.order == edge.order || !input.port_type.many))
}

/// The nodes Kahn's algorithm can't order: those in a cycle, or downstream of one.
fn unordered(canvas: &Canvas) -> BTreeSet<NodeId> {
    let mut indegree: BTreeMap<NodeId, usize> = canvas.nodes.keys().map(|id| (*id, 0)).collect();
    let mut downstream: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges.values() {
        if let Some(count) = indegree.get_mut(&edge.to.node) {
            *count = count.saturating_add(1);
        }
        downstream.entry(edge.from.node).or_default().push(edge.to.node);
    }
    let mut ready: VecDeque<NodeId> = indegree.iter().filter_map(|(id, count)| (*count == 0).then_some(*id)).collect();
    while let Some(id) = ready.pop_front() {
        indegree.remove(&id);
        for target in downstream.get(&id).into_iter().flatten() {
            if let Some(count) = indegree.get_mut(target) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push_back(*target);
                }
            }
        }
    }
    indegree.into_keys().collect()
}

/// Drops edges until the canvas has no cycle: the newest-id edge among the unordered nodes, one
/// at a time, then (past the pass limit) every edge among them, which always leaves none.
fn break_cycles(canvas: &mut Canvas) -> Vec<EdgeId> {
    let mut gone = Vec::new();
    for pass in 0..=CYCLE_PASSES {
        let stuck = unordered(canvas);
        if stuck.is_empty() {
            break;
        }
        let among: Vec<EdgeId> = canvas.edges.values().filter(|e| stuck.contains(&e.from.node) && stuck.contains(&e.to.node)).map(|e| e.id).collect();
        let drop: Vec<EdgeId> = if pass < CYCLE_PASSES { among.last().copied().into_iter().collect() } else { among };
        if drop.is_empty() {
            break;
        }
        for id in drop {
            canvas.edges.remove(&id);
            gone.push(id);
        }
    }
    gone
}

#[cfg(test)]
mod tests;
