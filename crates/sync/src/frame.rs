//! What peers and the relay send each other: one binary message per frame, a tag byte and its
//! payload. Every length is checked before anything is kept, so a hostile frame costs at most
//! its own size.
use std::fmt;
use std::str::FromStr;

use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};

/// The protocol version a hello names; a relay refuses any other.
pub const PROTOCOL: u32 = 1;
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

#[derive(Serialize, Deserialize)]
struct Hello {
    protocol: u32,
    room: String,
}

/// One message between a peer and the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
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
            _ => return Err(invalid("a frame of an unknown kind")),
        })
    }
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

    proptest! {
        #[test]
        fn any_bytes_decode_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = Frame::decode(&bytes);
        }
    }
}
