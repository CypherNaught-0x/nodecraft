//! What peers and the relay send each other: one binary message per frame, a tag byte and its
//! payload. Every length is checked before anything is kept, so a hostile frame costs at most
//! its own size.
use std::fmt;
use std::str::FromStr;

use nodecraft_graph::Comment;
use nodecraft_types::{CanvasId, CommentId, Error, ErrorKind};
use serde::{Deserialize, Serialize};

use crate::role::{Grant, Role};

/// The protocol version a hello names; a relay refuses any other. 2 added share links (#121).
pub const PROTOCOL: u32 = 2;
/// The largest update a frame carries: a whole shared project, as a late joiner receives it.
pub const MAX_UPDATE_BYTES: usize = 64 << 20;
/// The largest asset a frame carries, the same as the project's own asset limit.
pub const MAX_ASSET_BYTES: usize = 16 << 20;
/// The largest frame of any kind.
pub const MAX_FRAME_BYTES: usize = MAX_UPDATE_BYTES + 64;
/// The most assets one request names.
const MAX_ASKED: usize = 4096;
const HASH: usize = 32;

const HELLO: u8 = 0;
const STEP1: u8 = 1;
const STEP2: u8 = 2;
const UPDATE: u8 = 3;
const ASSET_PUT: u8 = 4;
const ASSET_GET: u8 = 5;
const REFUSED: u8 = 6;
const HELLO_LINK: u8 = 7;
const WELCOME: u8 = 8;
const LINK_PUT: u8 = 9;
const LINK_REVOKE: u8 = 10;
const COMMENT_PUT: u8 = 11;
const COMMENT_DONE: u8 = 12;

/// A room's name, which is also what lets a peer in: 32 random bytes, shared only with the people
/// invited, so it can't be guessed.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RoomId(String);

impl RoomId {
    /// A room named by `bytes`, which the caller draws from the platform's random source.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RoomId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        if value.len() == 2 * HASH && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "a room is 64 lowercase hex digits"))
        }
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The room id stays out of logs: it is the key to the room.
impl fmt::Debug for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RoomId(..)")
    }
}

/// A share link's key (#121): 32 random bytes that let whoever holds them into one room with
/// one role. The relay keeps only its hash, so a relay's memory never holds a working link.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkKey(String);

impl LinkKey {
    /// A key made of `bytes`, which the caller draws from the platform's random source.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    /// What the relay knows the link by.
    pub fn hash(&self) -> [u8; HASH] {
        *blake3::hash(self.0.as_bytes()).as_bytes()
    }
}

impl FromStr for LinkKey {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        if value.len() == 2 * HASH && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            Ok(Self(value.to_owned()))
        } else {
            Err(Error::new(ErrorKind::InvalidInput, "a share link is 64 lowercase hex digits"))
        }
    }
}

impl fmt::Display for LinkKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A link key stays out of logs, as a room id does.
impl fmt::Debug for LinkKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LinkKey(..)")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelloLink {
    protocol: u32,
    link: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Welcome {
    role: Role,
    name: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkPut {
    hash: String,
    grant: Grant,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentPut {
    seq: u64,
    canvas: CanvasId,
    id: CommentId,
    base: Option<Comment>,
    comment: Option<Comment>,
}

#[derive(Serialize, Deserialize)]
struct Hello {
    protocol: u32,
    room: String,
}

/// One message between a peer and the relay.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A peer's first frame: which room, in which protocol.
    Hello { protocol: u32, room: RoomId },
    /// "This is what I have": a state vector, answered with a `Step2`.
    Step1(Vec<u8>),
    /// What the asker lacks, as one update.
    Step2(Vec<u8>),
    /// A change made since, as one update.
    Update(Vec<u8>),
    /// An asset a card uses, under its content hash.
    AssetPut { hash: [u8; HASH], bytes: Vec<u8> },
    /// "Send me these assets."
    AssetGet(Vec<[u8; HASH]>),
    /// The relay won't go on, and why.
    Refused(String),
    /// A peer's first frame when it comes in through a share link instead of the room id.
    HelloLink { protocol: u32, link: LinkKey },
    /// The relay to a peer that came in through a link: what the link lets it do, and the name
    /// it was made for. An editor is then asked what it has; anyone else asks for the project.
    Welcome { role: Role, name: Option<String> },
    /// The room's owner (who joined by room id) adds or changes a link, by the link's hash.
    LinkPut { hash: [u8; HASH], grant: Grant },
    /// The room's owner takes a link back: whoever is in through it is put out.
    LinkRevoke([u8; HASH]),
    /// A commenter's change to one discussion: as it was (`base`) and as they made it (`None`
    /// on either side: it didn't exist, or they deleted it). Commenters don't write the shared
    /// document themselves: the relay checks each change is theirs to make, applies it to the
    /// discussion as it is now, and sends the result to everyone as an update.
    CommentPut { seq: u64, canvas: CanvasId, id: CommentId, base: Option<Box<Comment>>, comment: Option<Box<Comment>> },
    /// The relay has applied (or had nothing to apply for) the commenter's puts up to `seq`;
    /// the update holding them came first.
    CommentDone(u64),
}

impl Frame {
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let (tag, body) = match self {
            Self::Hello { protocol, room } => {
                let hello = Hello { protocol: *protocol, room: room.0.clone() };
                let body = serde_json::to_vec(&hello).map_err(|_| Error::new(ErrorKind::Internal, "the hello can't be written"))?;
                (HELLO, body)
            }
            Self::Step1(sv) => (STEP1, sv.clone()),
            Self::Step2(update) => (STEP2, checked(update, MAX_UPDATE_BYTES, "update")?.to_vec()),
            Self::Update(update) => (UPDATE, checked(update, MAX_UPDATE_BYTES, "update")?.to_vec()),
            Self::AssetPut { hash, bytes } => {
                let mut body = Vec::with_capacity(HASH.saturating_add(bytes.len()));
                body.extend_from_slice(hash);
                body.extend_from_slice(checked(bytes, MAX_ASSET_BYTES, "asset")?);
                (ASSET_PUT, body)
            }
            Self::AssetGet(hashes) => {
                if hashes.len() > MAX_ASKED {
                    return Err(Error::new(ErrorKind::Overflow, "too many assets asked for at once"));
                }
                (ASSET_GET, hashes.iter().flatten().copied().collect())
            }
            Self::Refused(message) => (REFUSED, message.as_bytes().to_vec()),
            Self::HelloLink { protocol, link } => (HELLO_LINK, body(&HelloLink { protocol: *protocol, link: link.0.clone() })?),
            Self::Welcome { role, name } => (WELCOME, body(&Welcome { role: *role, name: name.clone() })?),
            Self::LinkPut { hash, grant } => (LINK_PUT, body(&LinkPut { hash: hex(hash), grant: grant.clone() })?),
            Self::LinkRevoke(hash) => (LINK_REVOKE, hash.to_vec()),
            Self::CommentPut { seq, canvas, id, base, comment } => {
                (COMMENT_PUT, body(&CommentPut { seq: *seq, canvas: *canvas, id: *id, base: base.as_deref().cloned(), comment: comment.as_deref().cloned() })?)
            }
            Self::CommentDone(seq) => (COMMENT_DONE, seq.to_le_bytes().to_vec()),
        };
        let mut frame = Vec::with_capacity(body.len().saturating_add(1));
        frame.push(tag);
        frame.extend_from_slice(&body);
        checked(&frame, MAX_FRAME_BYTES, "frame")?;
        Ok(frame)
    }

    pub fn decode(frame: &[u8]) -> Result<Self, Error> {
        checked(frame, MAX_FRAME_BYTES, "frame")?;
        let (&tag, body) = frame.split_first().ok_or_else(|| invalid("an empty frame"))?;
        Ok(match tag {
            HELLO => {
                let hello: Hello = serde_json::from_slice(body).map_err(|_| invalid("the hello isn't readable"))?;
                Self::Hello { protocol: hello.protocol, room: hello.room.parse()? }
            }
            STEP1 => Self::Step1(body.to_vec()),
            STEP2 => Self::Step2(checked(body, MAX_UPDATE_BYTES, "update")?.to_vec()),
            UPDATE => Self::Update(checked(body, MAX_UPDATE_BYTES, "update")?.to_vec()),
            ASSET_PUT => {
                let (hash, bytes) = body.split_at_checked(HASH).ok_or_else(|| invalid("an asset without its hash"))?;
                let hash: [u8; HASH] = hash.try_into().map_err(|_| invalid("an asset without its hash"))?;
                let bytes = checked(bytes, MAX_ASSET_BYTES, "asset")?;
                if blake3::hash(bytes).as_bytes() != &hash {
                    return Err(invalid("an asset that doesn't match its hash"));
                }
                Self::AssetPut { hash, bytes: bytes.to_vec() }
            }
            ASSET_GET => {
                if body.len() % HASH != 0 || body.len() / HASH > MAX_ASKED {
                    return Err(invalid("the asset request isn't readable"));
                }
                Self::AssetGet(body.as_chunks::<HASH>().0.to_vec())
            }
            REFUSED => Self::Refused(String::from_utf8_lossy(body).chars().take(512).collect()),
            HELLO_LINK => {
                let hello: HelloLink = small(body).and_then(|b| serde_json::from_slice(b).ok()).ok_or_else(|| invalid("the hello isn't readable"))?;
                Self::HelloLink { protocol: hello.protocol, link: hello.link.parse()? }
            }
            WELCOME => {
                let welcome: Welcome = small(body).and_then(|b| serde_json::from_slice(b).ok()).ok_or_else(|| invalid("the welcome isn't readable"))?;
                Self::Welcome { role: welcome.role, name: welcome.name }
            }
            LINK_PUT => {
                let put: LinkPut = small(body).and_then(|b| serde_json::from_slice(b).ok()).ok_or_else(|| invalid("the share link isn't readable"))?;
                put.grant.validate()?;
                Self::LinkPut { hash: unhex(&put.hash).ok_or_else(|| invalid("the share link isn't readable"))?, grant: put.grant }
            }
            LINK_REVOKE => Self::LinkRevoke(body.try_into().map_err(|_| invalid("the share link isn't readable"))?),
            COMMENT_PUT => {
                let put: CommentPut =
                    serde_json::from_slice(checked(body, MAX_COMMENT_FRAME_BYTES, "discussion")?).map_err(|_| invalid("the discussion isn't readable"))?;
                for comment in put.base.iter().chain(&put.comment) {
                    if comment.id != put.id {
                        return Err(invalid("the discussion isn't the one named"));
                    }
                    comment.validate_payload()?;
                }
                Self::CommentPut { seq: put.seq, canvas: put.canvas, id: put.id, base: put.base.map(Box::new), comment: put.comment.map(Box::new) }
            }
            COMMENT_DONE => Self::CommentDone(u64::from_le_bytes(body.try_into().map_err(|_| invalid("a frame of an unknown kind"))?)),
            _ => return Err(invalid("a frame of an unknown kind")),
        })
    }
}

/// The largest discussion frame: a thread at its limit twice over, written as JSON (escapes
/// included).
const MAX_COMMENT_FRAME_BYTES: usize = 16 << 20;
/// The largest hello, welcome or link frame.
const MAX_SMALL_BYTES: usize = 4096;

fn small(body: &[u8]) -> Option<&[u8]> {
    (body.len() <= MAX_SMALL_BYTES).then_some(body)
}

fn body(value: &impl Serialize) -> Result<Vec<u8>, Error> {
    serde_json::to_vec(value).map_err(|_| Error::new(ErrorKind::Internal, "the frame can't be written"))
}

fn hex(bytes: &[u8; HASH]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; HASH]> {
    let mut out = [0; HASH];
    if text.len() != 2 * HASH {
        return None;
    }
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn checked<'a>(bytes: &'a [u8], limit: usize, what: &str) -> Result<&'a [u8], Error> {
    if bytes.len() > limit { Err(Error::new(ErrorKind::Overflow, format!("the {what} is too large to share"))) } else { Ok(bytes) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn room() -> RoomId {
        RoomId::from_bytes([0xab; 32])
    }

    #[test]
    fn every_frame_comes_back_as_it_went() {
        let bytes = b"pixels".to_vec();
        let hash = *blake3::hash(&bytes).as_bytes();
        for frame in [
            Frame::Hello { protocol: PROTOCOL, room: room() },
            Frame::Step1(vec![1, 2]),
            Frame::Step2(vec![3]),
            Frame::Update(Vec::new()),
            Frame::AssetPut { hash, bytes },
            Frame::AssetGet(vec![hash, [0; 32]]),
            Frame::Refused("full".into()),
            Frame::HelloLink { protocol: PROTOCOL, link: LinkKey::from_bytes([0xcd; 32]) },
            Frame::Welcome { role: Role::Comment, name: Some("Ana".into()) },
            Frame::LinkPut { hash, grant: Grant { role: Role::View, name: "Bo".into(), expires: Some(9) } },
            Frame::LinkRevoke(hash),
            Frame::CommentPut {
                seq: 4,
                canvas: CanvasId::from_bytes([1; 16]),
                id: CommentId::from_bytes([2; 16]),
                base: None,
                comment: Some(Box::new(Comment::new(CommentId::from_bytes([2; 16]), "Hi", nodecraft_graph::Position { x: 0.0, y: 0.0 }))),
            },
            Frame::CommentDone(4),
        ] {
            assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), frame);
        }
    }

    #[test]
    fn an_asset_must_match_its_hash() {
        let mut frame = Frame::AssetPut { hash: [7; 32], bytes: b"x".to_vec() }.encode().unwrap();
        assert!(Frame::decode(&frame).is_err());
        frame.truncate(10);
        assert!(Frame::decode(&frame).is_err());
    }

    #[test]
    fn a_room_id_is_hex_and_stays_out_of_debug_output() {
        assert_eq!(room().to_string().parse::<RoomId>().unwrap(), room());
        assert!("AB".repeat(32).parse::<RoomId>().is_err());
        assert!("ab".parse::<RoomId>().is_err());
        assert!(!format!("{:?}", room()).contains("ab"));
    }

    #[test]
    fn a_link_is_hex_stays_out_of_debug_output_and_is_known_by_its_hash() {
        let link = LinkKey::from_bytes([0xcd; 32]);
        assert_eq!(link.to_string().parse::<LinkKey>().unwrap(), link);
        assert!(!format!("{link:?}").contains("cd"));
        assert_ne!(link.hash(), [0xcd; 32]);
        // A grant that makes an owner, or has no name, doesn't decode.
        for grant in [Grant { role: Role::Owner, name: "Bo".into(), expires: None }, Grant { role: Role::View, name: " ".into(), expires: None }] {
            let mut frame = vec![LINK_PUT];
            frame.extend(serde_json::to_vec(&LinkPut { hash: hex(&[1; 32]), grant }).unwrap());
            assert!(Frame::decode(&frame).is_err());
        }
    }

    proptest! {
        #[test]
        fn any_bytes_decode_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = Frame::decode(&bytes);
        }
    }
}
