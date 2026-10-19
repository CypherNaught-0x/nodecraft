//! Batch fan-out (#92): which item combinations a node downstream of batches runs for.
//!
//! Every single input fed by a batch, or by a result fanned out by batches, is a *relation*: one
//! [`ItemTag`] per value, all naming the same batches (its axes). A step's relations combine in
//! input order. Relations that share a batch are joined on its items, so a value that came
//! from item 3 of a batch only meets other values from item 3 of that batch. Relations with no
//! batch in common pair up: every combination for Cross, item by item for Zip. Zip is used only
//! when every batch on both sides asks for it, and needs equal sizes; a mix of Cross and Zip, or
//! a Zip of unequal sizes, is an error rather than a silently larger (or smaller) paid run.
//!
//! The number of combinations is checked against [`MAX_ITEMS`] before any is built.

use nodecraft_graph::{ItemKey, ItemTag};
use nodecraft_nodes::batch::Pairing;
use nodecraft_types::{ItemId, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Most item combinations one step fans out to: a fanned result holds one value per item, within
/// the document's list limit.
pub const MAX_ITEMS: usize = nodecraft_nodes::batch::MAX_ITEMS;

/// One combination: its tag and, per relation, the index of the value it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub tag: ItemTag,
    pub rows: Vec<usize>,
}

/// The batches a tag names.
pub fn axes(tag: &ItemTag) -> BTreeSet<NodeId> {
    tag.iter().map(|key| key.batch).collect()
}

/// The stable id of a batch item: its batch, its value and which occurrence of that value it is,
/// so reordering items or adding others keeps each item's id.
pub fn item_id(batch: NodeId, value: &Value, occurrence: u32) -> ItemId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nodecraft.batch.item\0");
    hasher.update(batch.as_bytes());
    // Values are validated document data; their canonical JSON is the identity. A value that
    // can't be encoded hashes as its position alone, which is still deterministic.
    if let Ok(bytes) = serde_json::to_vec(value) {
        hasher.update(&bytes);
    }
    hasher.update(&occurrence.to_le_bytes());
    let digest = hasher.finalize();
    let mut id = [0u8; 16];
    for (to, from) in id.iter_mut().zip(digest.as_bytes()) {
        *to = *from;
    }
    ItemId::from_bytes(id)
}

/// The tags of a batch's items, in order.
pub fn batch_tags(batch: NodeId, items: &[Value]) -> Vec<ItemTag> {
    let mut seen: BTreeMap<[u8; 16], u32> = BTreeMap::new();
    items
        .iter()
        .map(|value| {
            let first = *item_id(batch, value, 0).as_bytes();
            let occurrence = seen.entry(first).or_insert(0);
            let id = item_id(batch, value, *occurrence);
            *occurrence = occurrence.saturating_add(1);
            vec![ItemKey { batch, item: id }]
        })
        .collect()
}

/// Checks that every tag of a stored relation names the same batches, each once, in order.
pub fn consistent(tags: &[ItemTag]) -> bool {
    let Some(first) = tags.first() else { return true };
    let shape: Vec<NodeId> = first.iter().map(|key| key.batch).collect();
    tags.iter().all(|tag| {
        tag.len() == shape.len()
            && tag.iter().zip(&shape).all(|(key, batch)| key.batch == *batch)
            && tag.windows(2).all(|pair| matches!(pair, [a, b] if a.batch < b.batch))
    })
}

fn merge(a: &ItemTag, b: &ItemTag) -> ItemTag {
    let mut tag: Vec<ItemKey> = a.iter().chain(b.iter()).copied().collect();
    tag.sort();
    tag.dedup();
    tag
}

fn project(tag: &ItemTag, onto: &BTreeSet<NodeId>) -> Vec<ItemKey> {
    tag.iter().filter(|key| onto.contains(&key.batch)).copied().collect()
}

fn too_many() -> String {
    format!("its batches would run it more than {MAX_ITEMS} times")
}

/// The combinations of `relations` (each a list of tags with one shape), in a stable order:
/// earlier relations vary slowest. `pairing` says how each batch pairs; a batch it doesn't know
/// pairs by Cross. An empty relation makes no combinations.
pub fn combine(relations: &[&[ItemTag]], pairing: &BTreeMap<NodeId, Pairing>) -> Result<Vec<Cell>, String> {
    let mut cells = vec![Cell { tag: Vec::new(), rows: Vec::new() }];
    let mut seen: BTreeSet<NodeId> = BTreeSet::new();
    for relation in relations {
        if !consistent(relation) {
            return Err("its inputs' items name different batches".into());
        }
        let shape = relation.first().map(axes).unwrap_or_default();
        let shared: BTreeSet<NodeId> = seen.intersection(&shape).copied().collect();
        let mut next = Vec::new();
        if relation.is_empty() {
            return Ok(Vec::new());
        } else if !shared.is_empty() {
            // Join on the shared batches' items.
            let mut index: BTreeMap<Vec<ItemKey>, Vec<usize>> = BTreeMap::new();
            for (row, tag) in relation.iter().enumerate() {
                index.entry(project(tag, &shared)).or_default().push(row);
            }
            for cell in &cells {
                for row in index.get(&project(&cell.tag, &shared)).into_iter().flatten() {
                    let tag = relation.get(*row).ok_or("an item row is missing")?;
                    if next.len() >= MAX_ITEMS {
                        return Err(too_many());
                    }
                    next.push(Cell { tag: merge(&cell.tag, tag), rows: cell.rows.iter().copied().chain([*row]).collect() });
                }
            }
        } else {
            let mode = pairing_of(seen.iter().chain(&shape), pairing)?;
            if seen.is_empty() || mode == Pairing::Cross {
                let count = cells.len().checked_mul(relation.len()).filter(|n| *n <= MAX_ITEMS).ok_or_else(too_many)?;
                next.reserve(count);
                for cell in &cells {
                    for (row, tag) in relation.iter().enumerate() {
                        next.push(Cell { tag: merge(&cell.tag, tag), rows: cell.rows.iter().copied().chain([row]).collect() });
                    }
                }
            } else {
                if cells.len() != relation.len() {
                    return Err(format!(
                        "its batches are zipped but have {} and {} items; make them the same size or pair them by Cross",
                        cells.len(),
                        relation.len()
                    ));
                }
                for (row, (cell, tag)) in cells.iter().zip(relation.iter()).enumerate() {
                    next.push(Cell { tag: merge(&cell.tag, tag), rows: cell.rows.iter().copied().chain([row]).collect() });
                }
            }
        }
        if next.first().is_some_and(|cell| cell.tag.len() > nodecraft_graph::MAX_ITEM_AXES) {
            return Err(format!("it combines more than {} batches", nodecraft_graph::MAX_ITEM_AXES));
        }
        seen.extend(shape);
        cells = next;
    }
    Ok(cells)
}

/// Zip only when every batch involved asks for it.
fn pairing_of<'a>(batches: impl Iterator<Item = &'a NodeId>, pairing: &BTreeMap<NodeId, Pairing>) -> Result<Pairing, String> {
    let modes: BTreeSet<bool> = batches.map(|batch| pairing.get(batch).copied().unwrap_or(Pairing::Cross) == Pairing::Zip).collect();
    match (modes.contains(&true), modes.contains(&false)) {
        (true, true) => Err("its batches disagree on pairing; set them all to Cross or all to Zip".into()),
        (true, false) => Ok(Pairing::Zip),
        _ => Ok(Pairing::Cross),
    }
}

#[cfg(test)]
mod tests;
