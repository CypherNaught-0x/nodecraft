//! A session shared live with other people (#119): the session's edits go out to a shared
//! document, and what the others did comes back in as ordinary changesets. The host moves
//! frames between this and its connection to the relay; nothing here touches a socket.
//!
//! Each turn the host calls [`Collab::publish`] after local edits, [`Collab::receive`] for each
//! frame from the relay, and [`Collab::settle`] before drawing, sending whatever each returns.
//!
//! Whoever comes in by the room id is its owner: they edit, and make and take back share links
//! (#121, `links`). Whoever comes in through a link takes its role, which [`Collab::settle`]
//! gives the session ([`crate::access::Access`]): an editor writes the shared document as the
//! owner does; a viewer writes nothing; a commenter sends each change to a discussion to the
//! relay, which checks it is theirs to make and writes it for them.
use std::collections::{BTreeMap, BTreeSet};

use nodecraft_graph::{CanvasKind, Change, Changeset, Node, Project, diff};
use nodecraft_sync::{CommentChange, Frame, Grant, LinkKey, MAX_ASSET_BYTES, Materialized, Mirror, PROTOCOL, Role, RoomId};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, ProjectId};

use crate::access::Access;
use crate::session::Session;

mod links;

pub use links::LINKS_SERVICE;

/// The most assets kept waiting for the edit that needs them.
const MAX_ARRIVED_BYTES: usize = 256 << 20;
/// The most discussion changes waiting for the relay before the commenter's next ones wait too.
const MAX_PENDING: usize = 256;
/// The most of a relay's reason shown to the person.
const MAX_REASON_BYTES: usize = 200;

/// How this session comes into the room.
enum Entry {
    Room(RoomId),
    Link(LinkKey),
}

pub struct Collab {
    mirror: Mirror,
    entry: Entry,
    /// What this session may do in the room: the owner's from the start, a link's once the
    /// relay has said what it is.
    role: Option<Role>,
    /// The name a link was made for: whose discussions this session writes.
    name: Option<String>,
    /// The session revision last published; a later one may hold local edits.
    published: Option<u64>,
    /// Something came in that the session hasn't taken yet.
    unsettled: bool,
    /// Assets asked of the others, and those that arrived before the edit that needs them.
    asked: BTreeSet<Blake3>,
    arrived: BTreeMap<Blake3, Vec<u8>>,
    /// Assets the others asked for, answered at the next publish.
    wanted: BTreeSet<Blake3>,
    /// The owner's share links, by hash, and those taken back. The relay keeps links only while
    /// the room is open, so each connection sends them again.
    links: BTreeMap<[u8; 32], Grant>,
    revoked: BTreeSet<[u8; 32]>,
    /// Link frames for the next publish.
    outbox: Vec<Frame>,
    /// A commenter's discussion changes the relay hasn't answered yet, oldest first. The
    /// session shows them as made until the relay's update holding them arrives.
    pending: Vec<(u64, CommentChange)>,
    sent_pending: bool,
    seq: u64,
}

impl Collab {
    /// Shares `session`'s project in `room`. The relay asks for it when the connection opens.
    pub fn share(session: &Session, room: RoomId) -> Result<Self, Error> {
        let (mirror, _) = Mirror::share(session.document())?;
        Ok(Self::with(mirror, Entry::Room(room), Some(session.revision())))
    }

    /// Joins `room` as its owner. Once [`Self::project_id`] knows the project, the host opens
    /// an empty session for it ([`joining`]) and settles into it.
    pub fn join(room: RoomId) -> Self {
        Self::with(Mirror::join(), Entry::Room(room), None)
    }

    /// Joins the room a share link opens, in the link's role, as [`Self::join`] does.
    pub fn join_link(link: LinkKey) -> Self {
        Self::with(Mirror::join(), Entry::Link(link), None)
    }

    fn with(mirror: Mirror, entry: Entry, published: Option<u64>) -> Self {
        let role = matches!(entry, Entry::Room(_)).then_some(Role::Owner);
        Self {
            mirror,
            entry,
            role,
            name: None,
            published,
            unsettled: false,
            asked: BTreeSet::new(),
            arrived: BTreeMap::new(),
            wanted: BTreeSet::new(),
            links: BTreeMap::new(),
            revoked: BTreeSet::new(),
            outbox: Vec::new(),
            pending: Vec::new(),
            sent_pending: false,
            seq: 0,
        }
    }

    /// What this session may do in the room; `None` until the relay has said what a link
    /// lets it do.
    pub fn role(&self) -> Option<Role> {
        self.role
    }

    /// The first frame on each new connection. Assets asked for on an earlier one are asked
    /// for again, the owner's links are sent again, and a commenter's unanswered changes go
    /// again once the relay has welcomed it.
    pub fn hello(&mut self) -> Frame {
        self.asked.clear();
        self.unsettled = true;
        self.sent_pending = false;
        match &self.entry {
            Entry::Room(room) => {
                self.outbox = self.revoked.iter().map(|hash| Frame::LinkRevoke(*hash)).collect();
                self.outbox.extend(self.links.iter().map(|(hash, grant)| Frame::LinkPut { hash: *hash, grant: grant.clone() }));
                Frame::Hello { protocol: PROTOCOL, room: room.clone() }
            }
            Entry::Link(link) => {
                // The link's terms may have changed while away.
                self.role = None;
                Frame::HelloLink { protocol: PROTOCOL, link: link.clone() }
            }
        }
    }

    /// The project shared in the room, once its first update has arrived.
    pub fn project_id(&self) -> Result<Option<ProjectId>, Error> {
        self.mirror.project_id()
    }

    /// The session's own edits since the last publish, and any assets the others asked for
    /// that this session has. A commenter's are its changes to discussions; a viewer has none.
    pub fn publish(&mut self, session: &Session) -> Result<Vec<Frame>, Error> {
        let mut out = std::mem::take(&mut self.outbox);
        let Some(role) = self.role else { return Ok(out) };
        if !role.edits() {
            self.wanted.clear();
        }
        for hash in std::mem::take(&mut self.wanted) {
            if let Some(bytes) = session.assets().get(&hash).filter(|b| b.len() <= MAX_ASSET_BYTES) {
                out.push(Frame::AssetPut { hash: *hash.as_bytes(), bytes: bytes.to_vec() });
            }
        }
        if role == Role::Comment && !self.sent_pending {
            out.extend(self.pending.iter().map(|(seq, change)| put(*seq, change)));
            self.sent_pending = true;
        }
        // A joiner has nothing of its own to publish until it has taken the project in.
        if self.published.is_none_or(|revision| revision == session.revision()) {
            return Ok(out);
        }
        match role {
            Role::Edit | Role::Owner => {
                if let Some(update) = self.mirror.publish(session.document())? {
                    out.push(Frame::Update(update));
                }
            }
            Role::Comment => {
                // Too many unanswered: the relay is slow or gone. These wait for the next turn.
                if self.pending.len() >= MAX_PENDING {
                    return Ok(out);
                }
                for change in self.mirror.comments_since(session.document())? {
                    self.seq = self.seq.saturating_add(1);
                    out.push(put(self.seq, &change));
                    self.pending.push((self.seq, change));
                }
            }
            // The session refuses a viewer's edits; the shared document stays as it is.
            Role::View => {}
        }
        self.published = Some(session.revision());
        Ok(out)
    }

    /// Takes one frame from the relay. An error ends the sharing: the host closes the
    /// connection and tells the person why.
    pub fn receive(&mut self, frame: Frame) -> Result<Vec<Frame>, Error> {
        match frame {
            // The relay asks what this peer has when it joins: answer, and ask back. Only an
            // editor's document is asked for.
            Frame::Step1(state_vector) if self.role.is_some_and(Role::edits) => {
                Ok(vec![Frame::Step2(self.mirror.update_for(&state_vector)?), Frame::Step1(self.mirror.state_vector()?)])
            }
            Frame::Step2(update) | Frame::Update(update) if self.role.is_some() => {
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
                if self.role.is_some_and(Role::edits) {
                    self.wanted.extend(hashes.into_iter().map(Blake3::from_bytes));
                }
                Ok(Vec::new())
            }
            Frame::Welcome { role, name } if matches!(self.entry, Entry::Link(_)) && self.role.is_none() && role != Role::Owner => {
                if let Some(name) = &name {
                    nodecraft_graph::validate_author(name)?;
                }
                if role == Role::Comment && name.is_none() {
                    return Err(Error::new(ErrorKind::InvalidInput, "the relay let this session comment without a name"));
                }
                self.role = Some(role);
                self.name = name;
                self.unsettled = true;
                // Nobody asks a viewer or commenter for its document: it asks for the project.
                if role.edits() { Ok(Vec::new()) } else { Ok(vec![Frame::Step1(self.mirror.state_vector()?)]) }
            }
            Frame::CommentDone(seq) if self.role == Some(Role::Comment) => {
                self.pending.retain(|(sent, _)| *sent > seq);
                self.unsettled = true;
                Ok(Vec::new())
            }
            Frame::Refused(reason) => Err(Error::new(ErrorKind::Conflict, format!("the relay ended the shared session: {}", shown(&reason)))),
            Frame::Step1(_)
            | Frame::Step2(_)
            | Frame::Update(_)
            | Frame::Welcome { .. }
            | Frame::CommentDone(_)
            | Frame::Hello { .. }
            | Frame::HelloLink { .. }
            | Frame::LinkPut { .. }
            | Frame::LinkRevoke(_)
            | Frame::CommentPut { .. } => Err(Error::new(ErrorKind::InvalidInput, "the relay sent a frame it doesn't send")),
        }
    }

    /// Brings `session` up to what the shared document holds, once every asset it needs has
    /// arrived, and gives it the role the room lets it have. Local edits go out first, so
    /// nothing made here is lost.
    pub fn settle(&mut self, session: &mut Session) -> Result<Vec<Frame>, Error> {
        let mut out = self.publish(session)?;
        let Some(role) = self.role else { return Ok(out) };
        session.set_access(match role {
            Role::Owner => Access::owner(),
            role => Access::new(role, self.name.clone())?,
        });
        if !self.unsettled {
            return Ok(out);
        }
        let Some(Materialized { mut project, repairs, .. }) = self.mirror.materialize()? else { return Ok(out) };
        // Only an editor writes the shared document; the others' copies take what an editor's
        // repair brings.
        if let Some(repairs) = repairs.filter(|_| role.edits()) {
            out.push(Frame::Update(repairs));
        }
        if project.id() != session.document().id() {
            return Err(Error::new(ErrorKind::Conflict, "this session holds a different project from the one shared"));
        }
        for (_, change) in &self.pending {
            project = overlaid(project, change);
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

/// A commenter's change to one discussion, for the relay.
fn put(seq: u64, change: &CommentChange) -> Frame {
    Frame::CommentPut { seq, canvas: change.canvas, id: change.id, base: change.base.clone().map(Box::new), comment: change.made.clone().map(Box::new) }
}

/// `project` with a commenter's change it doesn't hold yet, if it still fits.
fn overlaid(project: Project, change: &CommentChange) -> Project {
    let set = match &change.made {
        Some(comment) => Change::PutComment { canvas: change.canvas, comment: comment.clone() },
        None if project.canvases().get(&change.canvas).is_some_and(|c| c.comments().contains_key(&change.id)) => {
            Change::RemoveComment { canvas: change.canvas, id: change.id }
        }
        None => return project,
    };
    let mut next = project.clone();
    if Changeset::new(vec![set]).apply(&mut next).is_ok() { next } else { project }
}

/// A relay's reason, as the person may read it: one line, not too long.
fn shown(reason: &str) -> String {
    let mut text: String = reason.chars().filter(|c| !c.is_control()).collect();
    if text.len() > MAX_REASON_BYTES {
        let mut cut = MAX_REASON_BYTES;
        while !text.is_char_boundary(cut) {
            cut = cut.saturating_sub(1);
        }
        text.truncate(cut);
    }
    text
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
