//! The loop against a scripted model and a fake toolbox: no endpoint, no session.
use crate::*;
use nodecraft_providers::llm::{ChatResponse, Message, Part, Role, StopReason, Tool};
use nodecraft_providers::{ErrorKind as ProviderErrorKind, ProviderError, Usage};
use nodecraft_types::{CanvasId, Currency, Error, ErrorKind, NodeId};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const USD: Currency = Currency::USD;

/// `look` reads, `change` edits (and makes `connect` possible), `host` asks the host, `spend`
/// runs at `{usd, partial, unpriced}`, `broken` can't be judged.
#[derive(Default)]
struct Fake {
    calls: Vec<(String, Value)>,
    judged: Vec<String>,
    added: bool,
}

impl Toolbox for Fake {
    fn tools(&self) -> Vec<Tool> {
        ["look", "change", "host", "spend", "connect", "broken", "big", "late"]
            .iter()
            .map(|n| Tool { name: (*n).into(), description: format!("the {n} tool"), input_schema: json!({"type": "object"}), strict: false })
            .collect()
    }
    fn instructions(&self) -> String {
        "Fake toolbox instructions.".into()
    }
    fn impact(&mut self, name: &str, input: &Value) -> Result<Impact, Error> {
        self.judged.push(name.into());
        match name {
            "look" | "big" => Ok(Impact::Read),
            "change" => Ok(Impact::Edit),
            "host" => Ok(Impact::Host),
            "spend" => Ok(Impact::Run(RunImpact {
                ceiling: BTreeMap::from([(USD, input["usd"].as_u64().unwrap_or(0))]),
                partial: input["partial"].as_bool().unwrap_or(false),
                unpriced_jobs: input["unpriced"].as_u64().unwrap_or(0),
            })),
            "connect" if self.added => Ok(Impact::Edit),
            // A run that can only be priced once `change` has run.
            "late" if self.added => {
                Ok(Impact::Run(RunImpact { ceiling: BTreeMap::from([(USD, input["usd"].as_u64().unwrap_or(0))]), partial: false, unpriced_jobs: 0 }))
            }
            "late" => Err(Error::new(ErrorKind::Conflict, "the run changed since its quote; quote again")),
            "connect" => Err(Error::new(ErrorKind::NotFound, "node does not exist")),
            _ => Err(Error::new(ErrorKind::InvalidInput, "no such tool")),
        }
    }
    fn call(&mut self, name: &str, input: &Value) -> Result<Value, Error> {
        self.calls.push((name.into(), input.clone()));
        match name {
            "change" => {
                self.added = true;
                Ok(json!({"revision": 1}))
            }
            "big" => Ok(json!({"text": "é".repeat(MAX_RESULT_BYTES)})),
            _ => Ok(json!({"ok": name})),
        }
    }
    fn label(&self, name: &str, input: &Value) -> String {
        input["id"].as_str().map_or_else(|| name.to_owned(), str::to_owned)
    }
}

fn agent(mode: Mode) -> Agent {
    Agent::new(Config { model: "fixture-model".into(), mode, budget: BTreeMap::from([(USD, 50_000)]), max_tokens: Some(1000) })
}

fn call(id: &str, name: &str, input: Value) -> Part {
    Part::ToolUse { id: id.into(), name: name.into(), input }
}

fn reply(parts: Vec<Part>) -> Result<ChatResponse, ProviderError> {
    let stop = if parts.iter().any(|p| matches!(p, Part::ToolUse { .. })) { StopReason::ToolUse } else { StopReason::EndTurn };
    let message = Message { role: Role::Assistant, parts };
    let mut usage = Usage::default();
    usage.input_tokens = Some(10);
    usage.output_tokens = Some(5);
    Ok(ChatResponse { text: message.text(), message, stop, usage })
}

fn say(text: &str) -> Result<ChatResponse, ProviderError> {
    reply(vec![Part::Text(text.into())])
}

fn sent(next: Next) -> (Ticket, nodecraft_providers::llm::ChatRequest) {
    match next {
        Next::Send(ticket, request) => (ticket, request),
        other => panic!("expected a request, got {other:?}"),
    }
}

/// The results in the request's last (user) message, in order: (id, is_error, content).
fn results(request: &nodecraft_providers::llm::ChatRequest) -> Vec<(String, bool, String)> {
    let last = request.messages.last().unwrap();
    assert_eq!(last.role, Role::User);
    last.parts
        .iter()
        .filter_map(|p| match p {
            Part::ToolResult { id, content, is_error } => Some((id.clone(), *is_error, content.clone())),
            _ => None,
        })
        .collect()
}

fn statuses(agent: &Agent) -> Vec<(String, ToolStatus)> {
    agent
        .timeline()
        .filter_map(|s| match &s.kind {
            StepKind::Tool(t) => Some((t.label.clone(), t.status.clone())),
            _ => None,
        })
        .collect()
}

fn ask(agent: &mut Agent, tools: &Fake, text: &str) -> (Ticket, nodecraft_providers::llm::ChatRequest) {
    sent(agent.ask(text, &Focus::default(), tools).unwrap())
}

#[test]
fn the_first_request_carries_the_prompt_the_tools_and_the_question() {
    let tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (_, request) = ask(&mut a, &tools, "  make a moodboard  ");
    assert_eq!(request.model, "fixture-model");
    assert_eq!(request.max_tokens, Some(1000));
    assert_eq!(request.tools.len(), 8);
    let system = request.system.unwrap();
    assert!(system.contains("assist mode") && system.ends_with("Fake toolbox instructions."), "{system}");
    assert_eq!(request.messages, vec![Message::user("make a moodboard")]);
    assert_eq!(a.status(), Status::Thinking);
    assert!(matches!(a.timeline().next().map(|s| &s.kind), Some(StepKind::Asked(t)) if t == "make a moodboard"));
}

#[test]
fn assist_reads_at_once_and_waits_before_a_change() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "rename it");
    let next = a.on_reply(
        ticket,
        reply(vec![Part::Text("Looking first.".into()), call("1", "look", json!({})), call("2", "change", json!({"id": "node.title"}))]),
        &mut tools,
    );
    assert_eq!(next, Next::Approval);
    assert_eq!(a.status(), Status::Waiting);
    assert_eq!(tools.calls.len(), 1, "the read ran; the change waits");
    assert_eq!(statuses(&a), vec![("look".into(), ToolStatus::Ran("{\"ok\":\"look\"}".into())), ("node.title".into(), ToolStatus::Planned)]);
    let planned = a.timeline().find_map(|s| match &s.kind {
        StepKind::Tool(t) if t.status == ToolStatus::Planned => t.impact.clone(),
        _ => None,
    });
    assert_eq!(planned, Some(Impact::Edit), "the plan says what the step would do");

    let (ticket, request) = sent(a.approve(&mut tools));
    assert_eq!(tools.calls.len(), 2);
    let results = results(&request);
    assert_eq!(results.iter().map(|r| (r.0.as_str(), r.1)).collect::<Vec<_>>(), vec![("1", false), ("2", false)]);
    assert_eq!(a.on_reply(ticket, say("Renamed."), &mut tools), Next::Idle);
    assert_eq!(a.status(), Status::Idle);
    assert_eq!(a.tokens(), (20, 10));
}

#[test]
fn each_call_is_judged_just_before_it_runs() {
    // `connect` is only possible once `change` has run; judging it up front would refuse it.
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "add and wire");
    let (_, request) = sent(a.on_reply(ticket, reply(vec![call("1", "change", json!({})), call("2", "connect", json!({}))]), &mut tools));
    assert_eq!(tools.calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), vec!["change", "connect"]);
    assert!(results(&request).iter().all(|r| !r.1));
}

#[test]
fn approving_a_plan_runs_the_rest_of_the_reply_too() {
    let mut tools = Fake { added: true, ..Default::default() };
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "build it");
    let parts = vec![call("1", "change", json!({})), call("2", "connect", json!({})), call("3", "spend", json!({"usd": 10_000}))];
    assert_eq!(a.on_reply(ticket, reply(parts), &mut tools), Next::Approval);
    assert!(statuses(&a).iter().all(|s| s.1 == ToolStatus::Planned), "{:?}", statuses(&a));
    let (_, request) = sent(a.approve(&mut tools));
    assert_eq!(tools.calls.len(), 3, "one approval covers the reply's plan");
    assert_eq!(results(&request).len(), 3);
    assert_eq!(a.accepted(), &BTreeMap::from([(USD, 10_000)]));
}

#[test]
fn a_step_unknown_when_approved_is_asked_about_when_it_is_known() {
    // Assist: `connect` and the run can't be judged until `change` has run, so approving the
    // change doesn't approve them.
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "build and run");
    let parts = vec![call("1", "change", json!({})), call("2", "connect", json!({})), call("3", "late", json!({"usd": 5_000_000}))];
    assert_eq!(a.on_reply(ticket, reply(parts), &mut tools), Next::Approval);
    let shown: Vec<Option<Impact>> = a
        .timeline()
        .filter_map(|s| match &s.kind {
            StepKind::Tool(t) => Some(t.impact.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(shown, vec![Some(Impact::Edit), None, None], "nothing is claimed for what can't be known");
    assert_eq!(a.approve(&mut tools), Next::Approval, "connect is asked about once it is known");
    assert_eq!(tools.calls.len(), 1);
    assert_eq!(statuses(&a)[1].1, ToolStatus::Planned);
    let late = a.timeline().find_map(|s| match &s.kind {
        StepKind::Tool(t) if t.label == "late" => t.impact.clone(),
        _ => None,
    });
    assert!(matches!(late, Some(Impact::Run(_))), "the run's price is shown before it is approved: {late:?}");
    sent(a.approve(&mut tools));
    assert_eq!(tools.calls.len(), 3);

    // Auto: approving an over-budget run doesn't wave through a later run priced only after it.
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "run twice");
    let parts = vec![call("1", "spend", json!({"usd": 60_000})), call("2", "change", json!({})), call("3", "late", json!({"usd": 60_000}))];
    assert_eq!(a.on_reply(ticket, reply(parts), &mut tools), Next::Approval);
    assert_eq!(a.approve(&mut tools), Next::Approval, "the second run is over budget too");
    assert_eq!(tools.calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), vec!["spend", "change"]);
    assert_eq!(a.accepted(), &BTreeMap::from([(USD, 60_000)]));
}

#[test]
fn a_blank_or_malformed_reply_leaves_the_conversation_sendable() {
    let bad = [
        reply(vec![Part::Text("\n\n".into())]),
        reply(vec![call("1", "look", json!({})), call("1", "look", json!({}))]),
        reply(vec![call("", "look", json!({}))]),
        reply(vec![call(&"x".repeat(300), "look", json!({}))]),
    ];
    for answer in bad {
        let mut tools = Fake::default();
        let mut a = agent(Mode::Auto);
        let (ticket, _) = ask(&mut a, &tools, "go");
        assert_eq!(a.on_reply(ticket, answer, &mut tools), Next::Idle);
        assert!(tools.calls.is_empty());
        assert!(matches!(a.timeline().last().map(|s| &s.kind), Some(StepKind::Error(_))));
        assert_eq!(a.conversation().len(), 1, "nothing was added");
        let (_, request) = ask(&mut a, &tools, "try again");
        assert_eq!(request.messages.len(), 1);
    }
    // Blank text beside a call is dropped; the call stands.
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "go");
    let (_, request) = sent(a.on_reply(ticket, reply(vec![Part::Text(" ".into()), call("1", "look", json!({}))]), &mut tools));
    assert_eq!(request.messages[1].parts.len(), 1);
}

#[test]
fn large_earlier_turns_are_dropped_by_size() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    for i in 0..(MAX_CONVERSATION_BYTES / MAX_RESULT_BYTES + 8) {
        let (ticket, _) = ask(&mut a, &tools, &format!("q{i}"));
        let (ticket, _) = sent(a.on_reply(ticket, reply(vec![call(&format!("t{i}"), "big", json!({}))]), &mut tools));
        a.on_reply(ticket, say("ok"), &mut tools);
    }
    let (_, request) = ask(&mut a, &tools, "last");
    let bytes: usize = request
        .messages
        .iter()
        .map(|m| {
            m.parts
                .iter()
                .map(|p| match p {
                    Part::ToolResult { content, .. } => content.len(),
                    _ => 0,
                })
                .sum::<usize>()
        })
        .sum();
    assert!(bytes <= MAX_CONVERSATION_BYTES + MAX_RESULT_BYTES * 2, "{bytes}");
    assert!(matches!(&request.messages[0].parts[0], Part::Text(t) if t.starts_with('q')));
}

#[test]
fn auto_runs_within_the_budget_and_asks_beyond_it() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "run it twice");
    let (ticket, _) = sent(a.on_reply(ticket, reply(vec![call("1", "change", json!({})), call("2", "spend", json!({"usd": 30_000}))]), &mut tools));
    assert_eq!(tools.calls.len(), 2, "an edit and a run within budget go ahead");
    assert_eq!(a.accepted(), &BTreeMap::from([(USD, 30_000)]));
    // 30 000 of 50 000 are taken: another 30 000 doesn't fit.
    assert_eq!(a.on_reply(ticket, reply(vec![call("3", "spend", json!({"usd": 30_000}))]), &mut tools), Next::Approval);
    assert_eq!(tools.calls.len(), 2);
    let (ticket, _) = sent(a.approve(&mut tools));
    assert_eq!(a.accepted(), &BTreeMap::from([(USD, 60_000)]), "an approved run counts too");
    // Exactly what is left still fits; nothing is left now.
    assert_eq!(a.on_reply(ticket, reply(vec![call("4", "spend", json!({"usd": 1}))]), &mut tools), Next::Approval);
}

#[test]
fn auto_asks_for_runs_of_unknown_price_or_without_a_budget() {
    for input in [json!({"usd": 1, "partial": true}), json!({"usd": 0, "unpriced": 1})] {
        let mut tools = Fake::default();
        let mut a = agent(Mode::Auto);
        let (ticket, _) = ask(&mut a, &tools, "run");
        assert_eq!(a.on_reply(ticket, reply(vec![call("1", "spend", input.clone())]), &mut tools), Next::Approval, "{input}");
        assert!(tools.calls.is_empty());
    }
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    a.set_budget(BTreeMap::new());
    let (ticket, _) = ask(&mut a, &tools, "run");
    assert_eq!(a.on_reply(ticket, reply(vec![call("1", "spend", json!({"usd": 1}))]), &mut tools), Next::Approval);
    // A free run fits any budget.
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    a.set_budget(BTreeMap::new());
    let (ticket, _) = ask(&mut a, &tools, "run");
    assert!(matches!(a.on_reply(ticket, reply(vec![call("1", "spend", json!({"usd": 0}))]), &mut tools), Next::Send(..)));
}

#[test]
fn assist_asks_before_host_requests_and_auto_does_not() {
    for (mode, expect_wait) in [(Mode::Assist, true), (Mode::Auto, false)] {
        let mut tools = Fake::default();
        let mut a = agent(mode);
        let (ticket, _) = ask(&mut a, &tools, "fetch the model list");
        let next = a.on_reply(ticket, reply(vec![call("1", "host", json!({}))]), &mut tools);
        assert_eq!(next == Next::Approval, expect_wait, "{mode:?}");
    }
}

#[test]
fn declining_tells_the_model_and_asks_it_again() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "delete everything");
    a.on_reply(ticket, reply(vec![call("1", "look", json!({})), call("2", "change", json!({})), call("3", "change", json!({}))]), &mut tools);
    let (_, request) = sent(a.decline(&mut tools));
    assert_eq!(tools.calls.len(), 1, "only the read ran");
    let results = results(&request);
    assert_eq!(results.iter().map(|r| (r.0.as_str(), r.1)).collect::<Vec<_>>(), vec![("1", false), ("2", true), ("3", true)]);
    assert!(results[1].2.contains("declined"));
    assert_eq!(statuses(&a)[1].1, ToolStatus::Declined);
}

#[test]
fn cancelling_a_plan_answers_every_call_and_the_next_question_follows_on() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "change it");
    a.on_reply(ticket, reply(vec![call("1", "change", json!({}))]), &mut tools);
    a.cancel();
    assert_eq!(a.status(), Status::Idle);
    assert!(tools.calls.is_empty());
    assert_eq!(statuses(&a)[0].1, ToolStatus::Cancelled);
    assert!(matches!(a.timeline().last().map(|s| &s.kind), Some(StepKind::Cancelled)));
    // The next message joins the cancelled call's result, so turns keep alternating.
    let (_, request) = ask(&mut a, &tools, "never mind, just look");
    let roles: Vec<Role> = request.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::User]);
    let last = &request.messages[2].parts;
    assert!(matches!(&last[0], Part::ToolResult { id, is_error: true, content } if id == "1" && content.contains("cancelled")));
    assert_eq!(last[1], Part::Text("never mind, just look".into()));
}

#[test]
fn a_reply_after_cancel_is_ignored() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "go");
    a.cancel();
    assert_eq!(a.on_reply(ticket, reply(vec![call("1", "change", json!({}))]), &mut tools), Next::Stale);
    assert!(tools.calls.is_empty());
    // A newer request's ticket is the only one that counts.
    let (old, _) = ask(&mut a, &tools, "again");
    a.cancel();
    let (new, _) = ask(&mut a, &tools, "and again");
    assert_ne!(old, new);
    assert_eq!(a.on_reply(old, say("late"), &mut tools), Next::Stale);
    assert_eq!(a.on_reply(new, say("done"), &mut tools), Next::Idle);
}

#[test]
fn approve_and_decline_without_a_plan_change_nothing() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    assert_eq!(a.approve(&mut tools), Next::Stale);
    let (ticket, _) = ask(&mut a, &tools, "go");
    assert_eq!(a.approve(&mut tools), Next::Stale);
    assert_eq!(a.decline(&mut tools), Next::Stale);
    assert_eq!(a.status(), Status::Thinking, "still waiting for the model");
    assert_eq!(a.on_reply(ticket, say("ok"), &mut tools), Next::Idle);
}

#[test]
fn a_call_that_cant_be_judged_is_reported_to_the_model_not_run() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "go");
    let (_, request) = sent(a.on_reply(ticket, reply(vec![call("1", "broken", json!({}))]), &mut tools));
    assert!(tools.calls.is_empty());
    let results = results(&request);
    assert!(results[0].1);
    assert_eq!(results[0].2, "invalid_input: no such tool");
}

#[test]
fn asking_while_busy_or_with_nothing_to_say_is_refused() {
    let tools = Fake::default();
    let mut a = agent(Mode::Assist);
    assert_eq!(a.ask("   ", &Focus::default(), &tools).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(a.ask(&"x".repeat(MAX_ASK_BYTES + 1), &Focus::default(), &tools).unwrap_err().kind, ErrorKind::InvalidInput);
    ask(&mut a, &tools, "go");
    assert_eq!(a.ask("more", &Focus::default(), &tools).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(a.clear().unwrap_err().kind, ErrorKind::Conflict);
    let mut unset = Agent::new(Config { model: " ".into(), mode: Mode::Assist, budget: BTreeMap::new(), max_tokens: None });
    assert!(unset.ask("go", &Focus::default(), &tools).unwrap_err().message.contains("model"));
}

#[test]
fn a_failed_request_is_shown_and_the_conversation_goes_on() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "go");
    let next = a.on_reply(ticket, Err(ProviderError::new(ProviderErrorKind::RateLimited, "slow down")), &mut tools);
    assert_eq!(next, Next::Idle);
    assert!(matches!(a.timeline().last().map(|s| &s.kind), Some(StepKind::Error(e)) if e.contains("slow down")));
    let (_, request) = ask(&mut a, &tools, "try again");
    assert_eq!(request.messages.len(), 1, "both questions in one message");
    assert_eq!(request.messages[0].parts.len(), 2);
}

#[test]
fn a_reply_cut_off_says_so() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "write a lot");
    let message = Message::assistant("Once upon");
    let cut = Ok(ChatResponse { text: message.text(), message, stop: StopReason::MaxTokens, usage: Usage::default() });
    assert_eq!(a.on_reply(ticket, cut, &mut tools), Next::Idle);
    assert!(matches!(a.timeline().last().map(|s| &s.kind), Some(StepKind::Error(e)) if e.contains("cut off")));
}

#[test]
fn an_empty_reply_is_an_error_not_a_turn() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "go");
    assert_eq!(a.on_reply(ticket, reply(Vec::new()), &mut tools), Next::Idle);
    assert_eq!(a.conversation().len(), 1, "nothing to append");
}

#[test]
fn the_loop_stops_after_its_round_limit() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (mut ticket, _) = ask(&mut a, &tools, "loop forever");
    let mut rounds = 1;
    loop {
        match a.on_reply(ticket, reply(vec![call(&format!("c{rounds}"), "look", json!({}))]), &mut tools) {
            Next::Send(next, _) => {
                ticket = next;
                rounds += 1;
            }
            Next::Idle => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(rounds, MAX_ROUNDS);
    assert!(matches!(a.timeline().last().map(|s| &s.kind), Some(StepKind::Error(e)) if e.contains("Stopped")));
    // The calls of the last reply still have their results, so the next question can follow on.
    let (_, request) = ask(&mut a, &tools, "carry on");
    assert_eq!(request.messages.last().unwrap().parts.len(), 2);
}

#[test]
fn too_many_calls_in_one_reply_are_refused_past_the_limit() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "go");
    let parts = (0..MAX_CALLS_PER_REPLY + 3).map(|i| call(&i.to_string(), "look", json!({}))).collect();
    let (_, request) = sent(a.on_reply(ticket, reply(parts), &mut tools));
    assert_eq!(tools.calls.len(), MAX_CALLS_PER_REPLY);
    let results = results(&request);
    assert_eq!(results.len(), MAX_CALLS_PER_REPLY + 3, "every call is answered");
    assert_eq!(results.iter().filter(|r| r.1).count(), 3);
}

#[test]
fn a_long_result_is_cut_at_a_character_boundary() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let (ticket, _) = ask(&mut a, &tools, "read it all");
    let (_, request) = sent(a.on_reply(ticket, reply(vec![call("1", "big", json!({}))]), &mut tools));
    let content = &results(&request)[0].2;
    assert!(content.contains("[cut:"), "{}", &content[content.len() - 80..]);
    assert!(content.len() <= MAX_RESULT_BYTES + 80);
    let summary = match &statuses(&a)[0].1 {
        ToolStatus::Ran(summary) => summary.clone(),
        other => panic!("{other:?}"),
    };
    assert!(summary.chars().count() <= MAX_SUMMARY_CHARS + 1);
}

#[test]
fn old_turns_are_dropped_whole() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Auto);
    let mut i = 0;
    while a.conversation().len() < MAX_MESSAGES {
        let (ticket, _) = ask(&mut a, &tools, &format!("q{i}"));
        let (ticket, _) = sent(a.on_reply(ticket, reply(vec![call(&format!("t{i}"), "look", json!({}))]), &mut tools));
        a.on_reply(ticket, say("ok"), &mut tools);
        i += 1;
    }
    let (_, request) = ask(&mut a, &tools, "last");
    assert!(request.messages.len() < MAX_MESSAGES);
    let first = &request.messages[0];
    assert_eq!(first.role, Role::User);
    assert!(matches!(&first.parts[0], Part::Text(t) if t.starts_with('q')), "a turn starts at a question: {first:?}");
    // Every tool call kept still has its result in the next message.
    for (n, m) in request.messages.iter().enumerate() {
        for p in &m.parts {
            if let Part::ToolUse { id, .. } = p {
                let answered = request.messages.get(n + 1).is_some_and(|r| r.parts.iter().any(|q| matches!(q, Part::ToolResult { id: rid, .. } if rid == id)));
                assert!(answered, "call {id} lost its result");
            }
        }
    }
}

#[test]
fn the_timeline_is_bounded() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    for i in 0..MAX_STEPS {
        let (ticket, _) = ask(&mut a, &tools, &format!("q{i}"));
        a.on_reply(ticket, say("a"), &mut tools);
    }
    assert_eq!(a.timeline().count(), MAX_STEPS);
    let first = a.timeline().next().unwrap();
    assert_eq!(first.id, MAX_STEPS as u64, "the oldest steps went first");
    a.clear().unwrap();
    assert_eq!(a.timeline().count(), 0);
    assert!(a.conversation().is_empty());
}

#[test]
fn the_focus_names_the_canvas_and_a_bounded_selection() {
    let tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let canvas = CanvasId::from_bytes([1; 16]);
    let selection = (0..MAX_FOCUS_NODES + 5)
        .map(|i| FocusNode {
            id: NodeId::from_bytes([i as u8; 16]),
            kind: "generate.image".into(),
            title: if i == 0 { "Line\nbreak".into() } else { "x".repeat(500) },
        })
        .collect();
    let (_, request) = sent(a.ask("tidy these", &Focus { canvas: Some(canvas), selection }, &tools).unwrap());
    let text = request.messages[0].text();
    assert!(text.contains(&canvas.to_string()));
    assert!(text.contains("\"Line break\""), "a title can't start a line of its own: {text}");
    assert!(text.contains("and 5 more"));
    assert_eq!(text.matches("generate.image").count(), MAX_FOCUS_NODES);
    assert!(text.len() < 20_000, "{}", text.len());
    assert!(text.ends_with("tidy these"));
}

#[test]
fn switching_mode_applies_to_the_next_call() {
    let mut tools = Fake::default();
    let mut a = agent(Mode::Assist);
    let (ticket, _) = ask(&mut a, &tools, "go");
    assert_eq!(a.on_reply(ticket, reply(vec![call("1", "change", json!({}))]), &mut tools), Next::Approval);
    a.set_mode(Mode::Auto);
    let (ticket, request) = sent(a.approve(&mut tools));
    assert!(request.system.unwrap().contains("auto mode"));
    assert!(matches!(a.on_reply(ticket, reply(vec![call("2", "change", json!({}))]), &mut tools), Next::Send(..)));
    assert_eq!(tools.calls.len(), 2);
}
