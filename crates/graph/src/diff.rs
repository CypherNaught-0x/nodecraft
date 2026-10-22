//! The changes that turn one project into another (#119): how a peer takes in what a shared
//! document now holds through the one mutation path. Changes come in phases (removals, then
//! containers, then cards, wires and comments), split into changesets small enough to apply,
//! and every changeset leaves a valid project: whatever stays or is added is part of the target,
//! so a prefix of the phases never holds a wire, a reference or a name clash the target doesn't.
use std::collections::BTreeSet;

use nodecraft_types::{Error, NodeId};

use crate::validate::{self, MAX_CHANGES, MAX_CHANGESET_BYTES};
use crate::{Canvas, CanvasKind, Change, Changeset, NodeFieldAssignment, NodeTagAssignment, Project};

/// Each changeset stays well under the changeset byte limit, so its inverse fits too, and under
/// the 16 MiB a session's history allows one edit.
const CHUNK_BYTES: usize = MAX_CHANGESET_BYTES / 8;

/// Changesets that, applied in order, turn `from` into `to`. Empty when they are the same.
pub fn diff(from: &Project, to: &Project) -> Result<Vec<Changeset>, Error> {
    let mut phases = Phases::default();
    if from.title != to.title {
        phases.last.push(Change::SetProjectTitle { title: to.title.clone() });
    }
    for (id, old) in &from.canvases {
        match to.canvases.get(id) {
            Some(new) if editable_both(old, new) => canvas(&mut phases, old, new),
            Some(new) if old == new => {}
            _ => phases.canvases_out.push(Change::RemoveCanvas { id: *id }),
        }
    }
    for (id, new) in &to.canvases {
        let fresh = match from.canvases.get(id) {
            None => true,
            Some(old) => !editable_both(old, new) && old != new,
        };
        if !fresh {
            continue;
        }
        if new.kind == CanvasKind::Technique {
            // Frozen: it can only come whole.
            phases.canvases_in.push(Change::AddCanvas { canvas: new.clone() });
        } else {
            phases.canvases_in.push(Change::AddCanvas { canvas: Canvas { kind: new.kind, ..Canvas::new(*id, new.title.clone()) } });
            canvas(&mut phases, &Canvas { kind: new.kind, ..Canvas::new(*id, new.title.clone()) }, new);
        }
    }
    for (id, old) in &from.skills {
        // A changed skill goes and comes back, so two skills trading names never clash.
        if to.skills.get(id) != Some(old) {
            phases.removals.push(Change::RemoveSkill { id: *id });
        }
    }
    for (id, new) in &to.skills {
        if from.skills.get(id) != Some(new) {
            phases.last.push(Change::PutSkill { skill: new.clone() });
        }
    }
    phases.chunked()
}

fn editable_both(old: &Canvas, new: &Canvas) -> bool {
    old.kind == new.kind && old.kind != CanvasKind::Technique
}

/// The changes in each phase, in the order they apply.
#[derive(Default)]
struct Phases {
    wires_out: Vec<Change>,
    comments_out: Vec<Change>,
    memberships_out: Vec<Change>,
    cards_out: Vec<Change>,
    removals: Vec<Change>,
    canvases_out: Vec<Change>,
    canvases_in: Vec<Change>,
    containers_in: Vec<Change>,
    cards_in: Vec<Change>,
    wires_in: Vec<Change>,
    comments_in: Vec<Change>,
    last: Vec<Change>,
}

impl Phases {
    fn chunked(self) -> Result<Vec<Changeset>, Error> {
        let order = [
            self.wires_out,
            self.comments_out,
            self.memberships_out,
            self.cards_out,
            self.removals,
            self.canvases_out,
            self.canvases_in,
            self.containers_in,
            self.cards_in,
            self.wires_in,
            self.comments_in,
            self.last,
        ];
        let mut sets = Vec::new();
        let mut current = Vec::new();
        let mut bytes = 0usize;
        for change in order.into_iter().flatten() {
            let size = validate::encoded_bytes(&change, MAX_CHANGESET_BYTES)?;
            if !current.is_empty() && (current.len() >= MAX_CHANGES || bytes.saturating_add(size) > CHUNK_BYTES) {
                sets.push(Changeset::new(std::mem::take(&mut current)));
                bytes = 0;
            }
            bytes = bytes.saturating_add(size);
            current.push(change);
        }
        if !current.is_empty() {
            sets.push(Changeset::new(current));
        }
        Ok(sets)
    }
}

/// The changes within one editable canvas.
fn canvas(phases: &mut Phases, old: &Canvas, new: &Canvas) {
    let id = new.id;
    if old.title != new.title {
        phases.last.push(Change::SetCanvasTitle { canvas: id, title: new.title.clone() });
    }
    // A card whose ports change loses its wires until it has its new ports.
    let reshaped: BTreeSet<NodeId> =
        old.nodes.values().filter(|n| new.nodes.get(&n.id).is_some_and(|m| m.inputs != n.inputs || m.outputs != n.outputs)).map(|n| n.id).collect();
    let rewired = |edge: &crate::Edge| reshaped.contains(&edge.from.node) || reshaped.contains(&edge.to.node);
    for (edge_id, edge) in &old.edges {
        if new.edges.get(edge_id) != Some(edge) || rewired(edge) {
            phases.wires_out.push(Change::Disconnect { canvas: id, id: *edge_id });
        }
    }
    for (edge_id, edge) in &new.edges {
        if old.edges.get(edge_id) != Some(edge) || rewired(edge) {
            phases.wires_in.push(Change::Connect { canvas: id, edge: edge.clone() });
        }
    }
    for (comment_id, comment) in &old.comments {
        if new.comments.get(comment_id) != Some(comment) {
            phases.comments_out.push(Change::RemoveComment { canvas: id, id: *comment_id });
        }
    }
    for (comment_id, comment) in &new.comments {
        if old.comments.get(comment_id) != Some(comment) {
            phases.comments_in.push(Change::PutComment { canvas: id, comment: comment.clone() });
        }
    }
    // A card that stays but changes leaves its field and tag first, so they can go; it takes
    // its new ones when it changes.
    let mut fields = Vec::new();
    let mut tags = Vec::new();
    for (node_id, node) in &old.nodes {
        match new.nodes.get(node_id) {
            None => phases.cards_out.push(Change::RemoveNode { canvas: id, id: *node_id }),
            Some(next) if next != node => {
                if node.field.is_some() && next.field != node.field {
                    fields.push(NodeFieldAssignment { node: *node_id, field: None });
                }
                if node.tag.is_some() && next.tag != node.tag {
                    tags.push(NodeTagAssignment { node: *node_id, tag: None });
                }
                phases.cards_in.push(Change::SetNode { canvas: id, node: next.clone() });
            }
            Some(_) => {}
        }
    }
    for chunk in fields.chunks(MAX_CHANGES) {
        phases.memberships_out.push(Change::SetNodeFields { canvas: id, assignments: chunk.to_vec() });
    }
    for chunk in tags.chunks(MAX_CHANGES) {
        phases.memberships_out.push(Change::SetNodeTags { canvas: id, assignments: chunk.to_vec() });
    }
    for (node_id, node) in &new.nodes {
        if !old.nodes.contains_key(node_id) {
            phases.cards_in.push(Change::AddNode { canvas: id, node: node.clone() });
        }
    }
    for field_id in old.fields.keys() {
        if !new.fields.contains_key(field_id) {
            phases.removals.push(Change::RemoveField { canvas: id, id: *field_id });
        }
    }
    for (field_id, field) in &new.fields {
        if old.fields.get(field_id) != Some(field) {
            phases.containers_in.push(Change::PutField { canvas: id, field: field.clone() });
        }
    }
    for tag_id in old.tags.keys() {
        if !new.tags.contains_key(tag_id) {
            phases.removals.push(Change::RemoveTag { canvas: id, id: *tag_id });
        }
    }
    for (tag_id, tag) in &new.tags {
        if old.tags.get(tag_id) != Some(tag) {
            phases.containers_in.push(Change::PutTag { canvas: id, tag: tag.clone() });
        }
    }
}

#[cfg(test)]
mod tests;
