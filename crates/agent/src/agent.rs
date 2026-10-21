//! The tool loop. [`Agent`] is sans-IO: it hands the host each [`ChatRequest`] to send and takes
//! the reply back, so the host chooses the transport, the thread and the endpoint, and tests
//! script the model. Tool calls run through the host's [`Toolbox`] on the caller's thread.
use crate::prompt::{self, Focus};
use crate::timeline::{Step, StepKind, ToolStatus, ToolStep};
use nodecraft_engine::impact::{Impact, RunImpact};
use nodecraft_providers::ProviderError;
use nodecraft_providers::llm::{ChatRequest, ChatResponse, Message, Part, Role, StopReason, Tool};
use nodecraft_types::{Currency, Error, ErrorKind};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

/// Longest message the person may send, in bytes.
pub const MAX_ASK_BYTES: usize = 16 * 1024;
/// Most model calls one message may lead to before the agent stops and reports.
pub const MAX_ROUNDS: u32 = 32;
/// Most tool calls run from one reply; the rest are refused to the model.
pub const MAX_CALLS_PER_REPLY: usize = 32;
/// Longest tool result passed back to the model, in bytes; longer ones are cut.
pub const MAX_RESULT_BYTES: usize = 32 * 1024;
/// Most messages kept in the conversation; older turns are dropped whole.
pub const MAX_MESSAGES: usize = 400;
/// Most bytes of text, calls and results kept from earlier turns; older turns are dropped whole.
pub const MAX_CONVERSATION_BYTES: usize = 2 * 1024 * 1024;
/// Longest tool call id a reply may use (what the chat APIs accept).
const MAX_TOOL_ID: usize = 256;
/// Most steps kept in the timeline; older ones are dropped.
pub const MAX_STEPS: usize = 500;
/// Longest argument or result summary in a timeline step, in characters.
pub const MAX_SUMMARY_CHARS: usize = 300;

/// Whether the agent asks before it acts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Changes and runs wait for the person's approval; reads go ahead.
    #[default]
    Assist,
    /// Changes go ahead; runs go ahead while they fit the budget, and wait otherwise.
    Auto,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// The model to ask, as the endpoint names it.
    pub model: String,
    pub mode: Mode,
    /// The most auto mode may accept for runs this session, per currency, in micro-units. A
    /// currency without an entry has no budget: a run priced in it waits for approval.
    pub budget: BTreeMap<Currency, u64>,
    /// Reply length cap; `None` leaves the endpoint's default.
    pub max_tokens: Option<u32>,
}

/// The host's tools: what the model may call, and the means to look before calling.
pub trait Toolbox {
    /// The tools offered to the model.
    fn tools(&self) -> Vec<Tool>;
    /// Instructions about the tools, added to the system prompt.
    fn instructions(&self) -> String {
        String::new()
    }
    /// What calling `name` with `input` would do, without doing it. An error is passed to the
    /// model as the call's result, so it can correct itself.
    fn impact(&mut self, name: &str, input: &Value) -> Result<Impact, Error>;
    /// Makes the call.
    fn call(&mut self, name: &str, input: &Value) -> Result<Value, Error>;
    /// The call in a few words, for the timeline.
    fn label(&self, name: &str, _input: &Value) -> String {
        name.to_owned()
    }
}

/// Matches a reply to the request it answers; a reply to anything but the latest request is
/// ignored (it was cancelled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket(u64);

/// What the host should do next.
#[derive(Debug, Clone, PartialEq)]
pub enum Next {
    /// Send this request and pass the reply to [`Agent::on_reply`] with the ticket.
    Send(Ticket, ChatRequest),
    /// Show the planned steps and wait for [`Agent::approve`], [`Agent::decline`] or
    /// [`Agent::cancel`].
    Approval,
    /// Nothing until the person asks again.
    Idle,
    /// Nothing changed: a reply to a request that no longer matters, or an approval with
    /// nothing planned.
    Stale,
}

/// Where the agent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    /// Waiting for the model.
    Thinking,
    /// Waiting for the person to approve the planned steps.
    Waiting,
}

/// One tool call from a reply, not yet run.
#[derive(Debug, Clone)]
struct Call {
    id: String,
    name: String,
    input: Value,
    step: u64,
    /// What the call would do as the person was shown it, when that was known. Approval covers
    /// the call only if it still would do exactly that.
    shown: Option<Impact>,
}

#[derive(Debug)]
enum State {
    Idle,
    Thinking {
        ticket: Ticket,
        rounds: u32,
    },
    Waiting {
        rounds: u32,
        /// Results of the reply's calls that already ran (or were refused).
        done: Vec<Part>,
        /// The calls still to run, in order; the first is the one that needs approval.
        calls: VecDeque<Call>,
    },
}

/// The in-canvas agent for one session.
pub struct Agent {
    config: Config,
    conversation: Vec<Message>,
    steps: VecDeque<Step>,
    next_step: u64,
    next_ticket: u64,
    state: State,
    /// What runs accepted this session, per currency (their ceilings, not what they spent).
    accepted: BTreeMap<Currency, u64>,
    input_tokens: u64,
    output_tokens: u64,
}

impl Agent {
    pub fn new(config: Config) -> Self {
        Self {
            config,
            conversation: Vec::new(),
            steps: VecDeque::new(),
            next_step: 0,
            next_ticket: 0,
            state: State::Idle,
            accepted: BTreeMap::new(),
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Takes effect from the next tool call.
    pub fn set_mode(&mut self, mode: Mode) {
        self.config.mode = mode;
    }

    pub fn set_budget(&mut self, budget: BTreeMap<Currency, u64>) {
        self.config.budget = budget;
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        self.config.model = model.into();
    }

    pub fn status(&self) -> Status {
        match self.state {
            State::Idle => Status::Idle,
            State::Thinking { .. } => Status::Thinking,
            State::Waiting { .. } => Status::Waiting,
        }
    }

    pub fn timeline(&self) -> impl Iterator<Item = &Step> {
        self.steps.iter()
    }

    pub fn conversation(&self) -> &[Message] {
        &self.conversation
    }

    /// What runs accepted this session, per currency, in micro-units.
    pub fn accepted(&self) -> &BTreeMap<Currency, u64> {
        &self.accepted
    }

    /// Tokens the endpoint reported, in and out, this session.
    pub fn tokens(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }

    /// Starts over: forgets the conversation and the timeline. Only while idle.
    pub fn clear(&mut self) -> Result<(), Error> {
        if !matches!(self.state, State::Idle) {
            return Err(busy());
        }
        self.conversation.clear();
        self.steps.clear();
        Ok(())
    }

    /// The person's message: returns the first request to send.
    pub fn ask(&mut self, text: &str, focus: &Focus, toolbox: &dyn Toolbox) -> Result<Next, Error> {
        if !matches!(self.state, State::Idle) {
            return Err(busy());
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "say what you'd like the agent to do"));
        }
        if text.len() > MAX_ASK_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, format!("a message is at most {} KiB", MAX_ASK_BYTES / 1024)));
        }
        if self.config.model.trim().is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "choose a model for the agent first"));
        }
        self.trim_conversation();
        self.push_step(StepKind::Asked(text.to_owned()));
        self.push_user(vec![Part::Text(format!("{}{text}", prompt::focus(focus)))]);
        Ok(self.request(toolbox, 0))
    }

    /// The model's reply (or why there is none) to the request `ticket` named.
    pub fn on_reply(&mut self, ticket: Ticket, reply: Result<ChatResponse, ProviderError>, toolbox: &mut dyn Toolbox) -> Next {
        let rounds = match self.state {
            State::Thinking { ticket: current, rounds } if current == ticket => rounds,
            _ => return Next::Stale,
        };
        self.state = State::Idle;
        let reply = match reply {
            Ok(reply) => reply,
            Err(e) => {
                self.push_step(StepKind::Error(format!("The model couldn't answer: {}", e.message)));
                return Next::Idle;
            }
        };
        self.input_tokens = self.input_tokens.saturating_add(reply.usage.input_tokens.unwrap_or(0));
        self.output_tokens = self.output_tokens.saturating_add(reply.usage.output_tokens.unwrap_or(0));
        let mut message = reply.message;
        // Blank text would make the next request one the APIs refuse.
        message.parts.retain(|p| !matches!(p, Part::Text(t) if t.trim().is_empty()));
        if let Err(why) = sendable(&message) {
            self.push_step(StepKind::Error(why.into()));
            return Next::Idle;
        }
        let text = reply.text.trim();
        if !text.is_empty() {
            self.push_step(StepKind::Said(text.to_owned()));
        }
        let mut calls = VecDeque::new();
        let mut done = Vec::new();
        for part in &message.parts {
            let Part::ToolUse { id, name, input } = part else { continue };
            let label = prompt::clip(&toolbox.label(name, input), MAX_SUMMARY_CHARS);
            let step = self.push_step(StepKind::Tool(ToolStep { label, input: summary(input), impact: None, status: ToolStatus::Queued }));
            if calls.len() < MAX_CALLS_PER_REPLY {
                calls.push_back(Call { id: id.clone(), name: name.clone(), input: input.clone(), step, shown: None });
            } else {
                let why = format!("not run: at most {MAX_CALLS_PER_REPLY} tool calls run from one reply");
                self.set_status(step, ToolStatus::Failed(why.clone()));
                done.push(Part::ToolResult { id: id.clone(), content: why, is_error: true });
            }
        }
        let stop = reply.stop.clone();
        self.conversation.push(message);
        if calls.is_empty() && done.is_empty() {
            if stop == StopReason::MaxTokens {
                self.push_step(StepKind::Error("The reply was cut off at its length limit.".into()));
            }
            return Next::Idle;
        }
        self.proceed(rounds, done, calls, toolbox)
    }

    /// Runs the planned steps, then goes on. A step that would now do something other than
    /// what was shown is judged again, and may need approval of its own.
    pub fn approve(&mut self, toolbox: &mut dyn Toolbox) -> Next {
        let Some((rounds, done, calls)) = self.take_waiting() else { return Next::Stale };
        for call in &calls {
            self.set_status(call.step, ToolStatus::Queued);
        }
        self.proceed(rounds, done, calls, toolbox)
    }

    /// Refuses the planned steps; the model hears so and may suggest something else.
    pub fn decline(&mut self, toolbox: &mut dyn Toolbox) -> Next {
        let Some((rounds, mut done, calls)) = self.take_waiting() else { return Next::Stale };
        for call in calls {
            self.set_status(call.step, ToolStatus::Declined);
            done.push(Part::ToolResult { id: call.id, content: "The person declined this step; nothing was done.".into(), is_error: true });
        }
        self.push_user(done);
        self.request(toolbox, rounds)
    }

    /// Stops whatever is in progress (Esc). A reply still on its way is ignored when it comes.
    pub fn cancel(&mut self) {
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => {}
            State::Thinking { .. } => {
                self.push_step(StepKind::Cancelled);
            }
            State::Waiting { mut done, calls, .. } => {
                for call in calls {
                    self.set_status(call.step, ToolStatus::Cancelled);
                    done.push(Part::ToolResult { id: call.id, content: "The person cancelled before this step ran.".into(), is_error: true });
                }
                // Every call keeps its result, so the conversation stays one the APIs accept.
                self.push_user(done);
                self.push_step(StepKind::Cancelled);
            }
        }
    }

    /// The planned steps, leaving the agent idle; `None` (and nothing changed) when there are none.
    fn take_waiting(&mut self) -> Option<(u32, Vec<Part>, VecDeque<Call>)> {
        if !matches!(self.state, State::Waiting { .. }) {
            return None;
        }
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Waiting { rounds, done, calls } => Some((rounds, done, calls)),
            _ => None,
        }
    }

    /// Runs `calls` in order, each judged just before it runs (an earlier call may have made
    /// the next one possible), until one needs approval or all have run.
    fn proceed(&mut self, rounds: u32, mut done: Vec<Part>, mut calls: VecDeque<Call>, toolbox: &mut dyn Toolbox) -> Next {
        while let Some(mut call) = calls.pop_front() {
            let impact = match toolbox.impact(&call.name, &call.input) {
                Ok(impact) => impact,
                Err(e) => {
                    let message = cut(error_text(&e), MAX_RESULT_BYTES);
                    self.set_status(call.step, ToolStatus::Failed(prompt::clip(&message, MAX_SUMMARY_CHARS)));
                    done.push(Part::ToolResult { id: call.id, content: message, is_error: true });
                    continue;
                }
            };
            self.set_impact(call.step, impact.clone());
            let approved = call.shown.as_ref() == Some(&impact);
            if !approved && !self.allowed(&impact) {
                self.set_status(call.step, ToolStatus::Planned);
                call.shown = Some(impact);
                // Show what the rest would do too, as far as it can be known before this runs;
                // one that can't be known yet is judged again when its turn comes.
                for later in &mut calls {
                    later.shown = toolbox.impact(&later.name, &later.input).ok();
                    match &later.shown {
                        Some(impact) => self.set_impact(later.step, impact.clone()),
                        None => self.clear_impact(later.step),
                    }
                    self.set_status(later.step, ToolStatus::Planned);
                }
                calls.push_front(call);
                self.state = State::Waiting { rounds, done, calls };
                return Next::Approval;
            }
            if let Impact::Run(run) = &impact {
                self.charge(run);
            }
            let (content, is_error) = match toolbox.call(&call.name, &call.input) {
                Ok(value) => {
                    let text = value.to_string();
                    self.set_status(call.step, ToolStatus::Ran(prompt::clip(&text, MAX_SUMMARY_CHARS)));
                    (cut(text, MAX_RESULT_BYTES), false)
                }
                Err(e) => {
                    let message = cut(error_text(&e), MAX_RESULT_BYTES);
                    self.set_status(call.step, ToolStatus::Failed(prompt::clip(&message, MAX_SUMMARY_CHARS)));
                    (message, true)
                }
            };
            done.push(Part::ToolResult { id: call.id, content, is_error });
        }
        self.push_user(done);
        self.request(toolbox, rounds)
    }

    /// Whether a call with `impact` may run without asking.
    fn allowed(&self, impact: &Impact) -> bool {
        match (impact, self.config.mode) {
            (Impact::Read, _) => true,
            (_, Mode::Assist) => false,
            (Impact::Edit | Impact::Host, Mode::Auto) => true,
            (Impact::Run(run), Mode::Auto) => {
                !run.partial && run.unpriced_jobs == 0 && run.ceiling.iter().all(|(currency, amount)| *amount <= self.remaining(currency))
            }
        }
    }

    fn remaining(&self, currency: &Currency) -> u64 {
        let budget = self.config.budget.get(currency).copied().unwrap_or(0);
        budget.saturating_sub(self.accepted.get(currency).copied().unwrap_or(0))
    }

    fn charge(&mut self, run: &RunImpact) {
        for (currency, amount) in &run.ceiling {
            let total = self.accepted.entry(*currency).or_insert(0);
            *total = total.saturating_add(*amount);
        }
    }

    fn request(&mut self, toolbox: &dyn Toolbox, rounds: u32) -> Next {
        if rounds >= MAX_ROUNDS {
            self.push_step(StepKind::Error(format!("Stopped after {MAX_ROUNDS} steps. Ask again to carry on.")));
            return Next::Idle;
        }
        let ticket = Ticket(self.next_ticket);
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.state = State::Thinking { ticket, rounds: rounds.saturating_add(1) };
        let request = ChatRequest {
            model: self.config.model.clone(),
            system: Some(prompt::system(self.config.mode, &toolbox.instructions())),
            messages: self.conversation.clone(),
            max_tokens: self.config.max_tokens,
            temperature: None,
            tools: toolbox.tools(),
        };
        Next::Send(ticket, request)
    }

    /// Adds to the last message when it is the person's (tool results, then their next words),
    /// so turns keep alternating.
    fn push_user(&mut self, parts: Vec<Part>) {
        if parts.is_empty() {
            return;
        }
        match self.conversation.last_mut() {
            Some(last) if last.role == Role::User => last.parts.extend(parts),
            _ => self.conversation.push(Message { role: Role::User, parts }),
        }
    }

    /// Drops the oldest turns, whole, until the conversation fits: it restarts at a message of
    /// the person's that answers no tool call, so no call loses its result.
    fn trim_conversation(&mut self) {
        while !self.conversation.is_empty()
            && (self.conversation.len() >= MAX_MESSAGES || self.conversation.iter().map(message_bytes).fold(0, usize::saturating_add) > MAX_CONVERSATION_BYTES)
        {
            let start = self
                .conversation
                .iter()
                .enumerate()
                .skip(1)
                .find(|(_, m)| m.role == Role::User && !m.parts.iter().any(|p| matches!(p, Part::ToolResult { .. })))
                .map(|(i, _)| i);
            match start {
                Some(start) => {
                    self.conversation.drain(..start);
                }
                None => self.conversation.clear(),
            }
        }
    }

    fn push_step(&mut self, kind: StepKind) -> u64 {
        let id = self.next_step;
        self.next_step = self.next_step.wrapping_add(1);
        if self.steps.len() >= MAX_STEPS {
            self.steps.pop_front();
        }
        self.steps.push_back(Step { id, kind });
        id
    }

    fn tool_step(&mut self, id: u64) -> Option<&mut ToolStep> {
        self.steps.iter_mut().rev().find(|s| s.id == id).and_then(|s| match &mut s.kind {
            StepKind::Tool(tool) => Some(tool),
            _ => None,
        })
    }

    fn set_status(&mut self, id: u64, status: ToolStatus) {
        if let Some(tool) = self.tool_step(id) {
            tool.status = status;
        }
    }

    fn set_impact(&mut self, id: u64, impact: Impact) {
        if let Some(tool) = self.tool_step(id) {
            tool.impact = Some(impact);
        }
    }

    fn clear_impact(&mut self, id: u64) {
        if let Some(tool) = self.tool_step(id) {
            tool.impact = None;
        }
    }
}

fn busy() -> Error {
    Error::new(ErrorKind::Conflict, "the agent is still working; wait, or press Esc to stop it")
}

/// An error as the model reads it: its kind, then the message.
fn error_text(e: &Error) -> String {
    let kind = serde_json::to_value(e.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_else(|| format!("{:?}", e.kind));
    format!("{kind}: {}", e.message)
}

/// Whether a reply can join the conversation: the model said something or called a tool, and
/// its call ids are ones the chat APIs accept back.
fn sendable(message: &Message) -> Result<(), &'static str> {
    if message.role != Role::Assistant {
        return Err("The model's reply wasn't an assistant message.");
    }
    if !message.parts.iter().any(|p| matches!(p, Part::Text(_) | Part::ToolUse { .. })) {
        return Err("The model sent an empty reply.");
    }
    let mut ids = std::collections::HashSet::new();
    for part in &message.parts {
        match part {
            Part::ToolUse { id, .. } if id.is_empty() || id.len() > MAX_TOOL_ID || !ids.insert(id.as_str()) => {
                return Err("The model's reply named its tool calls in a way that can't be answered; ask again.");
            }
            Part::ToolResult { .. } | Part::Image { .. } => return Err("The model's reply carried parts only the person can send."),
            _ => {}
        }
    }
    Ok(())
}

/// About how much a message adds to every later request, in bytes.
fn message_bytes(message: &Message) -> usize {
    message
        .parts
        .iter()
        .map(|p| match p {
            Part::Text(t) => t.len(),
            Part::Image { bytes, .. } => bytes.len(),
            Part::ToolUse { id, name, input } => id.len().saturating_add(name.len()).saturating_add(input.to_string().len()),
            Part::ToolResult { id, content, .. } => id.len().saturating_add(content.len()),
            Part::Opaque { block, .. } => block.to_string().len(),
        })
        .fold(0, usize::saturating_add)
}

fn summary(input: &Value) -> String {
    prompt::clip(&input.to_string(), MAX_SUMMARY_CHARS)
}

/// `text` cut to at most `max` bytes at a character boundary, saying how much was left out.
pub(crate) fn cut(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let rest = text.len().saturating_sub(end);
    let mut out = text.get(..end).unwrap_or_default().to_owned();
    out.push_str(&format!("… [cut: {rest} more bytes; ask for less at a time]"));
    out
}
