#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft graph: document model and invertible changesets.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod changes;
pub mod layout;
mod model;
mod validate;

pub use changes::{Change, Changeset, NodeFieldAssignment, NodePosition, NodeTagAssignment, StagedChanges};
pub use model::{Canvas, Comment, CommentPin, CommentReply, Edge, Endpoint, Field, FieldRect, Generation, Node, Port, Position, Project, Tag};
pub use validate::{
    MAX_CHANGES, MAX_CHANGESET_BYTES, MAX_COMMENT_BYTES, MAX_COMMENT_REPLIES, MAX_HISTORY, MAX_NODE_BYTES, MAX_PROJECT_BYTES, MAX_REPLY_BYTES, MAX_VALUE_DEPTH,
    MAX_VALUE_ITEMS,
};
