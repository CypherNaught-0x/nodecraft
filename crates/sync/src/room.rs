//! One shared project at the relay: the merged document, so a late joiner gets all of it, and the
//! assets its cards use, passed from the peers that have them to the peers that ask. The room
//! never reads the project; it only merges and forwards, except for a commenter's discussion
//! changes, which it checks and writes itself (#121). Every byte it keeps is capped.
//!
//! An asset too large for one frame comes in parts (#700), which the room passes on without
//! keeping: each asker takes the parts of one sender, in order, and when that sender leaves the
//! room asks the others for the rest.
//!
//! Whoever joins by the room id is its owner. The owner may hand out share links, each with one
//! role ([`Role`]); the room keeps only each link's hash. A viewer or commenter is never asked
//! for its document, and anything it sends that would change the project is refused.
//!
//! Each peer, whatever its role, may say where it is ([`Presence`], #120); the room keeps the
//! latest, passes it on to the others with the peer's name, tells a newcomer where everyone
//! already is, and tells the rest when a peer leaves.
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use nodecraft_graph::Comment;
use nodecraft_types::{CanvasId, CommentId};

use crate::discussion;
use crate::frame::{Frame, MAX_UPDATE_BYTES, PART_BYTES, WINDOW_BYTES};
use crate::mirror::{comment_in, open_canvas_in, write_comment};
use crate::presence::{OWNER_NAME, Person, Presence};
use crate::role::{Grant, Role};

/// One connection, numbered by the relay.
pub type Peer = u64;

/// The most a room merges in all, so one room can't fill the relay's memory.
const MAX_ROOM_UPDATE_BYTES: usize = 512 << 20;
/// The most asset bytes a room keeps.
const MAX_ROOM_ASSET_BYTES: usize = 512 << 20;
/// The most peers in one room.
pub const MAX_PEERS: usize = 32;
/// The most assets asked for and not yet sent; later asks wait until some arrive.
const MAX_WANTED: usize = 4096;
/// The most share links one room holds.
pub const MAX_LINKS: usize = 256;

/// One asker's wait for an asset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Want {
    /// The next byte it takes of an asset coming in parts.
    next: u64,
    /// The end of what it has asked for.
    until: u64,
    /// The asset's size and who sends it, from the first part it took.
    total: Option<u64>,
    source: Option<Peer>,
    /// The sender whose copy the asker said was bad ([`Frame::AssetBad`]): its parts no longer
    /// reach this asker, and it isn't asked on the asker's behalf.
    shunned: Option<Peer>,
}

impl Want {
    /// Asks for what this asker still waits on, of whoever has it, once its sender has gone;
    /// nothing if all it asked for came (its next ask goes to everyone).
    fn again(&self, hash: [u8; 32]) -> Option<Frame> {
        let mut len = self.until.saturating_sub(self.next);
        if let Some(left) = self.total.map(|total| total.saturating_sub(self.next)) {
            len = len.min(left);
        }
        (len > 0).then_some(Frame::AssetFrom { hash, offset: self.next, len })
    }
}

/// What the relay should do after a frame.
#[derive(Debug, PartialEq)]
pub enum Out {
    Send(Peer, Frame),
    /// Send the frame, then hang up on the peer.
    Close(Peer, Frame),
}

/// Who a peer is in the room: what it may do, and through which link it came in.
#[derive(Clone, Debug, PartialEq)]
struct Member {
    role: Role,
    link: Option<[u8; 32]>,
    name: Option<String>,
    /// Where the peer last said it is.
    presence: Option<Presence>,
}

pub struct Room {
    doc: Doc,
    peers: BTreeMap<Peer, Member>,
    links: BTreeMap<[u8; 32], Grant>,
    /// The bytes the document holds, as last measured, plus what was merged since.
    merged: usize,
    /// `merged` when the document was last measured.
    measured: usize,
    /// The most the document may hold ([`MAX_ROOM_UPDATE_BYTES`]).
    limit: usize,
    assets: BTreeMap<[u8; 32], Vec<u8>>,
    asset_bytes: usize,
    /// Who waits for an asset, and how far each has got with one coming in parts.
    wanted: BTreeMap<[u8; 32], BTreeMap<Peer, Want>>,
    /// Who sent each asker the whole of an asset that came in parts, so that a copy the asker
    /// says is bad ([`Frame::AssetBad`]) is asked of the others; numbered in the order they came,
    /// so past [`MAX_WANTED`] the oldest goes.
    sent: BTreeMap<([u8; 32], Peer), (u64, Peer)>,
    sent_seq: u64,
    damaged: bool,
}

impl Default for Room {
    fn default() -> Self {
        Self::new()
    }
}

impl Room {
    pub fn new() -> Self {
        Self {
            doc: Doc::new(),
            peers: BTreeMap::new(),
            links: BTreeMap::new(),
            merged: 0,
            measured: 0,
            limit: MAX_ROOM_UPDATE_BYTES,
            assets: BTreeMap::new(),
            asset_bytes: 0,
            wanted: BTreeMap::new(),
            sent: BTreeMap::new(),
            sent_seq: 0,
            damaged: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// The bytes the room has taken in: merged updates and kept assets.
    pub fn held(&self) -> usize {
        self.merged.saturating_add(self.asset_bytes)
    }

    /// The terms of the link with this hash, if the room has it.
    pub fn grant(&self, link: &[u8; 32]) -> Option<&Grant> {
        self.links.get(link)
    }

    /// Lets `peer` in as the owner, and asks what it has: its answer brings in what it changed
    /// while away.
    pub fn join(&mut self, peer: Peer) -> Vec<Out> {
        self.enter(peer, Member { role: Role::Owner, link: None, name: None, presence: None })
    }

    /// Lets `peer` in through the link with this hash, if the room has it and it is still good
    /// at `now` (seconds since 1970). The peer is told what the link lets it do.
    pub fn join_link(&mut self, peer: Peer, link: [u8; 32], now: u64) -> Vec<Out> {
        let Some(grant) = self.links.get(&link).filter(|grant| grant.live(now)) else {
            return vec![Out::Close(peer, Frame::Refused(NO_LINK.into()))];
        };
        let (role, name) = (grant.role, grant.name.clone());
        let mut out = vec![Out::Send(peer, Frame::Welcome { role, name: Some(name.clone()) })];
        let entered = self.enter(peer, Member { role, link: Some(link), name: Some(name), presence: None });
        if entered.iter().any(|o| matches!(o, Out::Close(..))) {
            return entered;
        }
        out.extend(entered);
        out
    }

    fn enter(&mut self, peer: Peer, member: Member) -> Vec<Out> {
        if self.peers.len() >= MAX_PEERS {
            return vec![Out::Close(peer, Frame::Refused("the room is full".into()))];
        }
        match self.state_vector() {
            Some(sv) => {
                let edits = member.role.edits();
                self.peers.insert(peer, member);
                // Where everyone already is.
                let mut out: Vec<Out> = self
                    .peers
                    .iter()
                    .filter(|(other, _)| **other != peer)
                    .filter_map(|(other, member)| Some(Out::Send(peer, Frame::Person(Box::new(person(*other, member)?)))))
                    .collect();
                // Only an editor's document comes in; anyone else asks for the project itself.
                if !edits {
                    return out;
                }
                let at = out.len();
                out.push(Out::Send(peer, Frame::Step1(sv)));
                // Whoever asked for an asset may be waiting on someone who left, or on nobody
                // yet: the newcomer may have it.
                let mut whole = Vec::new();
                for (hash, askers) in &self.wanted {
                    let mut rest: Vec<Frame> =
                        askers.values().filter(|want| want.source.is_none() && want.next > 0).filter_map(|want| want.again(*hash)).collect();
                    rest.dedup();
                    if askers.values().any(|want| want.source.is_none() && want.next == 0) {
                        whole.push(*hash);
                    }
                    out.extend(rest.into_iter().map(|frame| Out::Send(peer, frame)));
                }
                if !whole.is_empty() {
                    out.insert(at + 1, Out::Send(peer, Frame::AssetGet(whole)));
                }
                out
            }
            None => vec![Out::Close(peer, Frame::Refused(DAMAGED.into()))],
        }
    }

    /// Lets `peer` go. Whoever was taking an asset's parts from it is sent the rest by another
    /// editor who has it, if one answers what this returns; if it had said where it was, the
    /// others are told it has gone.
    #[must_use]
    pub fn leave(&mut self, peer: Peer) -> Vec<Out> {
        let shown = self.peers.remove(&peer).is_some_and(|member| member.presence.is_some());
        let mut again = Vec::new();
        for (hash, askers) in &mut self.wanted {
            askers.remove(&peer);
            for (asker, want) in askers.iter_mut().filter(|(_, want)| want.source == Some(peer)) {
                want.source = None;
                again.extend(want.again(*hash).map(|frame| (*asker, want.shunned, frame)));
            }
        }
        self.wanted.retain(|_, askers| !askers.is_empty());
        self.sent.retain(|(_, asker), _| *asker != peer);
        let mut out = Vec::new();
        for (asker, shunned, frame) in again {
            for editor in self.editors_but(asker).into_iter().filter(|editor| Some(*editor) != shunned) {
                let ask = Out::Send(editor, frame.clone());
                if !out.contains(&ask) {
                    out.push(ask);
                }
            }
        }
        if shown {
            out.extend(self.others(peer).map(|other| Out::Send(other, Frame::Gone(peer))));
        }
        out
    }

    /// Puts out everyone whose link has run out by `now` (seconds since 1970), and forgets the
    /// links that have.
    pub fn expire(&mut self, now: u64) -> Vec<Out> {
        let gone: Vec<[u8; 32]> = self.links.iter().filter(|(_, grant)| !grant.live(now)).map(|(hash, _)| *hash).collect();
        let mut out = Vec::new();
        for hash in gone {
            self.links.remove(&hash);
            out.extend(self.put_out(hash, "the share link has expired"));
        }
        out
    }

    /// Hangs up on everyone in through `link`.
    fn put_out(&mut self, link: [u8; 32], why: &str) -> Vec<Out> {
        let through: Vec<Peer> = self.peers.iter().filter(|(_, member)| member.link == Some(link)).map(|(peer, _)| *peer).collect();
        let mut out = Vec::new();
        let mut again = Vec::new();
        for peer in through {
            again.extend(self.leave(peer));
            out.push(Out::Close(peer, Frame::Refused(why.into())));
        }
        // Those put out aren't asked for anything, nor told who else has gone.
        out.extend(again.into_iter().filter(|o| !matches!(o, Out::Send(peer, _) if !self.peers.contains_key(peer))));
        out
    }

    /// The role `peer` is in the room with, if it is in it.
    pub fn role_of(&self, peer: Peer) -> Option<Role> {
        self.peers.get(&peer).map(|member| member.role)
    }

    /// Takes one frame from `from`, a peer in the room.
    pub fn handle(&mut self, from: Peer, frame: Frame) -> Vec<Out> {
        let Some(member) = self.peers.get(&from).cloned() else {
            return vec![Out::Close(from, Frame::Refused("not in the room".into()))];
        };
        match frame {
            Frame::Step1(sv) => match self.update_for(&sv) {
                Some(update) if update.len() > MAX_UPDATE_BYTES => self.refuse(from, "the project is too large to send"),
                Some(update) => vec![Out::Send(from, Frame::Step2(update))],
                None => self.refuse(from, "the state vector isn't readable"),
            },
            Frame::Step2(_) | Frame::Update(_) if !member.role.edits() => self.refuse(from, READ_ONLY),
            Frame::Step2(update) | Frame::Update(update) => {
                if !self.fits(update.len()) {
                    return self.refuse(from, "the room has grown too large");
                }
                if let Err(message) = self.merge(&update) {
                    return self.refuse(from, message);
                }
                self.merged = self.merged.saturating_add(update.len());
                self.others(from).map(|peer| Out::Send(peer, Frame::Update(update.clone()))).collect()
            }
            // A viewer's or commenter's media goes nowhere: it can't have added any.
            Frame::AssetPut { .. } if !member.role.edits() => Vec::new(),
            Frame::AssetPut { hash, bytes } => {
                // The frame checked the hash when it was read.
                let out: Vec<Out> = self
                    .wanted
                    .remove(&hash)
                    .unwrap_or_default()
                    .into_keys()
                    .filter(|peer| *peer != from && self.peers.contains_key(peer))
                    .map(|peer| Out::Send(peer, Frame::AssetPut { hash, bytes: bytes.clone() }))
                    .collect();
                if !self.assets.contains_key(&hash) && self.asset_bytes.saturating_add(bytes.len()) <= MAX_ROOM_ASSET_BYTES {
                    self.asset_bytes = self.asset_bytes.saturating_add(bytes.len());
                    self.assets.insert(hash, bytes);
                }
                // Past what the room keeps, nobody else gets it from the room; whoever asks
                // later is sent to an editor who has it.
                out
            }
            Frame::AssetGet(hashes) => {
                let mut out = Vec::new();
                let mut missing = Vec::new();
                for hash in hashes {
                    match self.assets.get(&hash) {
                        Some(bytes) => out.push(Out::Send(from, Frame::AssetPut { hash, bytes: bytes.clone() })),
                        None if self.wanted.len() < MAX_WANTED || self.wanted.contains_key(&hash) => {
                            // Asked for whole, it starts again: one in parts comes first as its
                            // first part, from whoever sends it first, whatever came before.
                            let askers = self.wanted.entry(hash).or_default();
                            let shunned = askers.get(&from).and_then(|want| want.shunned);
                            askers.insert(from, Want { until: PART_BYTES as u64, shunned, ..Want::default() });
                            missing.push(hash);
                        }
                        None => {}
                    }
                }
                if !missing.is_empty() {
                    out.extend(self.editors_but(from).into_iter().map(|peer| Out::Send(peer, Frame::AssetGet(missing.clone()))));
                }
                out
            }
            Frame::AssetFrom { hash, offset, len } => {
                let len = len.min(WINDOW_BYTES);
                let end = offset.saturating_add(len);
                match self.wanted.get_mut(&hash).and_then(|askers| askers.get_mut(&from)) {
                    // An asker goes on from the end of what it asked for, at most two windows
                    // ahead of what it has; any other ask, as a flood would be, isn't passed on.
                    Some(want) => {
                        if offset != want.until || end.saturating_sub(want.next) > 2 * WINDOW_BYTES {
                            return Vec::new();
                        }
                        want.until = end;
                    }
                    // A first ask, on a new connection, starts where it says.
                    None => {
                        if self.wanted.len() >= MAX_WANTED && !self.wanted.contains_key(&hash) {
                            return Vec::new();
                        }
                        self.wanted.entry(hash).or_default().insert(from, Want { next: offset, until: end, ..Want::default() });
                    }
                }
                let Some(want) = self.wanted.get(&hash).and_then(|askers| askers.get(&from)) else { return Vec::new() };
                let ask = Frame::AssetFrom { hash, offset, len };
                // The rest comes from whoever sends the parts already taken, while they are here.
                match want.source.filter(|source| self.peers.contains_key(source)) {
                    Some(source) => vec![Out::Send(source, ask)],
                    None => self.editors_but(from).into_iter().filter(|peer| Some(*peer) != want.shunned).map(|peer| Out::Send(peer, ask.clone())).collect(),
                }
            }
            Frame::AssetBad(hash) => {
                // Who sent the copy: the sender of the parts being taken, or of the whole that
                // came. One that has left, or been forgotten, can't be told apart from the rest:
                // everyone is asked, and the asker gives up on a second bad copy.
                let taking = self.wanted.get(&hash).and_then(|askers| askers.get(&from)).and_then(|want| want.source);
                let shunned = taking.or(self.sent.remove(&(hash, from)).map(|(_, source)| source));
                let others: Vec<Peer> = self.editors_but(from).into_iter().filter(|peer| Some(*peer) != shunned).collect();
                if others.is_empty() || (self.wanted.len() >= MAX_WANTED && !self.wanted.contains_key(&hash)) {
                    // Nobody else to ask: the asker is told, and gives the asset up.
                    if let Some(askers) = self.wanted.get_mut(&hash) {
                        askers.remove(&from);
                        if askers.is_empty() {
                            self.wanted.remove(&hash);
                        }
                    }
                    return vec![Out::Send(from, Frame::AssetBad(hash))];
                }
                self.wanted.entry(hash).or_default().insert(from, Want { until: PART_BYTES as u64, shunned, ..Want::default() });
                // The asset again from the start, from everyone else who may have it.
                others.into_iter().map(|peer| Out::Send(peer, Frame::AssetGet(vec![hash]))).collect()
            }
            Frame::AssetPart { .. } if !member.role.edits() => Vec::new(),
            Frame::AssetPart { hash, total, offset, bytes } => {
                // The frame checked the part lies inside its asset; the asker checks the hash.
                let end = offset.saturating_add(bytes.len() as u64);
                let mut out = Vec::new();
                let mut whole = Vec::new();
                if let Some(askers) = self.wanted.get_mut(&hash) {
                    askers.retain(|peer, want| {
                        // Each asker takes one sender's parts, in order; any other copy is dropped.
                        let takes = *peer != from
                            && want.next == offset
                            && want.source.is_none_or(|s| s == from)
                            && want.shunned != Some(from)
                            && want.total.is_none_or(|t| t == total);
                        if takes {
                            out.push(Out::Send(*peer, Frame::AssetPart { hash, total, offset, bytes: bytes.clone() }));
                            *want = Want { next: end, total: Some(total), source: Some(from), ..*want };
                        }
                        let waits = want.total.is_none_or(|total| want.next < total);
                        if !waits {
                            whole.push(*peer);
                        }
                        waits
                    });
                    if askers.is_empty() {
                        self.wanted.remove(&hash);
                    }
                }
                for asker in whole {
                    if self.sent.len() >= MAX_WANTED
                        && let Some(oldest) = self.sent.iter().min_by_key(|(_, (seq, _))| *seq).map(|(key, _)| *key)
                    {
                        self.sent.remove(&oldest);
                    }
                    self.sent_seq = self.sent_seq.saturating_add(1);
                    self.sent.insert((hash, asker), (self.sent_seq, from));
                }
                out
            }
            Frame::LinkPut { .. } | Frame::LinkRevoke(_) if member.role != Role::Owner => self.refuse(from, "only the owner manages share links"),
            Frame::LinkPut { hash, grant } => {
                if grant.validate().is_err() {
                    return self.refuse(from, "the share link isn't valid");
                }
                // A link past the most a room holds isn't kept. Its owner's session holds no more
                // than that itself; only two devices of the owner's together can go over.
                if !self.links.contains_key(&hash) && self.links.len() >= MAX_LINKS {
                    return Vec::new();
                }
                // Whoever is in through a link that changed comes back in on its new terms.
                let changed = self.links.insert(hash, grant.clone()).is_some_and(|old| old != grant);
                if changed { self.put_out(hash, "the share link changed: join again") } else { Vec::new() }
            }
            Frame::LinkRevoke(hash) => {
                self.links.remove(&hash);
                self.put_out(hash, "the share link was revoked")
            }
            Frame::CommentPut { .. } if member.role != Role::Comment => self.refuse(from, "editors change discussions in the document"),
            Frame::CommentPut { seq, canvas, id, base, comment } => {
                let Some(name) = member.name else { return self.refuse(from, NO_LINK) };
                match self.discuss(&name, canvas, id, base.as_deref(), comment.as_deref()) {
                    Ok(update) => {
                        let mut out: Vec<Out> =
                            update.map(|update| self.peers.keys().map(|peer| Out::Send(*peer, Frame::Update(update.clone()))).collect()).unwrap_or_default();
                        out.push(Out::Send(from, Frame::CommentDone { seq, refused: None }));
                        out
                    }
                    Err(DAMAGED) => self.refuse(from, DAMAGED),
                    // A change that isn't theirs to make is dropped; the commenter stays. An undo
                    // or a reconnect can send one in good faith.
                    Err(message) => vec![Out::Send(from, Frame::CommentDone { seq, refused: Some(message.into()) })],
                }
            }
            Frame::Presence(presence) => {
                if presence.validate().is_err() {
                    return self.refuse(from, "the presence isn't readable");
                }
                let Some(member) = self.peers.get_mut(&from) else { return Vec::new() };
                if member.presence.as_ref() == Some(&presence) {
                    return Vec::new();
                }
                member.presence = Some(presence);
                let Some(person) = self.peers.get(&from).and_then(|member| person(from, member)) else { return Vec::new() };
                self.others(from).map(|peer| Out::Send(peer, Frame::Person(Box::new(person.clone())))).collect()
            }
            Frame::Hello { .. }
            | Frame::HelloLink { .. }
            | Frame::Welcome { .. }
            | Frame::CommentDone { .. }
            | Frame::Refused(_)
            | Frame::Person(_)
            | Frame::Gone(_) => self.refuse(from, "a frame a peer doesn't send"),
        }
    }

    /// Applies a commenter's change to one discussion, as the update that writes it (`None`
    /// when it changes nothing now).
    fn discuss(
        &mut self,
        name: &str,
        canvas: CanvasId,
        id: CommentId,
        base: Option<&Comment>,
        made: Option<&Comment>,
    ) -> Result<Option<Vec<u8>>, &'static str> {
        if self.damaged {
            return Err(DAMAGED);
        }
        let (now, open) = {
            let txn = self.doc.try_transact().map_err(|_| "the room is busy")?;
            (comment_in(&txn, canvas, id), open_canvas_in(&txn, canvas))
        };
        // Someone removed the canvas (or froze it into a technique) meanwhile: the change has
        // nowhere to go, and the commenter learns so from the update that did it.
        if !open {
            return Ok(None);
        }
        let Some(next) = discussion::merge(name, now.as_ref(), base, made)? else { return Ok(None) };
        let size = next.as_ref().map_or(0, |c| c.text.len().saturating_add(c.replies.iter().map(|r| r.text.len()).sum()));
        if !self.fits(size) {
            return Err("the room has grown too large");
        }
        let doc = &self.doc;
        let written = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<u8>, &'static str> {
            let mut txn = doc.try_transact_mut().map_err(|_| "the room is busy")?;
            write_comment(&mut txn, canvas, id, next.as_ref()).map_err(|_| "the discussion can't be written")?;
            Ok(txn.encode_update_v1())
        }));
        let update = written.unwrap_or_else(|_| {
            self.damaged = true;
            Err(DAMAGED)
        })?;
        self.merged = self.merged.saturating_add(update.len());
        Ok(Some(update))
    }

    /// Whether `more` bytes fit in the document. What was merged counts in full until the room
    /// measures what the document really holds: content replaced or removed is collected as it
    /// goes, so a long session of edits needn't fill the room (#701).
    fn fits(&mut self, more: usize) -> bool {
        if self.merged.saturating_add(more) <= self.limit {
            return true;
        }
        self.settle();
        self.merged.saturating_add(more) <= self.limit
    }

    /// Measures what the document really holds, if enough came in since it was last measured:
    /// an eighth of the limit, or half of what was left then, so a room measured close to full
    /// is measured again before it refuses. Measuring is a pass over the whole document, as
    /// catching up a newcomer is. Returns whether the count went down.
    pub fn settle(&mut self) -> bool {
        let headroom = self.limit.saturating_sub(self.measured);
        if self.merged.saturating_sub(self.measured) < (self.limit / 8).min(headroom / 2).max(1) {
            return false;
        }
        let Some(size) = self.size() else { return false };
        let shrank = size < self.merged;
        self.merged = size;
        self.measured = size;
        shrank
    }

    /// The bytes of the document as one update.
    fn size(&self) -> Option<usize> {
        if self.damaged {
            return None;
        }
        Some(self.doc.try_transact().ok()?.encode_state_as_update_v1(&StateVector::default()).len())
    }

    fn others(&self, from: Peer) -> impl Iterator<Item = Peer> + '_ {
        self.peers.keys().copied().filter(move |peer| *peer != from)
    }

    /// The editors other than `from`: only an editor can have media the project needs.
    fn editors_but(&self, from: Peer) -> Vec<Peer> {
        self.others(from).filter(|peer| self.peers.get(peer).is_some_and(|m| m.role.edits())).collect()
    }

    fn refuse(&mut self, peer: Peer, message: &str) -> Vec<Out> {
        let again = self.leave(peer);
        let mut out = vec![Out::Close(peer, Frame::Refused(message.into()))];
        out.extend(again);
        out
    }

    fn state_vector(&self) -> Option<Vec<u8>> {
        if self.damaged {
            return None;
        }
        Some(self.doc.try_transact().ok()?.state_vector().encode_v1())
    }

    fn update_for(&self, sv: &[u8]) -> Option<Vec<u8>> {
        if self.damaged {
            return None;
        }
        let sv = StateVector::decode_v1(sv).ok()?;
        Some(self.doc.try_transact().ok()?.encode_diff_v1(&sv))
    }

    fn merge(&mut self, update: &[u8]) -> Result<(), &'static str> {
        if self.damaged {
            return Err(DAMAGED);
        }
        let update = Update::decode_v1(update).map_err(|_| "the update isn't readable")?;
        let doc = &self.doc;
        let merged = catch_unwind(AssertUnwindSafe(|| {
            let mut txn = doc.try_transact_mut().map_err(|_| "the room is busy")?;
            txn.apply_update(update).map_err(|_| "the update doesn't fit the room")
        }));
        merged.unwrap_or_else(|_| {
            self.damaged = true;
            Err(DAMAGED)
        })
    }
}

/// `member` as the others see it, once it has said where it is.
fn person(peer: Peer, member: &Member) -> Option<Person> {
    let presence = member.presence.clone()?;
    Some(Person { peer, name: member.name.clone().unwrap_or_else(|| OWNER_NAME.into()), role: member.role, presence })
}

const DAMAGED: &str = "the room is damaged: share the project again";
const READ_ONLY: &str = "this share link doesn't let you edit the project";
const NO_LINK: &str = "that share link doesn't open a project being shared now";

#[cfg(test)]
mod tests;
