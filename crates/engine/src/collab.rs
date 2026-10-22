//! A session shared live with other people (#119): the session's edits go out to a shared
//! document, and what the others did comes back in as ordinary changesets. The host moves
//! frames between this and its connection to the relay; nothing here touches a socket.
//!
//! Each turn the host calls [`Collab::publish`] after local edits, [`Collab::receive`] for each
//! frame from the relay, and [`Collab::settle`] before drawing, sending whatever each returns.
use std::collections::{BTreeMap, BTreeSet};

use nodecraft_graph::{CanvasKind, Change, Changeset, Node, Project, diff};
use nodecraft_sync::{Frame, MAX_ASSET_BYTES, Materialized, Mirror, PROTOCOL, RoomId};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, ProjectId};

use crate::session::Session;

/// The most assets kept waiting for the edit that needs them.
const MAX_ARRIVED_BYTES: usize = 256 << 20;

pub struct Collab {
    mirror: Mirror,
    room: RoomId,
    /// The session revision last published; a later one may hold local edits.
    published: Option<u64>,
    /// Something came in that the session hasn't taken yet.
    unsettled: bool,
    /// Assets asked of the others, and those that arrived before the edit that needs them.
    asked: BTreeSet<Blake3>,
    arrived: BTreeMap<Blake3, Vec<u8>>,
    /// Assets the others asked for, answered at the next publish.
    wanted: BTreeSet<Blake3>,
}

impl Collab {
    /// Shares `session`'s project in `room`. The relay asks for it when the connection opens.
    pub fn share(session: &Session, room: RoomId) -> Result<Self, Error> {
        let (mirror, _) = Mirror::share(session.document())?;
        Ok(Self::with(mirror, room, Some(session.revision())))
    }

    /// Joins `room`. Once [`Self::project_id`] knows the project, the host opens an empty
    /// session for it (`Project::new(id, "")`) and settles into it.
    pub fn join(room: RoomId) -> Self {
        Self::with(Mirror::join(), room, None)
    }

    fn with(mirror: Mirror, room: RoomId, published: Option<u64>) -> Self {
        Self { mirror, room, published, unsettled: false, asked: BTreeSet::new(), arrived: BTreeMap::new(), wanted: BTreeSet::new() }
    }

    /// The first frame on each new connection. Assets asked for on an earlier one are asked
    /// for again.
    pub fn hello(&mut self) -> Frame {
        self.asked.clear();
        self.unsettled = true;
        Frame::Hello { protocol: PROTOCOL, room: self.room.clone() }
    }

    /// The project shared in the room, once its first update has arrived.
    pub fn project_id(&self) -> Result<Option<ProjectId>, Error> {
        self.mirror.project_id()
    }

    /// The session's own edits since the last publish, and any assets the others asked for
    /// that this session has.
    pub fn publish(&mut self, session: &Session) -> Result<Vec<Frame>, Error> {
        let mut out = Vec::new();
        for hash in std::mem::take(&mut self.wanted) {
            if let Some(bytes) = session.assets().get(&hash).filter(|b| b.len() <= MAX_ASSET_BYTES) {
                out.push(Frame::AssetPut { hash: *hash.as_bytes(), bytes: bytes.to_vec() });
            }
        }
        // A joiner has nothing of its own to publish until it has taken the project in.
        if self.published.is_none_or(|revision| revision == session.revision()) {
            return Ok(out);
        }
        if let Some(update) = self.mirror.publish(session.document())? {
            out.push(Frame::Update(update));
        }
        self.published = Some(session.revision());
        Ok(out)
    }

    /// Takes one frame from the relay. An error ends the sharing: the host closes the
    /// connection and tells the person why.
    pub fn receive(&mut self, frame: Frame) -> Result<Vec<Frame>, Error> {
        match frame {
            // The relay asks what this peer has when it joins: answer, and ask back.
            Frame::Step1(state_vector) => Ok(vec![Frame::Step2(self.mirror.update_for(&state_vector)?), Frame::Step1(self.mirror.state_vector()?)]),
            Frame::Step2(update) | Frame::Update(update) => {
                self.mirror.receive(&update)?;
                self.unsettled = true;
                Ok(Vec::new())
            }
            Frame::AssetPut { hash, bytes } => {
                let hash = Blake3::from_bytes(hash);
                let held: usize = self.arrived.values().map(Vec::len).sum();
                if self.asked.remove(&hash) && held.saturating_add(bytes.len()) <= MAX_ARRIVED_BYTES {
                    self.arrived.insert(hash, bytes);
                    self.unsettled = true;
                }
                Ok(Vec::new())
            }
            Frame::AssetGet(hashes) => {
                self.wanted.extend(hashes.into_iter().map(Blake3::from_bytes));
                Ok(Vec::new())
            }
            Frame::Refused(_) => Err(Error::new(ErrorKind::Conflict, "the relay ended the shared session")),
            Frame::Hello { .. } => Err(Error::new(ErrorKind::InvalidInput, "the relay sent a frame it doesn't send")),
        }
    }

    /// Brings `session` up to what the shared document holds, once every asset it needs has
    /// arrived. Local edits go out first, so nothing made here is lost.
    pub fn settle(&mut self, session: &mut Session) -> Result<Vec<Frame>, Error> {
        let mut out = self.publish(session)?;
        if !self.unsettled {
            return Ok(out);
        }
        let Some(Materialized { project, repairs, .. }) = self.mirror.materialize()? else { return Ok(out) };
        if let Some(repairs) = repairs {
            out.push(Frame::Update(repairs));
        }
        if project.id() != session.document().id() {
            return Err(Error::new(ErrorKind::Conflict, "this session holds a different project from the one shared"));
        }
        let mut missing = BTreeSet::new();
        for (canvas_id, canvas) in project.canvases() {
            let now = session.document().canvases().get(canvas_id);
            // A card the session already holds as it is has its assets.
            for node in canvas.nodes().values().filter(|node| now.and_then(|now| now.nodes().get(&node.id)) != Some(*node)) {
                missing.extend(assets_of(canvas.id(), node)?.into_iter().filter(|hash| !session.assets().contains(hash)));
            }
        }
        for hash in &missing {
            if let Some(bytes) = self.arrived.remove(hash) {
                session.import_asset(&bytes)?;
            }
        }
        missing.retain(|hash| !session.assets().contains(hash));
        // Ask once for each asset still missing; the cards that use one wait for it, and the
        // rest goes in now.
        let ask: Vec<[u8; 32]> = missing.iter().filter(|hash| self.asked.insert(**hash)).map(|hash| *hash.as_bytes()).collect();
        if !ask.is_empty() {
            out.push(Frame::AssetGet(ask));
        }
        let project = if missing.is_empty() { project } else { waiting_without(session.document(), project, &missing)? };
        let sets = diff::diff(session.document(), &project)?;
        self.unsettled = false;
        let applied = take(session, sets);
        // What the session now holds is what later publishes compare with. A set that didn't
        // apply, or a card still waiting, is tried again when the next update or asset comes.
        self.mirror.adopt(session.document());
        self.published = Some(session.revision());
        applied?;
        Ok(out)
    }
}

/// The assets one card names.
fn assets_of(canvas: CanvasId, node: &Node) -> Result<BTreeSet<Blake3>, Error> {
    nodecraft_format::assets_in_changes(&Changeset::new(vec![Change::AddNode { canvas, node: node.clone() }]))
}

/// `target` with each card that names a missing asset as `current` has it, or without it if
/// `current` has no such card. A frozen canvas waits whole.
fn waiting_without(current: &Project, mut target: Project, missing: &BTreeSet<Blake3>) -> Result<Project, Error> {
    let mut holds = Vec::new();
    for (canvas_id, canvas) in target.canvases() {
        let now = current.canvases().get(canvas_id);
        for node in canvas.nodes().values() {
            if now.and_then(|now| now.nodes().get(&node.id)) == Some(node) || assets_of(*canvas_id, node)?.is_disjoint(missing) {
                continue;
            }
            if canvas.kind() == CanvasKind::Technique {
                holds.push(match now {
                    Some(now) => vec![Change::RemoveCanvas { id: *canvas_id }, Change::AddCanvas { canvas: now.clone() }],
                    None => vec![Change::RemoveCanvas { id: *canvas_id }],
                });
                break;
            }
            let remove = Change::RemoveNode { canvas: *canvas_id, id: node.id };
            holds.push(match now.and_then(|now| now.nodes().get(&node.id)) {
                Some(now) => vec![Change::SetNode { canvas: *canvas_id, node: now.clone() }],
                None => vec![remove],
            });
        }
    }
    for hold in holds {
        // The card as it was may not fit the wires it now has: then it waits out of the canvas.
        let fallback = match hold.first() {
            Some(Change::SetNode { canvas, node }) => Some(Change::RemoveNode { canvas: *canvas, id: node.id }),
            _ => None,
        };
        if Changeset::new(hold).apply(&mut target).is_err() {
            match fallback {
                Some(fallback) => {
                    Changeset::new(vec![fallback]).apply(&mut target)?;
                }
                None => return Err(Error::new(ErrorKind::Conflict, "a shared technique can't wait for its assets")),
            }
        }
    }
    Ok(target)
}

fn take(session: &mut Session, sets: Vec<Changeset>) -> Result<(), Error> {
    for set in sets {
        session.apply_remote(set)?;
    }
    Ok(())
}

/// The project a joiner opens its session with: empty, with the shared project's id.
pub fn joining(id: ProjectId) -> Result<Project, Error> {
    Project::new(id, "")
}

#[cfg(test)]
mod tests;
