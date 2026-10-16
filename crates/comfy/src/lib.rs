#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft comfy: ComfyUI client and workflow model.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.
//!
//! Sans-IO: this crate builds requests ([`Server`]) and reads replies and WebSocket messages, and
//! leaves sending them to the transport in `providers` (reqwest on both desktop and wasm). That
//! keeps it testable from fixtures and free of an async runtime.
//!
//! It is written against ComfyUI's public HTTP/WebSocket protocol and JSON formats; no ComfyUI
//! code (GPL) is used or consulted.

mod error;
#[cfg(test)]
mod fixtures;
mod mapping;
mod object_info;
mod ordered;
mod prompt;
mod server;
mod status;
mod workflow;
mod ws;

pub use error::{ComfyError, NodeError};
pub use mapping::{Exposed, ExposedInput, ExposedKind, ExposedOutput, InputRole, Media, discover};
pub use object_info::{InputKind, InputSpec, MAX_OBJECT_INFO_BYTES, NodeCatalog, NodeInfo, OutputSpec};
pub use prompt::{PromptRequest, Queued, parse_prompt_response};
pub use server::{Body, FileRef, Method, Request, Server, UploadKind, parse_upload_response};
pub use status::{Device, HistoryEntry, QueueState, SystemStats};
pub use workflow::{ApiNode, ApiWorkflow, CheckReport, InputProblem, InputValue, MAX_NODES, MAX_WORKFLOW_BYTES};
pub use ws::{MAX_WS_MESSAGE_BYTES, Outcome, PreviewFormat, PromptProgress, WsEvent, files_in, parse_binary, parse_text};
