//! Fresh document ids made by the shell.
//!
//! Ids come from the caller (the types crate holds no randomness), and the shell has no portable
//! random source on wasm yet, so an id is a hash of what is being created, the session revision
//! and a probe counter, checked against the ids already taken. Collaboration (#119) will need ids
//! that are unique across peers too.
use nodecraft_graph::Project;
use nodecraft_types::{EdgeId, Error, ErrorKind, FieldId, NodeId};
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

/// An edge id unused on every canvas.
pub fn edge_id(project: &Project, seed: impl Hash) -> Option<EdgeId> {
    fresh(seed, |bytes| project.canvases().values().any(|c| c.edges().contains_key(&EdgeId::from_bytes(bytes)))).map(EdgeId::from_bytes)
}

/// A field id unused on every canvas.
pub fn field_id(project: &Project, seed: impl Hash) -> Option<FieldId> {
    fresh(seed, |bytes| project.canvases().values().any(|c| c.fields().contains_key(&FieldId::from_bytes(bytes)))).map(FieldId::from_bytes)
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

const MAX_ATTEMPTS: usize = 16;

#[cfg(test)]
thread_local! {
    pub(crate) static FAIL_ENTROPY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn random_id(used: impl Fn(&[u8; 16]) -> bool) -> Result<[u8; 16], Error> {
    allocate_with(used, |bytes| {
        #[cfg(test)]
        if FAIL_ENTROPY.with(|failure| failure.get()) {
            return Err(Error::new(ErrorKind::Unsupported, "Cannot obtain secure randomness. Check system/browser permissions and retry."));
        }
        getrandom::fill(bytes).map_err(|_| Error::new(ErrorKind::Unsupported, "Cannot obtain secure randomness. Check system/browser permissions and retry."))
    })
}

fn allocate_with(used: impl Fn(&[u8; 16]) -> bool, mut fill: impl FnMut(&mut [u8; 16]) -> Result<(), Error>) -> Result<[u8; 16], Error> {
    for _ in 0..MAX_ATTEMPTS {
        let mut bytes = [0; 16];
        fill(&mut bytes)?;
        // RFC 9562 UUIDv4 version and variant bits. No timestamp or persisted counter.
        if let Some(version) = bytes.get_mut(6) {
            *version = (*version & 0x0f) | 0x40;
        }
        if let Some(variant) = bytes.get_mut(8) {
            *variant = (*variant & 0x3f) | 0x80;
        }
        if !used(&bytes) {
            return Ok(bytes);
        }
    }
    Err(Error::new(ErrorKind::Conflict, "Cannot allocate a unique identifier. Retry the action; existing data and drafts are retained."))
}

/// Note creation uses NodeId's own namespace, independently of discussion drafts.
pub(crate) fn note_id(project: &nodecraft_graph::Project) -> Result<NodeId, Error> {
    random_id(|bytes| project.canvases().values().any(|canvas| canvas.nodes().contains_key(&NodeId::from_bytes(*bytes)))).map(NodeId::from_bytes)
}

#[cfg(test)]
mod secure_tests {
    use super::*;
    #[test]
    fn collision_retry_is_bounded_and_entropy_failure_is_actionable() {
        let mut attempts = 0;
        let result = allocate_with(
            |_| true,
            |bytes| {
                attempts += 1;
                bytes.fill(7);
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts, MAX_ATTEMPTS);
        let result = allocate_with(|_| false, |_| Err(Error::new(ErrorKind::Unsupported, "No entropy")));
        assert!(result.is_err());
    }
    #[test]
    fn successful_retry_sets_uuid_v4_bits_and_never_reuses_a_candidate() {
        let mut attempts = 0;
        let bytes = allocate_with(
            |bytes| bytes.first() == Some(&1),
            |bytes| {
                attempts += 1;
                bytes.fill(attempts);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(bytes.get(6).copied().unwrap() >> 4, 4);
        assert_eq!(bytes.get(8).copied().unwrap() >> 6, 2);
    }
}
