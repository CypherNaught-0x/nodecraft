#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft graph: document model and invertible changesets.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

pub mod assemble;
mod changes;
pub mod diff;
pub mod layout;
mod model;
mod skill;
mod technique;
mod validate;

pub use changes::{Change, Changeset, NodeFieldAssignment, NodePosition, NodeTagAssignment, StagedChanges};
pub use model::{
    Canvas, CanvasKind, Comment, CommentPin, CommentReply, Edge, Endpoint, Field, FieldRect, Generation, Incomplete, ItemKey, ItemTag, JobInput, JobSettings,
    MAX_SENT_KEY, Missing, MissingItem, Node, Port, Position, Project, SentKey, Tag,
};
pub use skill::{
    MAX_SKILL_DESCRIPTION_BYTES, MAX_SKILL_INSTRUCTIONS_BYTES, MAX_SKILL_NAME_BYTES, MAX_SKILL_ORIGIN_BYTES, MAX_SKILL_REQUIREMENTS, MAX_SKILLS, Requires,
    Skill, Source as SkillSource, TechniqueRef, name_key as skill_name_key, validate_skills,
};
pub use technique::{
    MAX_TECHNIQUE_DESCRIPTION_BYTES, MAX_TECHNIQUE_NAME_BYTES, MAX_TECHNIQUE_NODES, MAX_TECHNIQUE_PORTS, MAX_TECHNIQUE_TAGS, Technique, TechniqueParam,
    TechniquePort, upstream_of,
};
pub use validate::{
    MAX_AUTHOR_BYTES, MAX_CHANGES, MAX_CHANGESET_BYTES, MAX_COMMENT_BYTES, MAX_COMMENT_REPLIES, MAX_EDGES, MAX_HISTORY, MAX_ITEM_AXES, MAX_NODE_BYTES,
    MAX_NODES, MAX_PROJECT_BYTES, MAX_REPLY_BYTES, MAX_VALUE_DEPTH, MAX_VALUE_ITEMS, validate_author,
};
