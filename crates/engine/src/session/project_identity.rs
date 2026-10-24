//! Migrate the old starter identity before a local document's checkpoint is published.
use super::{Entry, History, NEVER_SAVED, history_bytes};
use nodecraft_format::Snapshot;
use nodecraft_graph::Change;
use nodecraft_types::{Blake3, Error, ErrorKind, ProjectId};

pub(crate) const LEGACY: ProjectId = ProjectId::from_bytes([2; 16]);

pub(crate) struct Migration {
    pub(super) from: ProjectId,
    pub(super) to: ProjectId,
    pub(super) history: History,
}

pub(crate) struct OpenedDocument {
    pub(crate) snapshot: Snapshot,
    pub(crate) migration: Option<Migration>,
}

pub(crate) fn prepare(mut snapshot: Snapshot, scope: Option<Blake3>) -> Result<OpenedDocument, Error> {
    // An unlocated snapshot cannot safely be distinguished from another copy. Keep
    // its conservative shared cap until the host supplies a stable source scope.
    if snapshot.project.id() != LEGACY || scope.is_none() {
        return Ok(OpenedDocument { snapshot, migration: None });
    }
    let to = stable(scope.ok_or_else(|| Error::new(ErrorKind::Internal, "project identity scope is missing"))?)?;
    let inverse = Change::SetProjectId { id: to }.apply(&mut snapshot.project)?;
    let bytes = history_bytes(&inverse)?;
    let mut history = History::default();
    history.entries.try_reserve(1).map_err(|_| Error::new(ErrorKind::Internal, "couldn't record the project's identity migration"))?;
    history.push(Entry { inverse, bytes, state: NEVER_SAVED, shared: false });
    Ok(OpenedDocument { snapshot, migration: Some(Migration { from: LEGACY, to, history }) })
}

fn stable(scope: Blake3) -> Result<ProjectId, Error> {
    for attempt in 0u8..8 {
        let mut hash = blake3::Hasher::new_derive_key("NodeCraft legacy project identity v1");
        hash.update(scope.as_bytes());
        hash.update(&[attempt]);
        let digest = hash.finalize();
        let bytes: [u8; 16] = digest
            .as_bytes()
            .get(..16)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| Error::new(ErrorKind::Internal, "project identity digest is invalid"))?;
        let id = ProjectId::from_bytes(bytes);
        if id != LEGACY && bytes != [0; 16] {
            return Ok(id);
        }
    }
    Err(Error::new(ErrorKind::Internal, "couldn't derive a project identity; the project wasn't opened"))
}
