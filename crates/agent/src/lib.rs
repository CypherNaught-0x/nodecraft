#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! NodeCraft agent: the in-canvas agent: an LLM tool loop over engine commands.
//!
//! [`Agent`] keeps one conversation with a chat model and runs the model's tool calls through
//! the host's [`Toolbox`] (the app gives it the same tools the MCP server offers). Before each
//! call it asks the toolbox what the call would do ([`Impact`]): in [`Mode::Assist`] changes and
//! runs wait for the person's approval; in [`Mode::Auto`] changes go ahead and runs go ahead
//! while what they accept fits the agent's budget. Every step lands in a timeline the sidebar
//! shows, and [`Agent::cancel`] (Esc) stops at once. The agent is sans-IO: the host sends each
//! request on its own transport and hands the reply back.
//!
//! Layer and allowed dependencies: `docs/architecture.md`, enforced by `cargo xtask layers`.

mod agent;
mod prompt;
#[cfg(test)]
mod tests;
mod timeline;

pub use agent::{
    Agent, Config, MAX_ASK_BYTES, MAX_CALLS_PER_REPLY, MAX_CONVERSATION_BYTES, MAX_MESSAGES, MAX_RESULT_BYTES, MAX_ROUNDS, MAX_STEPS, MAX_SUMMARY_CHARS, Mode,
    Next, Status, Ticket, Toolbox,
};
pub use nodecraft_engine::impact::{Impact, RunImpact};
pub use prompt::{Focus, FocusNode, MAX_FOCUS_NODES};
pub use timeline::{Step, StepKind, ToolStatus, ToolStep};
