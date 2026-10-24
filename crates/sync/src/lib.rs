#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft sync: a project shared live (#119). Each peer mirrors its document into a CRDT
//! document (`yrs`, Yjs-compatible), one entry per card, wire, field, tag, comment and skill, so
//! what several people do at once merges; the merged entries are assembled back into a valid
//! project (`nodecraft_graph::assemble`) and reach the peer's own document as ordinary
//! changesets. The relay's rooms and the wire frames live here too, free of any socket, so the
//! same code runs in the desktop app, the CLI relay and the browser.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod discussion;
mod frame;
mod mirror;
mod presence;
mod role;
mod room;

pub use frame::{Frame, LinkKey, MAX_ASSET_BYTES, MAX_FRAME_BYTES, MAX_SHARED_ASSET_BYTES, MAX_UPDATE_BYTES, PART_BYTES, PROTOCOL, RoomId, WINDOW_BYTES};
pub use mirror::{CommentChange, Materialized, Mirror};
pub use presence::{MAX_SELECTION, OWNER_NAME, Person, Presence};
pub use role::{Grant, Role};
pub use room::{MAX_LINKS, Out, Peer, Room};
