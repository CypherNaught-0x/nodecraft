//! The index and archive contract shared by bundled data, remote data and author CI.
use super::invalid;
use nodecraft_format::Snapshot;
use nodecraft_types::{CanvasId, Error};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_INDEX_BYTES: usize = 128 * 1024;
/// Author CI's complete append-only metadata, never downloaded by the runtime client.
pub const MAX_HISTORY_BYTES: usize = 1024 * 1024;
pub const MAX_HISTORY_ENTRIES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub description: String,
    pub version: u32,
    pub file: String,
    pub blake3: String,
    pub features: Vec<String>,
    pub canvas: CanvasId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema_version: u32,
    templates: Vec<Entry>,
}

/// Reject unknown schemas before typed records are used. The entire raw input is bounded first.
pub fn parse_index(bytes: &[u8]) -> Result<Vec<Entry>, Error> {
    parse_records(bytes, MAX_INDEX_BYTES, 128, false)
}
/// The same record contract, allowing distinct immutable versions of one ID. All historical
/// root canvas IDs remain distinct, including versions absent from the current index.
pub fn parse_history(bytes: &[u8]) -> Result<Vec<Entry>, Error> {
    parse_records(bytes, MAX_HISTORY_BYTES, MAX_HISTORY_ENTRIES, true)
}
fn parse_records(bytes: &[u8], byte_limit: usize, entry_limit: usize, history: bool) -> Result<Vec<Entry>, Error> {
    if bytes.len() > byte_limit {
        return Err(invalid());
    }
    let index: Index = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if index.schema_version != 1 || index.templates.is_empty() || index.templates.len() > entry_limit {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    let mut versions = BTreeSet::new();
    let mut canvases = BTreeSet::new();
    for entry in &index.templates {
        if entry.version == 0
            || entry.id.is_empty()
            || entry.id.len() > 64
            || !entry.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
            || (!history && !ids.insert(entry.id.as_str()))
            || !versions.insert((entry.id.as_str(), entry.version))
            || !canvases.insert(entry.canvas)
            || entry.title.trim().is_empty()
            || entry.title.len() > 128
            || entry.description.trim().is_empty()
            || entry.description.len() > 512
            || entry.features.is_empty()
            || entry.features.len() > 16
            || entry.features.iter().any(|f| f.trim().is_empty() || f.len() > 64)
            || entry.blake3.len() != 64
            || !entry.blake3.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || entry.file != format!("templates/{}/{}/{}.ncraft", entry.id, entry.version, entry.blake3)
        {
            return Err(invalid());
        }
    }
    Ok(index.templates)
}

/// Verify exact bytes before decoding. Format admission refuses compressed/encrypted ZIPs and
/// verifies every embedded asset, graph and reference, so a tiny download cannot expand as a ZIP bomb.
pub fn validate_archive(entry: &Entry, bytes: &[u8]) -> Result<Snapshot, Error> {
    if bytes.len() > MAX_BYTES || blake3::hash(bytes).to_hex().as_str() != entry.blake3 {
        return Err(invalid());
    }
    let snapshot = nodecraft_format::decode(bytes)?;
    if !snapshot.project.canvases().contains_key(&entry.canvas) {
        return Err(invalid());
    }
    Ok(snapshot)
}

/// The same node validation admission uses, without readiness checks or provider calls.
pub fn validate_nodes(snapshot: &Snapshot, kinds: &nodecraft_nodes::Registry) -> Result<(), Error> {
    for canvas in snapshot.project.canvases().values() {
        for node in canvas.nodes().values() {
            kinds.get(&node.kind)?.validate_instance(node, false)?;
        }
    }
    Ok(())
}
