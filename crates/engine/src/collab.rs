//! A session shared live with other people (#119): the session's edits go out to a shared
//! document, and what the others did comes back in as ordinary changesets. The host moves
//! frames between this and its connection to the relay; nothing here touches a socket.
//!
//! Each turn the host calls [`Collab::publish`] after local edits, [`Collab::receive`] for each
//! frame from the relay, and [`Collab::settle`] before drawing, sending whatever each returns.
//!
//! Settling a large project takes a while: the shared document is read whole and compared with
//! the session's. A host that mustn't wait for it (#714) settles a copy instead: it hands
//! [`Collab::plan`] a [`View`] of the session, wherever that runs, and the session takes the
//! [`Plan`] that comes back ([`Plan::take`]) only if it hasn't changed since. [`Collab::taken`]
//! then says what came of it.
//!
//! Whoever comes in by the room id is its owner: they edit, and make and take back share links
//! (#121, `links`). Whoever comes in through a link takes its role, which [`Collab::settle`]
//! gives the session ([`crate::access::Access`]): an editor writes the shared document as the
//! owner does; a viewer writes nothing; a commenter sends each change to a discussion to the
//! relay, which checks it is theirs to make and writes it for them.
//!
//! An asset too large for one frame comes in parts into a [`Spool`] (#700): a host that can
//! write files gives the session one that keeps them on disk ([`Collab::with_spool`]).
//!
//! Each peer also says where it is ([`Collab::set_presence`], #120), and hears where the others
//! are ([`Collab::people`]); none of that touches the session.
//!
//! A canvas marked private stays out of the shared document: the room sees the project
//! without it ([`shared`]), each settle keeps it as the session has it, and no asset is given
//! out that only it uses.
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use nodecraft_format::AssetStore;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_graph::{CanvasKind, Change, Changeset, Node, Project, diff};
use nodecraft_sync::{CommentChange, Frame, LinkKey, Materialized, Mirror, PROTOCOL, Person, Presence, Role, RoomId};
use nodecraft_types::{Blake3, CanvasId, Error, ErrorKind, ProjectId};

use crate::access::Access;
use crate::session::Session;

mod join;
mod links;
mod parts;

pub use join::{JoinLink, Key, MAX_RELAY_BYTES, relay_address};
pub use links::{LINKS_SERVICE, Links};
use parts::Incoming;
pub use parts::{Arrival, MemorySpool, Spool, Stretch};

/// The most assets sent whole kept waiting for the edit that needs them.
const MAX_ARRIVED_BYTES: usize = 256 << 20;
/// The most times sealing one asset is tried while the storage is full or taken by another
/// transfer (a long save, say), before it is given up: some ten minutes at the desktop's pace.
/// One the storage is busy for waits as long as it takes.
const MAX_SEAL_TRIES: u32 = 300;
/// The most times one asset coming in parts is tried again after its spool couldn't keep it.
const MAX_SPOOL_TRIES: u32 = 8;
/// The most asks for assets kept for the next publish.
const MAX_WANTED: usize = 4096;
/// The most asks for assets answered at one publish. A stretch is at most a window, so this
/// bounds what one publish reads; the rest wait for the next ([`Collab::owes_assets`]).
const ANSWERED_PER_PUBLISH: usize = 2;
/// The most discussion changes waiting for the relay before the commenter's next ones wait too.
const MAX_PENDING: usize = 256;
/// The most other people kept track of; a relay lets fewer into a room.
const MAX_PEOPLE: usize = 64;
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
    arrived: BTreeMap<Blake3, Arrival>,
    /// Assets coming in parts, kept across connections so a new one asks only for the rest.
    incoming: BTreeMap<Blake3, Incoming>,
    spool: Box<dyn Spool>,
    /// The host seals large assets that came before a plan brings them ([`Collab::sealing`]).
    seals: bool,
    /// How often sealing each failed for a reason that may pass.
    seal_tries: BTreeMap<Blake3, u32>,
    /// The last arrival that couldn't be sealed yet: the next call starts after it, so one that
    /// waits doesn't hold up the others (#855).
    seal_waited: Option<Blake3>,
    /// Assets the spool had no room or no file for: asked for again only at
    /// [`Collab::retry_put_off`], so asking doesn't spin.
    put_off: BTreeSet<Blake3>,
    /// The size of each asset put off for want of room, so it is asked for again only once
    /// the spool has room for it.
    room_for: BTreeMap<Blake3, u64>,
    /// How often the spool failed to keep each.
    spool_tries: BTreeMap<Blake3, u32>,
    /// Assets this session can't take: too large for its spool, or not what they were named.
    /// They aren't asked for again; the cards that use them wait.
    unusable: BTreeSet<Blake3>,
    /// Assets whose copy on this connection wasn't what it was named: the relay was told, and
    /// the next copy comes from someone else. A second bad copy makes it unusable.
    reported: BTreeSet<Blake3>,
    /// What the others asked for, answered at the next publish.
    wanted: BTreeSet<(Blake3, Stretch)>,
    /// Asks the storage was too busy to read for (#855): answered again at
    /// [`Collab::retry_put_off`], so a long seal doesn't leave the asker waiting for good.
    busy_asks: BTreeSet<(Blake3, Stretch)>,
    /// The owner's share links, when this keeps them ([`Collab::links_request`]).
    links: Links,
    /// Link frames for the next publish.
    outbox: Vec<Frame>,
    /// A commenter's discussion changes the relay hasn't answered yet, oldest first. The
    /// session shows them as made until the relay's update holding them arrives.
    pending: Vec<(u64, CommentChange)>,
    sent_pending: bool,
    seq: u64,
    /// A commenter's local changes are held back until the relay answers: the session isn't
    /// brought up to the shared document meanwhile, which would undo them.
    held: bool,
    /// Why the relay last dropped one of this commenter's changes, for the person to read.
    refusal: Option<String>,
    /// The session revision at which what the room holds clashed with one of the session's
    /// private canvases; nothing is taken in until the session or the room changes again.
    clash: Option<u64>,
    /// Why, for the person to read.
    clash_said: Option<String>,
    /// What the last plan kept back of a private canvas the shared document still held, for
    /// [`Collab::taken`] to take out of it again.
    lingering: Vec<nodecraft_graph::Canvas>,
    /// Where this session last said it is, said again on each new connection.
    presence: Option<Presence>,
    /// Where the others in the room are, by peer.
    people: BTreeMap<u64, Person>,
}

/// What [`Collab::plan`] reads of a session: its document at `revision`, and which assets it
/// holds. A host settling away from the session gives it a copy.
pub struct View<'a> {
    pub revision: u64,
    pub document: &'a Project,
    pub has: &'a dyn Fn(&Blake3) -> bool,
}

/// What goes to the relay: a frame, or (some of) an asset the others asked for, which only the
/// session holds the bytes of ([`Out::frames`]).
#[derive(Debug)]
pub enum Out {
    Frame(Frame),
    Asset(Blake3, Stretch),
}

impl Out {
    /// The frames, with an asset's bytes from `assets`, the session's store or a clone of it:
    /// whole if it fits in one frame, else the parts asked for. None for an asset it can't
    /// read, or one too large to share. Its first read of an asset held only on disk checks the
    /// whole of it, so a host answers away from its window (#842).
    pub fn frames(self, assets: &AssetStore) -> Vec<Frame> {
        match self {
            Self::Frame(frame) => vec![frame],
            Self::Asset(hash, stretch) => parts::frames(assets, &hash, stretch).ok().flatten().unwrap_or_default(),
        }
    }
}

/// What a settle made for the session to take: the access the room gives it, the assets that
/// arrived, and what brings its document, as it was at [`Plan::base`], up to the shared one.
#[derive(Debug)]
pub struct Plan {
    base: u64,
    access: Option<Access>,
    imports: Vec<(Blake3, Arrival)>,
    /// `None` when nothing was settled: there is nothing to tell [`Collab::taken`].
    sets: Option<Sets>,
}

/// What brings a session's document up to the shared one.
#[derive(Debug)]
enum Sets {
    /// Changesets, applied in turn.
    Diff(Vec<Changeset>),
    /// The shared project itself, for a joiner that still holds the empty start: taken in one
    /// step, where changesets would each copy the growing document (#714).
    Whole(Box<SharedProject>),
}

/// A shared project checked where the plan was made, so [`Session::adopt_shared`] needn't walk
/// it again: valid, with the assets its cards name.
#[derive(Debug)]
pub struct SharedProject {
    pub(crate) project: Project,
    pub(crate) assets: BTreeSet<Blake3>,
}

impl SharedProject {
    fn new(project: Project) -> Result<Self, Error> {
        project.validate()?;
        let assets = assets_in(&project)?;
        Ok(Self { project, assets })
    }
}

/// What came of [`Plan::take`].
#[derive(Debug)]
pub enum Taken {
    /// Nothing was settled.
    Nothing,
    /// The session changed since its view was taken, so the changesets weren't applied; they
    /// are made again from the next view.
    Stale,
    /// The changesets were applied, or one of them failed.
    Applied(Result<(), Error>),
}

impl Plan {
    /// The session revision this plan was made at.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Whether [`Collab::taken`] must hear what came of this plan.
    pub fn settles(&self) -> bool {
        self.sets.is_some()
    }

    /// Whether taking this plan replaces the session's document ([`Session::adopt_shared`]),
    /// which gives it a new lineage.
    pub fn replaces(&self) -> bool {
        matches!(self.sets, Some(Sets::Whole(_)))
    }

    /// Gives `session` the assets, and the access and changesets if its revision is still the
    /// one the plan was made at. An error here ends the sharing.
    pub fn take(self, session: &mut Session) -> Result<Taken, Error> {
        let fresh = session.revision() == self.base;
        // A joiner may edit only once it holds the project: what it made before would be lost.
        if let Some(access) = self.access.filter(|_| fresh || self.sets.is_none()) {
            session.set_access(access);
        }
        for (hash, arrival) in self.imports {
            arrival.import(&hash, session)?;
        }
        Ok(match self.sets {
            None => Taken::Nothing,
            Some(_) if !fresh => Taken::Stale,
            Some(Sets::Diff(sets)) => Taken::Applied(take(session, sets)),
            Some(Sets::Whole(project)) => Taken::Applied(session.adopt_shared(*project)),
        })
    }
}

impl Collab {
    /// Shares `session`'s project in `room`. The relay asks for it when the connection opens.
    pub fn share(session: &Session, room: RoomId) -> Result<Self, Error> {
        Self::share_project(session.document(), session.revision(), room)
    }

    /// Shares `project`, a session's document at `revision`, as [`Self::share`] does; a host
    /// sharing away from the session (#714) shares a copy.
    pub fn share_project(project: &Project, revision: u64, room: RoomId) -> Result<Self, Error> {
        let (mirror, _) = Mirror::share(&*shared(project)?)?;
        Ok(Self::with(mirror, Entry::Room(room), Some(revision)))
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
            incoming: BTreeMap::new(),
            spool: Box::new(MemorySpool::default()),
            seals: false,
            seal_tries: BTreeMap::new(),
            seal_waited: None,
            put_off: BTreeSet::new(),
            room_for: BTreeMap::new(),
            spool_tries: BTreeMap::new(),
            unusable: BTreeSet::new(),
            reported: BTreeSet::new(),
            wanted: BTreeSet::new(),
            busy_asks: BTreeSet::new(),
            links: Links::default(),
            outbox: Vec::new(),
            pending: Vec::new(),
            sent_pending: false,
            seq: 0,
            held: false,
            refusal: None,
            clash: None,
            clash_said: None,
            lingering: Vec::new(),
            presence: None,
            people: BTreeMap::new(),
        }
    }

    /// Keeps the parts of assets coming in in `spool`, which decides how large one may be.
    pub fn with_spool(mut self, spool: Box<dyn Spool>) -> Self {
        for hash in std::mem::take(&mut self.incoming).keys() {
            self.spool.discard(hash);
        }
        self.spool = spool;
        self
    }

    /// Leaves sealing the large assets that come to the host, which calls
    /// [`Self::seal_arrivals`] away from its window before each plan (#842); a plan brings one
    /// only once it is sealed.
    pub fn sealing(mut self) -> Self {
        self.seals = true;
        self
    }

    /// Seals one of the large assets that came into `assets`' storage, a clone of the session's
    /// store, so taking it is quick: it reads it whole, so it is slow, and the host calls it
    /// once a turn so a few large assets don't hold up the others' edits (#855). `cancel` stops
    /// it, leaving the asset to seal later. One the storage is too busy or too full for is
    /// sealed at a later call ([`Self::seals_pending`]), which the host makes after a while
    /// rather than at once; one it can't keep is given up. Whether it sealed or gave up one, so
    /// another may be tried at once.
    pub fn seal_arrivals(&mut self, assets: &AssetStore, cancel: &TransferCancel) -> bool {
        // One that doesn't seal takes them in with the plan.
        if !self.seals {
            return false;
        }
        let range = |(hash, arrival): (&Blake3, &Arrival)| match arrival {
            Arrival::Range(range) => Some((*hash, range.clone())),
            _ => None,
        };
        let after = self.seal_waited.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        let next = self.arrived.range((after, std::ops::Bound::Unbounded)).find_map(range).or_else(|| self.arrived.iter().find_map(range));
        let Some((hash, range)) = next else {
            return false;
        };
        match assets.seal(&range, cancel, &StreamMetrics::default()) {
            Ok(sealed) if sealed.hash() == hash => {
                self.arrived.insert(hash, Arrival::Sealed(sealed));
                self.seal_tries.remove(&hash);
                self.unsettled = true;
                return true;
            }
            // Stopped: it waits as it was.
            Err(error) if TransferCancel::is_cancellation(&error) => {}
            Err(error) if seal_waits(error.kind, self.seal_tries.get(&hash).copied().unwrap_or(0)) => {
                if error.kind != ErrorKind::Busy {
                    let tries = self.seal_tries.entry(hash).or_insert(0);
                    *tries = tries.saturating_add(1);
                }
                self.seal_waited = Some(hash);
            }
            // Given up: the next is tried at once.
            Ok(_) | Err(_) => {
                self.arrived.remove(&hash);
                self.seal_tries.remove(&hash);
                self.unusable.insert(hash);
                return true;
            }
        }
        false
    }

    /// Whether large assets that came wait for [`Self::seal_arrivals`].
    pub fn seals_pending(&self) -> bool {
        self.arrived.values().any(|arrival| matches!(arrival, Arrival::Range(_)))
    }

    /// How many assets this session waits for before it can take in what the others did.
    pub fn waiting_for_assets(&self) -> usize {
        let sealing = self.arrived.values().filter(|arrival| matches!(arrival, Arrival::Range(_))).count();
        self.asked.len().saturating_add(self.put_off.len()).saturating_add(sealing)
    }

    /// Whether assets the spool couldn't keep, or asks the storage was too busy to answer,
    /// wait for [`Self::retry_put_off`].
    pub fn puts_off(&self) -> bool {
        !self.put_off.is_empty() || !self.busy_asks.is_empty()
    }

    /// Asks again, at the next plan, for the assets the spool had no file for, and for those it
    /// had no room for once it has, and answers again the asks the storage was too busy for;
    /// the host calls it after a while. Whether it asks or answers any.
    pub fn retry_put_off(&mut self) -> bool {
        let answers = !self.busy_asks.is_empty();
        for (hash, stretch) in std::mem::take(&mut self.busy_asks) {
            self.want(hash, stretch);
        }
        let before = self.put_off.len();
        let budget = self.spool.budget();
        let mut spooled = self.spooled();
        let room_for = &mut self.room_for;
        // Asked for only when it fits, so a full disk doesn't bring parts that are thrown away.
        self.put_off.retain(|hash| match room_for.get(hash) {
            Some(total) if spooled.saturating_add(*total) > budget => true,
            Some(total) => {
                spooled = spooled.saturating_add(*total);
                room_for.remove(hash);
                false
            }
            None => false,
        });
        if self.put_off.len() == before {
            return answers;
        }
        self.unsettled = true;
        true
    }

    /// The bytes the spool holds or is to hold: assets coming in parts, and come and not sealed.
    fn spooled(&self) -> u64 {
        self.incoming
            .values()
            .map(|incoming| incoming.total)
            .chain(self.arrived.values().filter(|arrival| matches!(arrival, Arrival::Range(_))).map(Arrival::byte_len))
            .fold(0u64, u64::saturating_add)
    }

    /// Whether asks for assets wait to be answered at the next publish.
    pub fn owes_assets(&self) -> bool {
        !self.wanted.is_empty()
    }

    /// How far the assets coming in parts have got: the bytes received, of how many.
    pub fn asset_progress(&self) -> (u64, u64) {
        self.incoming.values().fold((0, 0), |(received, total), incoming| (received.saturating_add(incoming.received), total.saturating_add(incoming.total)))
    }

    /// How many assets the cards wait for that this session can't take: too large for it, or
    /// not what they were named.
    pub fn unusable_assets(&self) -> usize {
        self.unusable.len()
    }

    /// Whether this session holds what the room holds, as far as it has heard: the relay has
    /// said what it may do and nothing that came in waits to be taken.
    pub fn caught_up(&self) -> bool {
        self.role.is_some() && !self.unsettled
    }

    /// What this session may do in the room; `None` until the relay has said what a link
    /// lets it do.
    pub fn role(&self) -> Option<Role> {
        self.role
    }

    /// Why the relay dropped one of this commenter's discussion changes since the last call,
    /// if it did: the change wasn't theirs to make. The session takes back the change.
    pub fn take_refusal(&mut self) -> Option<String> {
        self.refusal.take()
    }

    /// Why what the room holds can't be taken in beside one of this session's private canvases,
    /// once each time it happens: someone else used one of its ids or technique versions. The
    /// room's changes wait until the canvas is shared, changed or removed.
    pub fn take_clash(&mut self) -> Option<String> {
        self.clash_said.take()
    }

    /// The first frame on each new connection. Assets asked for on an earlier one are asked
    /// for again, the owner's links are sent again, and a commenter's unanswered changes go
    /// again once the relay has welcomed it.
    /// Where this session last said it is goes again too; who else was there is forgotten
    /// until the relay says again.
    pub fn hello(&mut self) -> Frame {
        self.asked.clear();
        // The relay knows only this connection's reports.
        self.reported.clear();
        self.unsettled = true;
        self.sent_pending = false;
        self.people.clear();
        let hello = match &self.entry {
            Entry::Room(room) => {
                self.outbox = self.links.again();
                Frame::Hello { protocol: PROTOCOL, room: room.clone() }
            }
            Entry::Link(link) => {
                // The link's terms may have changed while away.
                self.role = None;
                Frame::HelloLink { protocol: PROTOCOL, link: link.clone() }
            }
        };
        self.outbox.retain(|frame| !matches!(frame, Frame::Presence(_)));
        if let Some(presence) = &self.presence {
            self.outbox.push(Frame::Presence(presence.clone()));
        }
        hello
    }

    /// Says where this session is: the frame to send, or `None` when it hasn't moved. A
    /// presence that wouldn't pass the relay's checks is refused here.
    pub fn set_presence(&mut self, presence: Presence) -> Result<Option<Frame>, Error> {
        presence.validate()?;
        if self.presence.as_ref() == Some(&presence) {
            return Ok(None);
        }
        self.presence = Some(presence.clone());
        // One queued by a reconnect is older: sent after this one, it would undo it.
        self.outbox.retain(|frame| !matches!(frame, Frame::Presence(_)));
        Ok(Some(Frame::Presence(presence)))
    }

    /// The others in the room who have said where they are, by peer.
    pub fn people(&self) -> impl Iterator<Item = &Person> {
        self.people.values()
    }

    /// The project shared in the room, once its first update has arrived.
    pub fn project_id(&self) -> Result<Option<ProjectId>, Error> {
        self.mirror.project_id()
    }

    /// Frames for the next publish, ahead of what it makes: a host keeping the share links
    /// itself ([`Links`]) queues theirs here.
    pub fn queue(&mut self, frames: Vec<Frame>) {
        self.outbox.extend(frames);
    }

    /// The session's own edits since the last publish, and any assets the others asked for
    /// that this session has. A commenter's are its changes to discussions; a viewer has none.
    pub fn publish(&mut self, session: &Session) -> Result<Vec<Frame>, Error> {
        let has = |hash: &Blake3| session.assets().contains(hash);
        let view = View { revision: session.revision(), document: session.document(), has: &has };
        let out = self.publish_view(&view)?;
        Ok(self.frames(out, session.assets()))
    }

    fn publish_view(&mut self, view: &View<'_>) -> Result<Vec<Out>, Error> {
        let mut out: Vec<Out> = std::mem::take(&mut self.outbox).into_iter().map(Out::Frame).collect();
        let Some(role) = self.role else { return Ok(out) };
        if !role.edits() {
            self.wanted.clear();
        }
        // Only what the room's project uses goes out: not what only a private canvas does.
        if !self.wanted.is_empty() {
            let used = assets_in(&*shared(view.document)?)?;
            self.wanted.retain(|(hash, _)| used.contains(hash) && (view.has)(hash));
        }
        for _ in 0..ANSWERED_PER_PUBLISH {
            let Some((hash, stretch)) = self.wanted.pop_first() else { break };
            out.push(Out::Asset(hash, stretch));
        }
        if role == Role::Comment && !self.sent_pending {
            out.extend(self.pending.iter().map(|(seq, change)| Out::Frame(put(*seq, change))));
            self.sent_pending = true;
        }
        // A joiner has nothing of its own to publish until it has taken the project in.
        if self.published.is_none_or(|revision| revision == view.revision) {
            return Ok(out);
        }
        match role {
            Role::Edit | Role::Owner => {
                if let Some(update) = self.mirror.publish(&*shared(view.document)?)? {
                    out.push(Out::Frame(Frame::Update(update)));
                }
            }
            Role::Comment => {
                // Too many unanswered: the relay is slow or gone. These wait for the next turn.
                self.held = self.pending.len() >= MAX_PENDING;
                if self.held {
                    return Ok(out);
                }
                for change in self.mirror.comments_since(&*shared(view.document)?)? {
                    self.seq = self.seq.saturating_add(1);
                    out.push(Out::Frame(put(self.seq, &change)));
                    self.pending.push((self.seq, change));
                }
            }
            // The session refuses a viewer's edits; the shared document stays as it is.
            Role::View => {}
        }
        self.published = Some(view.revision);
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
                self.clash = None;
                Ok(Vec::new())
            }
            Frame::AssetPut { hash, bytes } => {
                let hash = Blake3::from_bytes(hash);
                let held: u64 = self.arrived.values().filter(|arrival| matches!(arrival, Arrival::Bytes(_))).map(Arrival::byte_len).sum();
                if self.asked.remove(&hash) && held.saturating_add(bytes.len() as u64) <= MAX_ARRIVED_BYTES as u64 {
                    self.arrived.insert(hash, Arrival::Bytes(bytes));
                    self.unsettled = true;
                }
                Ok(Vec::new())
            }
            Frame::AssetPart { hash, total, offset, bytes } => self.part(Blake3::from_bytes(hash), total, offset, &bytes),
            Frame::AssetGet(hashes) => {
                if self.role.is_some_and(Role::edits) {
                    for hash in hashes {
                        self.want(Blake3::from_bytes(hash), Stretch::Whole);
                    }
                }
                Ok(Vec::new())
            }
            // Nobody else has the asset this session said came bad.
            Frame::AssetBad(hash) => {
                let hash = Blake3::from_bytes(hash);
                if self.reported.remove(&hash) && self.asked.contains(&hash) {
                    self.give_up(hash);
                }
                Ok(Vec::new())
            }
            Frame::AssetFrom { hash, offset, len } => {
                if self.role.is_some_and(Role::edits) {
                    self.want(Blake3::from_bytes(hash), Stretch::From { offset, len });
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
            Frame::CommentDone { seq, refused } if self.role == Some(Role::Comment) => {
                self.pending.retain(|(sent, _)| *sent > seq);
                if let Some(reason) = refused {
                    self.refusal = Some(shown(&reason));
                }
                self.unsettled = true;
                Ok(Vec::new())
            }
            Frame::Person(person) => {
                if self.people.len() < MAX_PEOPLE || self.people.contains_key(&person.peer) {
                    self.people.insert(person.peer, *person);
                }
                Ok(Vec::new())
            }
            Frame::Gone(peer) => {
                self.people.remove(&peer);
                Ok(Vec::new())
            }
            Frame::Refused(reason) => Err(Error::new(ErrorKind::Conflict, format!("the relay ended the shared session: {}", shown(&reason)))),
            Frame::Step1(_)
            | Frame::Step2(_)
            | Frame::Update(_)
            | Frame::Welcome { .. }
            | Frame::CommentDone { .. }
            | Frame::Hello { .. }
            | Frame::HelloLink { .. }
            | Frame::LinkPut { .. }
            | Frame::LinkRevoke(_)
            | Frame::CommentPut { .. }
            | Frame::Presence(_) => Err(Error::new(ErrorKind::InvalidInput, "the relay sent a frame it doesn't send")),
        }
    }

    /// The frames for `out`, with assets' bytes read from `assets` ([`Out::frames`]). An ask
    /// the storage is too busy to read for is answered after a while ([`Self::retry_put_off`]);
    /// one for an asset that can't be read is dropped.
    pub fn frames(&mut self, out: Vec<Out>, assets: &AssetStore) -> Vec<Frame> {
        let mut frames = Vec::new();
        for out in out {
            match out {
                Out::Frame(frame) => frames.push(frame),
                Out::Asset(hash, stretch) => match parts::frames(assets, &hash, stretch) {
                    Ok(answer) => frames.extend(answer.unwrap_or_default()),
                    Err(error) if error.kind == ErrorKind::Busy && self.busy_asks.len() < MAX_WANTED => {
                        self.busy_asks.insert((hash, stretch));
                    }
                    Err(_) => {}
                },
            }
        }
        frames
    }

    /// Notes an ask for `stretch` of an asset, answered at the next publish.
    fn want(&mut self, hash: Blake3, stretch: Stretch) {
        if self.wanted.len() < MAX_WANTED {
            self.wanted.insert((hash, stretch));
        }
    }

    /// Takes the part at `offset` of an asset of `total` bytes, if it was asked for and is the
    /// next one; may ask for more of it.
    fn part(&mut self, hash: Blake3, total: u64, offset: u64, bytes: &[u8]) -> Result<Vec<Frame>, Error> {
        if !self.asked.contains(&hash) || self.unusable.contains(&hash) {
            return Ok(Vec::new());
        }
        if !self.incoming.contains_key(&hash) {
            if offset != 0 {
                return Ok(Vec::new());
            }
            let spooled = self.spooled();
            // One larger than this session takes can't come from anyone; a frame can't say an
            // asset is larger than any shared one.
            if total > self.spool.limit() {
                self.give_up(hash);
                return Ok(Vec::new());
            }
            // Over budget, it waits for the others to be taken and is asked for again.
            if spooled.saturating_add(total) > self.spool.budget() {
                self.asked.remove(&hash);
                self.put_off.insert(hash);
                self.room_for.insert(hash, total);
                return Ok(Vec::new());
            }
            if self.spool.begin(hash, total).is_err() {
                self.spool_failed(hash);
                return Ok(Vec::new());
            }
            self.incoming.insert(hash, Incoming::new(total));
        }
        let Some(incoming) = self.incoming.get_mut(&hash) else { return Ok(Vec::new()) };
        // Parts come in order; one that doesn't follow on is another sender's copy.
        if incoming.total != total || incoming.received != offset {
            return Ok(Vec::new());
        }
        if self.spool.append(&hash, bytes).is_err() {
            self.spool_failed(hash);
            return Ok(Vec::new());
        }
        incoming.take(bytes);
        if !incoming.whole() {
            return Ok(incoming.more(&hash).into_iter().collect());
        }
        let checks = incoming.checks(&hash);
        self.incoming.remove(&hash);
        self.asked.remove(&hash);
        match checks.then(|| self.spool.finish(&hash)) {
            Some(Ok(range)) => {
                self.arrived.insert(hash, Arrival::Range(range));
                self.spool_tries.remove(&hash);
                self.reported.remove(&hash);
                self.unsettled = true;
            }
            Some(Err(_)) => self.spool_failed(hash),
            None => {
                self.spool.discard(&hash);
                return Ok(self.bad_copy(hash));
            }
        }
        Ok(Vec::new())
    }

    /// A copy of an asset that isn't the one named: the first time, the relay is told and asks
    /// someone else for it (a peer with a damaged file, or a hostile one, doesn't stop the asset
    /// from coming), or says nobody else has it; the second time, it is given up.
    fn bad_copy(&mut self, hash: Blake3) -> Vec<Frame> {
        if !self.reported.insert(hash) {
            self.reported.remove(&hash);
            self.give_up(hash);
            return Vec::new();
        }
        if self.incoming.remove(&hash).is_some() {
            self.spool.discard(&hash);
        }
        self.asked.insert(hash);
        vec![Frame::AssetBad(*hash.as_bytes())]
    }

    /// Puts off an asset the spool failed to keep (a full disk, say), to be asked for again from
    /// the start; one it keeps failing is given up.
    fn spool_failed(&mut self, hash: Blake3) {
        let tries = self.spool_tries.entry(hash).or_insert(0);
        *tries = tries.saturating_add(1);
        if *tries > MAX_SPOOL_TRIES {
            self.spool_tries.remove(&hash);
            self.give_up(hash);
            return;
        }
        if self.incoming.remove(&hash).is_some() {
            self.spool.discard(&hash);
        }
        self.asked.remove(&hash);
        self.put_off.insert(hash);
    }

    /// Stops taking an asset this session can't keep.
    fn give_up(&mut self, hash: Blake3) {
        if self.incoming.remove(&hash).is_some() {
            self.spool.discard(&hash);
        }
        self.asked.remove(&hash);
        self.unusable.insert(hash);
    }

    /// Brings `session` up to what the shared document holds, once every asset it needs has
    /// arrived, and gives it the role the room lets it have. Local edits go out first, so
    /// nothing made here is lost.
    pub fn settle(&mut self, session: &mut Session) -> Result<Vec<Frame>, Error> {
        let has = |hash: &Blake3| session.assets().contains(hash);
        let (out, plan) = self.plan(&View { revision: session.revision(), document: session.document(), has: &has })?;
        let mut out = self.frames(out, session.assets());
        let applied = match plan.take(session)? {
            Taken::Nothing => return Ok(out),
            Taken::Stale => {
                out.extend(self.taken(session.document(), session.revision(), false)?);
                return Ok(out);
            }
            Taken::Applied(applied) => applied,
        };
        // A set that didn't apply, or a card still waiting, is tried again when the next update
        // or asset comes.
        out.extend(self.taken(session.document(), session.revision(), true)?);
        applied?;
        Ok(out)
    }

    /// What the session should take to hold what the shared document holds, as [`Self::settle`]
    /// does, from `view` of it. Local edits in `view` go out first. Say what came of the plan
    /// with [`Self::taken`] before the next.
    pub fn plan(&mut self, view: &View<'_>) -> Result<(Vec<Out>, Plan), Error> {
        let mut out = self.publish_view(view)?;
        let mut plan = Plan { base: view.revision, access: None, imports: Vec::new(), sets: None };
        let Some(role) = self.role else { return Ok((out, plan)) };
        plan.access = Some(match role {
            Role::Owner => Access::owner(),
            role => Access::new(role, self.name.clone())?,
        });
        if !self.unsettled || self.held || self.clash == Some(view.revision) {
            return Ok((out, plan));
        }
        let Some(Materialized { mut project, repairs, .. }) = self.mirror.materialize()? else { return Ok((out, plan)) };
        // Only an editor writes the shared document; the others' copies take what an editor's
        // repair brings.
        if let Some(repairs) = repairs.filter(|_| role.edits()) {
            out.push(Out::Frame(Frame::Update(repairs)));
        }
        if project.id() != view.document.id() {
            return Err(Error::new(ErrorKind::Conflict, "this session holds a different project from the one shared"));
        }
        for (_, change) in &self.pending {
            project = overlaid(project, change);
        }
        // The session's private canvases stay as it has them. One the shared document still
        // holds (made private as someone changed it) is taken out of it again once taken.
        let mut lingering = Vec::new();
        for canvas in view.document.canvases().values().filter(|canvas| canvas.is_private()) {
            let mut kept = vec![Change::AddCanvas { canvas: canvas.clone() }];
            if let Some(there) = project.canvases().get(&canvas.id()) {
                lingering.push(there.clone());
                kept.insert(0, Change::RemoveCanvas { id: canvas.id() });
            }
            // Others can't see a private canvas, so they may reuse what it holds: a technique
            // version, or an id they saw before it went private. Neither side's work is dropped;
            // the room's changes wait, and the person is told why.
            if let Err(error) = Changeset::new(kept).apply(&mut project) {
                self.clash = Some(view.revision);
                self.clash_said = Some(format!(
                    "a change from the room clashes with your private canvas “{}” ({}); share, change or remove that canvas to take in the others' changes",
                    canvas.title(),
                    error.message
                ));
                return Ok((out, plan));
            }
        }
        let mut missing = BTreeSet::new();
        for (canvas_id, canvas) in project.canvases() {
            let now = view.document.canvases().get(canvas_id);
            // A card the session already holds as it is has its assets.
            for node in canvas.nodes().values().filter(|node| now.and_then(|now| now.nodes().get(&node.id)) != Some(*node)) {
                missing.extend(assets_of(canvas.id(), node)?.into_iter().filter(|hash| !(view.has)(hash)));
            }
        }
        // An asset that arrived goes in with the plan, if it is what was asked for. A host that
        // seals large ones first brings them once sealed; the cards that use them wait.
        let mut brought = BTreeSet::new();
        for hash in &missing {
            let Some(arrival) = self.arrived.get(hash) else { continue };
            match arrival {
                Arrival::Range(_) if self.seals => continue,
                Arrival::Bytes(bytes) if blake3::hash(bytes).as_bytes() != hash.as_bytes() => {
                    self.arrived.remove(hash);
                    continue;
                }
                _ => {}
            }
            if let Some(arrival) = self.arrived.remove(hash) {
                brought.insert(*hash);
                plan.imports.push((*hash, arrival));
            }
        }
        missing.retain(|hash| !brought.contains(hash));
        // One coming in parts, or come, that no card waits for any more needn't be kept.
        self.arrived.retain(|hash, _| missing.contains(hash));
        self.put_off.retain(|hash| missing.contains(hash));
        self.room_for.retain(|hash, _| missing.contains(hash));
        self.spool_tries.retain(|hash, _| missing.contains(hash));
        self.seal_tries.retain(|hash, _| missing.contains(hash));
        self.reported.retain(|hash| missing.contains(hash));
        let unneeded: Vec<Blake3> = self.incoming.keys().filter(|hash| !missing.contains(hash)).copied().collect();
        for hash in unneeded {
            self.incoming.remove(&hash);
            self.spool.discard(&hash);
            self.asked.remove(&hash);
        }
        // Ask once for each asset still missing, or for the rest of one that was coming on an
        // earlier connection; the cards that use one wait for it, and the rest goes in now.
        let mut ask = Vec::new();
        for hash in missing.iter().filter(|hash| !self.unusable.contains(hash) && !self.put_off.contains(hash) && !self.arrived.contains_key(hash)) {
            if !self.asked.insert(*hash) {
                continue;
            }
            match self.incoming.get_mut(hash) {
                Some(incoming) => out.push(Out::Frame(incoming.resume(hash))),
                None => ask.push(*hash.as_bytes()),
            }
        }
        if !ask.is_empty() {
            out.push(Out::Frame(Frame::AssetGet(ask)));
        }
        let project = if missing.is_empty() { project } else { waiting_without(view.document, project, &missing)? };
        // Only a joiner that never took anything starts from the empty placeholder.
        let start = self.published.is_none() && *view.document == joining(view.document.id())?;
        plan.sets = Some(if start && project != *view.document {
            Sets::Whole(Box::new(SharedProject::new(project)?))
        } else {
            Sets::Diff(diff::diff(view.document, &project)?)
        });
        self.unsettled = false;
        self.lingering = lingering;
        Ok((out, plan))
    }

    /// What came of the last plan that settled: `applied`, `document` (at `revision`) is what
    /// the session now holds, and later publishes compare with it; else the session had
    /// changed, and the next plan settles again.
    pub fn taken(&mut self, document: &Project, revision: u64, applied: bool) -> Result<Vec<Frame>, Error> {
        let lingering = std::mem::take(&mut self.lingering);
        if !applied {
            self.unsettled = true;
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let room = shared(document)?;
        if lingering.is_empty() || !self.role.is_some_and(Role::edits) {
            self.mirror.adopt(&room);
        } else {
            let mut held = room.clone().into_owned();
            Changeset::new(lingering.into_iter().map(|canvas| Change::AddCanvas { canvas }).collect()).apply(&mut held)?;
            self.mirror.adopt(&held);
            if let Some(update) = self.mirror.publish(&room)? {
                out.push(Frame::Update(update));
            }
        }
        self.published = Some(revision);
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

/// `project` as the room sees it: without the canvases kept private.
fn shared(project: &Project) -> Result<Cow<'_, Project>, Error> {
    let private: Vec<Change> =
        project.canvases().values().filter(|canvas| canvas.is_private()).map(|canvas| Change::RemoveCanvas { id: canvas.id() }).collect();
    if private.is_empty() {
        return Ok(Cow::Borrowed(project));
    }
    let mut room = project.clone();
    Changeset::new(private).apply(&mut room)?;
    Ok(Cow::Owned(room))
}

/// The assets the cards of `project` name.
fn assets_in(project: &Project) -> Result<BTreeSet<Blake3>, Error> {
    let mut used = BTreeSet::new();
    for canvas in project.canvases().values() {
        for node in canvas.nodes().values() {
            used.extend(assets_of(canvas.id(), node)?);
        }
    }
    Ok(used)
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

/// Whether sealing an asset that failed with `kind`, `tries` times already for a reason that
/// may pass, is tried again: always while the storage is busy (another transfer holds it), for a
/// while when it conflicts; never when the asset can't be kept.
fn seal_waits(kind: ErrorKind, tries: u32) -> bool {
    match kind {
        ErrorKind::Busy => true,
        ErrorKind::Conflict => tries < MAX_SEAL_TRIES,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
