//! The steps of a conversation, as the sidebar shows them: what the person asked, what the
//! model said, and each tool call with what it would do and what became of it.
use nodecraft_engine::impact::Impact;

/// One entry of the timeline. `id`s grow by one per step and are never reused.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: u64,
    pub kind: StepKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StepKind {
    /// The person's message.
    Asked(String),
    /// The model's text.
    Said(String),
    /// A tool call the model made.
    Tool(ToolStep),
    /// Something went wrong; the conversation can go on.
    Error(String),
    /// The person cancelled what was in progress.
    Cancelled,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolStep {
    /// What the call is, in a few words (the command id for a command).
    pub label: String,
    /// The call's arguments as compact JSON, cut to [`crate::MAX_SUMMARY_CHARS`].
    pub input: String,
    /// What the call would do, once known.
    pub impact: Option<Impact>,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    /// Waiting its turn in this reply.
    Queued,
    /// Waiting for the person to approve it.
    Planned,
    /// Ran; a short summary of the result.
    Ran(String),
    Failed(String),
    /// The person declined it.
    Declined,
    /// The person cancelled before it ran.
    Cancelled,
}
