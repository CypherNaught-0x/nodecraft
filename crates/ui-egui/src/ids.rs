//! Fresh document ids made by the shell.
//!
//! Ids come from the caller (the types crate holds no randomness), and the shell has no portable
//! random source on wasm yet, so an id is a hash of what is being created, the session revision
//! and a probe counter, checked against the ids already taken. Collaboration (#119) will need ids
//! that are unique across peers too.
use nodecraft_graph::Project;
use nodecraft_types::{EdgeId, GenerationId, NodeId};
use std::hash::{DefaultHasher, Hash, Hasher};

/// Probes before giving up (a project would need ~2^63 ids for one to collide).
const PROBES: u64 = 1024;

/// 16 bytes from `seed` and `attempt`, the first that `taken` doesn't claim.
pub fn fresh(seed: impl Hash, taken: impl Fn([u8; 16]) -> bool) -> Option<[u8; 16]> {
    (0..PROBES).find_map(|attempt| {
        let half = |lane: u8| {
            let mut h = DefaultHasher::new();
            (lane, &seed, attempt).hash(&mut h);
            h.finish().to_le_bytes()
        };
        let mut bytes = [0u8; 16];
        for (dst, src) in bytes.iter_mut().zip(half(0).into_iter().chain(half(1))) {
            *dst = src;
        }
        Some(bytes).filter(|bytes| !taken(*bytes))
    })
}

/// A node id unused on every canvas.
pub fn node_id(project: &Project, seed: impl Hash) -> Option<NodeId> {
    fresh(seed, |bytes| project.canvases().values().any(|c| c.nodes().contains_key(&NodeId::from_bytes(bytes)))).map(NodeId::from_bytes)
}

/// A generation id unused in every node's history.
pub fn generation_id(project: &Project, seed: impl Hash) -> Option<GenerationId> {
    let taken = |bytes| {
        let id = GenerationId::from_bytes(bytes);
        project.canvases().values().any(|c| c.nodes().values().any(|n| n.history.iter().any(|g| g.id == id)))
    };
    fresh(seed, taken).map(GenerationId::from_bytes)
}

/// An edge id unused on every canvas.
pub fn edge_id(project: &Project, seed: impl Hash) -> Option<EdgeId> {
    fresh(seed, |bytes| project.canvases().values().any(|c| c.edges().contains_key(&EdgeId::from_bytes(bytes)))).map(EdgeId::from_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_past_taken_ids_and_gives_up_eventually() {
        let first = fresh("seed", |_| false).unwrap();
        let second = fresh("seed", |bytes| bytes == first).unwrap();
        assert_ne!(first, second);
        assert_eq!(fresh("seed", |_| true), None);
        assert_ne!(fresh("a", |_| false), fresh("b", |_| false));
    }
}
