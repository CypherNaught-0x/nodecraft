//! Join links (#698): where a shared project is and what lets someone in, as one line of text
//! to send a person. `ws://relay.example:7070/#room=<64 hex>` comes in by the room id, as its
//! owner; `ws://relay.example:7070/#link=<64 hex>` through a share link, in its role.
//!
//! What follows the `#` is the capability. A link's `Debug` output leaves it out, no error
//! repeats a link, and only [`JoinLink::reveal`] writes one out whole.
use std::fmt;
use std::str::FromStr;

use nodecraft_sync::{LinkKey, RoomId};
use nodecraft_types::{Error, ErrorKind};

use super::Collab;
use crate::session::Session;

/// The longest relay address taken, from settings, a command or a link.
pub const MAX_RELAY_BYTES: usize = 2048;

/// What lets the holder of a join link in.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Key {
    /// The room id: its holder is the owner.
    Room(RoomId),
    /// A share link's key, in the link's role.
    Link(LinkKey),
}

#[derive(Clone, PartialEq, Eq)]
pub struct JoinLink {
    relay: String,
    key: Key,
}

impl JoinLink {
    pub fn new(relay: &str, key: Key) -> Result<Self, Error> {
        Ok(Self { relay: relay_address(relay)?, key })
    }

    /// A link to a new room on `relay`, named from the platform's random source.
    pub fn new_room(relay: &str) -> Result<Self, Error> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "room entropy is unavailable"))?;
        Self::new(relay, Key::Room(RoomId::from_bytes(bytes)))
    }

    pub fn relay(&self) -> &str {
        &self.relay
    }

    pub fn key(&self) -> &Key {
        &self.key
    }

    /// The whole link, for the person to copy and send. Whoever holds it can come in.
    pub fn reveal(&self) -> String {
        match &self.key {
            Key::Room(room) => format!("{}#room={}", self.relay, room.as_str()),
            Key::Link(link) => format!("{}#link={}", self.relay, link.as_str()),
        }
    }

    /// Shares `session` in this link's room; only a room id shares.
    pub fn share(&self, session: &Session) -> Result<Collab, Error> {
        match &self.key {
            Key::Room(room) => Collab::share(session, room.clone()),
            Key::Link(_) => Err(Error::new(ErrorKind::InvalidInput, "a share link joins a project; it can't share one")),
        }
    }

    /// Shares `project`, a session's document at `revision`, as [`Self::share`] does.
    pub fn share_project(&self, project: &nodecraft_graph::Project, revision: u64) -> Result<Collab, Error> {
        match &self.key {
            Key::Room(room) => Collab::share_project(project, revision, room.clone()),
            Key::Link(_) => Err(Error::new(ErrorKind::InvalidInput, "a share link joins a project; it can't share one")),
        }
    }

    /// Joins the room this link opens.
    pub fn join(&self) -> Collab {
        match &self.key {
            Key::Room(room) => Collab::join(room.clone()),
            Key::Link(link) => Collab::join_link(link.clone()),
        }
    }
}

impl FromStr for JoinLink {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        let unreadable = || Error::new(ErrorKind::InvalidInput, "a join link is a ws:// relay address, then #room= or #link= and 64 hex digits");
        let (relay, key) = value.trim().split_once('#').ok_or_else(unreadable)?;
        let key = if let Some(room) = key.strip_prefix("room=") {
            Key::Room(room.parse().map_err(|_| unreadable())?)
        } else if let Some(link) = key.strip_prefix("link=") {
            Key::Link(link.parse().map_err(|_| unreadable())?)
        } else {
            return Err(unreadable());
        };
        Self::new(relay, key)
    }
}

/// The relay shows; the key stays out of logs.
impl fmt::Debug for JoinLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinLink").field("relay", &self.relay).field("key", &self.key).finish()
    }
}

/// A relay address as NodeCraft connects to it: `ws://`, a host, at most [`MAX_RELAY_BYTES`],
/// and nothing but visible ASCII. Errors don't repeat the address.
pub fn relay_address(value: &str) -> Result<String, Error> {
    let value = value.trim();
    if value.len() > MAX_RELAY_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, format!("a relay address is at most {MAX_RELAY_BYTES} bytes")));
    }
    if value.starts_with("wss://") {
        return Err(Error::new(ErrorKind::Unsupported, "NodeCraft can't reach a relay over wss:// yet; use a ws:// address"));
    }
    let Some(rest) = value.strip_prefix("ws://") else {
        return Err(Error::new(ErrorKind::InvalidInput, "a relay address starts with ws://"));
    };
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() || host.contains('@') || !value.bytes().all(|b| b.is_ascii_graphic() && b != b'#') {
        return Err(Error::new(ErrorKind::InvalidInput, "a relay address is ws:// and a host, with an optional port and path"));
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_link_reads_back_as_it_was_written() {
        for text in [format!("ws://relay.example:7070/#room={HEX}"), format!("ws://127.0.0.1:9/rooms#link={HEX}")] {
            let link: JoinLink = text.parse().unwrap();
            assert_eq!(link.reveal(), text);
        }
        let made = JoinLink::new_room("ws://relay.example").unwrap();
        assert_eq!(made.reveal().parse::<JoinLink>().unwrap(), made);
    }

    #[test]
    fn a_bad_link_is_refused_without_repeating_it() {
        for text in [
            HEX.to_string(),
            format!("http://relay/#room={HEX}"),
            format!("ws://#room={HEX}"),
            format!("ws://me@relay/#room={HEX}"),
            format!("ws://relay/#room={}", &HEX[1..]),
            format!("ws://relay/#door={HEX}"),
            format!("ws://re lay/#room={HEX}"),
            format!("ws://{}/#room={HEX}", "r".repeat(MAX_RELAY_BYTES)),
        ] {
            let error = text.parse::<JoinLink>().unwrap_err();
            assert!(!error.message.contains(&HEX[8..40]), "{text}: {}", error.message);
        }
        assert_eq!(relay_address("wss://relay").unwrap_err().kind, ErrorKind::Unsupported);
    }

    #[test]
    fn debug_output_leaves_the_key_out() {
        let link: JoinLink = format!("ws://relay/#link={HEX}").parse().unwrap();
        let shown = format!("{link:?}");
        assert!(shown.contains("ws://relay/") && !shown.contains(&HEX[..16]), "{shown}");
    }
}
