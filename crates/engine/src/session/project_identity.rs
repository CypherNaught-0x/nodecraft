//! Migrate the old starter identity before a local document's checkpoint is published.
use super::{Entry, History, NEVER_SAVED, history_bytes};
use nodecraft_format::Snapshot;
use nodecraft_graph::Change;
use nodecraft_types::{Error, ErrorKind, ProjectId};

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

pub(crate) fn prepare(mut snapshot: Snapshot) -> Result<OpenedDocument, Error> {
    if snapshot.project.id() != LEGACY {
        return Ok(OpenedDocument { snapshot, migration: None });
    }
    let to = fresh()?;
    let inverse = Change::SetProjectId { id: to }.apply(&mut snapshot.project)?;
    let bytes = history_bytes(&inverse)?;
    let mut history = History::default();
    history.entries.try_reserve(1).map_err(|_| Error::new(ErrorKind::Internal, "couldn't record the project's identity migration"))?;
    history.push(Entry { inverse, bytes, state: NEVER_SAVED, shared: false });
    Ok(OpenedDocument { snapshot, migration: Some(Migration { from: LEGACY, to, history }) })
}

fn fresh() -> Result<ProjectId, Error> {
    for _ in 0..8 {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "project identity entropy is unavailable; the project wasn't opened"))?;
        let id = ProjectId::from_bytes(bytes);
        if id != LEGACY && bytes != [0; 16] {
            return Ok(id);
        }
    }
    Err(Error::new(ErrorKind::Internal, "couldn't allocate a fresh project identity; the project wasn't opened"))
}
