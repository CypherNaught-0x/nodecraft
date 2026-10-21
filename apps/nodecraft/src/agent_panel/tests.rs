use super::*;
use nodecraft_engine::settings::AgentEndpoint;
use nodecraft_providers::llm::{ChatResponse, Message, Part, Role, StopReason};
use nodecraft_providers::{ProviderError, Usage};
use serde_json::{Value, json};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Reply = Result<ChatResponse, ProviderError>;

fn reply(parts: Vec<Part>) -> Reply {
    let stop = if parts.iter().any(|p| matches!(p, Part::ToolUse { .. })) { StopReason::ToolUse } else { StopReason::EndTurn };
    let message = Message { role: Role::Assistant, parts };
    Ok(ChatResponse { text: message.text(), message, stop, usage: Usage::default() })
}

fn say(text: &str) -> Reply {
    reply(vec![Part::Text(text.into())])
}

fn tool(name: &str, input: Value) -> Reply {
    reply(vec![Part::ToolUse { id: format!("call-{name}"), name: name.into(), input }])
}

/// A model that answers each request with the next scripted reply, once the test releases it,
/// and records what it was sent.
struct Script {
    release: Sender<Reply>,
    sent: Arc<Mutex<Vec<(AgentSettings, nodecraft_providers::llm::ChatRequest)>>>,
}

fn panel(settings: AgentSettings) -> (AgentPanel, Script) {
    let (release, replies): (Sender<Reply>, Receiver<Reply>) = channel();
    let sent = Arc::new(Mutex::new(Vec::new()));
    let log = sent.clone();
    let send: SendFn = Box::new(move |settings, request, _still| {
        log.lock().unwrap().push((settings.clone(), request.clone()));
        replies.recv().unwrap_or_else(|_| Err(ProviderError::new(nodecraft_providers::ErrorKind::Network, "script ended")))
    });
    let worker = Worker::spawn(send, Arc::new(|| {})).unwrap();
    let mut panel = AgentPanel::new(Ok(worker), settings);
    panel.open = true;
    (panel, Script { release, sent })
}

/// Pumps the panel until `done` holds (the worker answers on its own thread).
fn pump_until(panel: &mut AgentPanel, app: &mut NodeCraftApp, ctx: &egui::Context, done: impl Fn(&AgentPanel) -> bool) {
    let start = Instant::now();
    while !done(panel) {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out; timeline: {:?}", panel.agent().timeline().collect::<Vec<_>>());
        panel.logic(app, ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn said(panel: &AgentPanel, text: &str) -> bool {
    panel.agent().timeline().any(|s| matches!(&s.kind, StepKind::Said(t) if t == text))
}

fn first_node(app: &NodeCraftApp) -> NodeId {
    *app.session.document().canvases().get(&app.canvas).unwrap().nodes().keys().next().unwrap()
}

#[test]
fn a_question_is_sent_with_the_selection_and_the_agent_reads_through_the_live_session() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let node = first_node(&app);
    app.selection.only(node);
    let (mut panel, script) = panel(AgentSettings::default());
    panel.input = "What is on my canvas?".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    assert_eq!(panel.agent().status(), Status::Thinking);
    assert!(panel.input.is_empty(), "the message left the input");
    script.release.send(tool("canvas_inspect", json!({}))).unwrap();
    script.release.send(say("One node.")).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| said(p, "One node."));
    let sent = script.sent.lock().unwrap();
    assert_eq!(sent.len(), 2);
    let (settings, first) = &sent[0];
    assert_eq!(settings.endpoint, AgentEndpoint::Anthropic);
    assert_eq!(first.model, nodecraft_providers::llm::ANTHROPIC_DEFAULT_MODEL, "Anthropic's default model without a setting");
    let asked = first.messages[0].text();
    assert!(asked.contains(&node.to_string()) && asked.contains("What is on my canvas?"), "{asked}");
    assert!(first.tools.iter().any(|t| t.name == "command_run"), "the MCP tools are offered");
    let inspected = panel.agent().timeline().find_map(|s| match &s.kind {
        StepKind::Tool(t) => Some(t.clone()),
        _ => None,
    });
    let inspected = inspected.unwrap();
    assert_eq!(inspected.label, "look at the canvas");
    assert_eq!(inspected.impact, Some(Impact::Read));
    assert!(matches!(inspected.status, ToolStatus::Ran(_)), "{inspected:?}");
}

#[test]
fn in_assist_mode_a_change_waits_for_approve_and_then_lands_as_one_undoable_step() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let node = first_node(&app);
    let revision = app.session.revision();
    let (mut panel, script) = panel(AgentSettings::default());
    panel.input = "Call it Sea".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    let rename = json!({"id": "node.title", "params": {"canvas": app.canvas, "id": node, "title": "Sea"}});
    script.release.send(tool("command_run", rename)).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| p.agent().status() == Status::Waiting);
    assert_eq!(app.session.revision(), revision, "nothing changes before approval");
    panel.apply(vec![Intent::Approve], &mut app, &ctx);
    let title = |app: &NodeCraftApp| app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&node).unwrap().title.clone();
    assert_eq!(title(&app), "Sea");
    script.release.send(say("Renamed.")).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| said(p, "Renamed."));
    assert!(app.session.can_undo());
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_ne!(title(&app), "Sea", "the agent's change undoes like any other");
}

#[test]
fn declining_tells_the_model_and_cancelling_ignores_the_late_reply() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let node = first_node(&app);
    let revision = app.session.revision();
    let (mut panel, script) = panel(AgentSettings::default());
    panel.input = "Rename it".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    script.release.send(tool("command_run", json!({"id": "node.title", "params": {"canvas": app.canvas, "id": node, "title": "X"}}))).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| p.agent().status() == Status::Waiting);
    panel.apply(vec![Intent::Decline], &mut app, &ctx);
    assert_eq!(panel.agent().status(), Status::Thinking, "the model hears it was declined");
    assert_eq!(app.session.revision(), revision);
    // Cancelled while the model answers: its reply, when it comes, changes nothing.
    let start = Instant::now();
    while script.sent.lock().unwrap().len() < 2 {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    panel.apply(vec![Intent::Cancel], &mut app, &ctx);
    assert_eq!(panel.agent().status(), Status::Idle);
    script.release.send(say("Too late.")).unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(100) {
        panel.logic(&mut app, &ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    panel.logic(&mut app, &ctx);
    assert!(!said(&panel, "Too late."));
    assert!(panel.agent().timeline().any(|s| s.kind == StepKind::Cancelled));
    assert_eq!(app.session.revision(), revision);
}

#[test]
fn an_endpoint_without_a_model_or_a_worker_says_so_instead_of_asking() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    // Headless apps have no settings file, so the defaults apply; give the panel a worker that
    // couldn't start instead.
    let mut panel = AgentPanel::new(Err("no thread".into()), AgentSettings::default());
    panel.input = "hello".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    assert!(panel.notice().is_some_and(|n| n.contains("no thread")));
    assert_eq!(panel.agent().status(), Status::Idle);
    assert_eq!(panel.input, "hello", "the message is kept to send again");
    let local = AgentSettings { endpoint: AgentEndpoint::Ollama, ..AgentSettings::default() };
    assert_eq!(worker::model(&local), None);
    assert_eq!(worker::model(&AgentSettings { model: "llama3".into(), ..local }), Some("llama3".into()));
}

#[test]
fn the_endpoint_needs_its_key_and_local_servers_none() {
    let keys = nodecraft_engine::secrets::MemoryStore::default();
    let missing = worker::endpoint(&AgentSettings::default(), &keys).unwrap_err();
    assert!(missing.message.contains("anthropic"), "{missing:?}");
    let account = nodecraft_engine::secrets::Account::new("anthropic").unwrap();
    nodecraft_engine::secrets::SecretStore::set(&keys, &account, &nodecraft_engine::secrets::Secret::new("sk-test").unwrap()).unwrap();
    let endpoint = worker::endpoint(&AgentSettings::default(), &keys).unwrap();
    assert_eq!(endpoint.id, "anthropic");
    let local = AgentSettings { endpoint: AgentEndpoint::LmStudio, base_url: Some("http://10.0.0.2:1234".into()), ..AgentSettings::default() };
    let endpoint = worker::endpoint(&local, &keys).unwrap();
    assert_eq!((endpoint.base_url.as_str(), endpoint.key.is_none()), ("http://10.0.0.2:1234/v1", true));
}

#[test]
fn mentions_join_the_focus_once_and_the_mode_switch_takes_effect() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let node = first_node(&app);
    app.selection.only(node);
    let mut panel = AgentPanel::new(Err("unused".into()), AgentSettings::default());
    panel.apply(vec![Intent::Mention(node), Intent::Mention(node)], &mut app, &ctx);
    assert_eq!(panel.mentions(), &[node]);
    let focus = focus(&app, panel.mentions());
    assert_eq!(focus.selection.len(), 1, "a selected node mentioned again is named once");
    assert_eq!(focus.canvas, Some(app.canvas));
    panel.apply(vec![Intent::Unmention(node), Intent::Mode(Mode::Auto)], &mut app, &ctx);
    assert!(panel.mentions().is_empty());
    assert_eq!(panel.agent().config().mode, Mode::Auto);
}

#[test]
fn costs_show_rounded_up_to_the_cent() {
    assert_eq!(usd(0), "$0.00");
    assert_eq!(usd(1), "$0.01");
    assert_eq!(usd(1_000_000), "$1.00");
    assert_eq!(usd(u64::MAX), format!("${}.{:02}", u64::MAX.div_ceil(10_000) / 100, u64::MAX.div_ceil(10_000) % 100));
    let run = Impact::Run(nodecraft_agent::RunImpact { ceiling: BTreeMap::from([(Currency::USD, 20_000)]), partial: false, unpriced_jobs: 1 });
    assert_eq!(impact_text(&run), "runs, up to $0.02, part of the price unknown");
}

fn shell(panel: AgentPanel) -> crate::shell::Shell {
    crate::shell::Shell { app: NodeCraftApp::headless().unwrap(), agent: panel, channel: None }
}

/// A frame of the real window: the sidebar's logic before the app's, then `Shell::show`.
fn frame(ui: &mut egui::Ui, shell: &mut crate::shell::Shell) {
    shell.agent.logic(&mut shell.app, ui.ctx());
    shell.show(ui);
}

fn sent_texts(script: &Script) -> Vec<String> {
    script.sent.lock().unwrap().iter().map(|(_, r)| r.messages.first().map(|m| m.text()).unwrap_or_default()).collect()
}

fn canvas_width(shell: &crate::shell::Shell) -> f32 {
    shell.app.viewport.width()
}

#[test]
fn the_sidebar_opens_with_the_shortcut_beside_a_narrower_canvas_and_sends_on_enter() {
    use egui_kittest::{Harness, kittest::Queryable};
    let (mut panel, script) = panel(AgentSettings::default());
    panel.open = false;
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(frame, shell(panel));
    h.run_steps(2);
    assert!(h.query_by_label("Agent").is_none());
    let full = canvas_width(h.state());
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Slash);
    h.run_steps(2);
    assert!(h.state().agent.open);
    h.get_by_label("Agent");
    let tokens = h.state().app.theme.tokens();
    assert!((full - canvas_width(h.state()) - tokens.agent_width).abs() < 1.0, "{full} vs {}", canvas_width(h.state()));
    // The input has focus on opening. Enter sends from mid-text too, without a newline.
    h.get_by_role(egui::accesskit::Role::MultilineTextInput).type_text("Hello");
    h.run_steps(1);
    h.key_press(egui::Key::ArrowLeft);
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().agent.agent().status(), Status::Thinking);
    let start = Instant::now();
    while script.sent.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    let sent = sent_texts(&script);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].contains("Hello") && !sent[0].contains("Hel\nlo") && !sent[0].ends_with('\n'), "{sent:?}");
    // Esc from somewhere else keeps its meaning there; from the sidebar's input it cancels.
    let input = AgentPanel::input_id();
    h.ctx.memory_mut(|m| m.surrender_focus(input));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().agent.agent().status(), Status::Thinking, "Esc on the canvas doesn't cancel");
    h.ctx.memory_mut(|m| m.request_focus(input));
    h.run_steps(1);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().agent.agent().status(), Status::Idle, "Esc cancels");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Slash);
    h.run_steps(2);
    assert!(!h.state().agent.open);
    assert!((canvas_width(h.state()) - full).abs() < 1.0);
}

#[test]
fn with_a_dialog_open_esc_and_the_shortcut_belong_to_the_dialog() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let (mut panel, _script) = panel(AgentSettings::default());
    panel.input = "Hi".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    ctx.memory_mut(|m| m.request_focus(AgentPanel::input_id()));
    let press = |key, modifiers| egui::RawInput {
        events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }],
        ..Default::default()
    };
    // As in the app: the sidebar's logic runs first, then the dialog draws and reads its keys.
    let frame = |input: egui::RawInput, panel: &mut AgentPanel, app: &mut NodeCraftApp| {
        let mut closes = false;
        let mut slash = false;
        let mut out = ctx.run_ui(input, |ui| {
            panel.logic(app, ui.ctx());
            slash = ui.input(|i| i.key_pressed(egui::Key::Slash));
            closes = egui::Modal::new(egui::Id::new("price")).show(ui.ctx(), |ui| ui.label("Run for $1?")).should_close();
        });
        out.textures_delta.clear();
        (closes, slash)
    };
    frame(egui::RawInput::default(), &mut panel, &mut app);
    assert!(frame(press(egui::Key::Escape, egui::Modifiers::NONE), &mut panel, &mut app).0, "the dialog closes on Esc");
    assert!(frame(press(egui::Key::Slash, egui::Modifiers::COMMAND), &mut panel, &mut app).1, "the dialog still sees Cmd+/");
    assert_eq!(panel.agent().status(), Status::Thinking);
    assert!(panel.open);
}

#[test]
fn a_cancelled_question_still_queued_is_never_sent() {
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    let (mut panel, script) = panel(AgentSettings::default());
    let ask = |panel: &mut AgentPanel, app: &mut NodeCraftApp, text: &str| {
        panel.input = text.into();
        panel.apply(vec![Intent::Ask], app, &ctx);
        assert_eq!(panel.agent().status(), Status::Thinking, "{:?}", panel.notice());
    };
    ask(&mut panel, &mut app, "one");
    let start = Instant::now();
    while script.sent.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    // "one" is with the model; "two" waits behind it and is cancelled before its turn.
    panel.apply(vec![Intent::Cancel], &mut app, &ctx);
    ask(&mut panel, &mut app, "two");
    panel.apply(vec![Intent::Cancel], &mut app, &ctx);
    ask(&mut panel, &mut app, "three");
    script.release.send(say("for one")).unwrap();
    script.release.send(say("for three")).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| said(p, "for three"));
    let sent = sent_texts(&script);
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].contains("one") && sent[1].contains("three"), "{sent:?}");
    assert!(!said(&panel, "for one"));
}

#[test]
fn opening_the_sidebar_keeps_the_canvas_auto_ids() {
    let ctx = egui::Context::default();
    let app = NodeCraftApp::headless().unwrap();
    let mut ids = Vec::new();
    for open in [false, true] {
        let mut panel = AgentPanel::new(Err("unused".into()), AgentSettings::default());
        panel.open = open;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = panel.show(ui, &app);
            ids.push(ui.next_auto_id());
        });
        out.textures_delta.clear();
    }
    assert_eq!(ids[0], ids[1]);
}

#[test]
fn a_card_under_the_sidebar_neither_shows_over_it_nor_takes_its_clicks() {
    use egui_kittest::{Harness, kittest::Queryable};
    let (panel, script) = panel(AgentSettings { mode: AgentMode::Assist, ..AgentSettings::default() });
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(frame, shell(panel));
    h.run_steps(2);
    let node = first_node(&h.state().app);
    let canvas = h.state().app.canvas;
    h.state_mut().agent.input = "Rename it".into();
    let ctx = h.ctx.clone();
    let shell = h.state_mut();
    shell.agent.apply(vec![Intent::Ask], &mut shell.app, &ctx);
    script.release.send(tool("command_run", json!({"id": "node.title", "params": {"canvas": canvas, "id": node, "title": "Sea"}}))).unwrap();
    let start = Instant::now();
    while h.state().agent.agent().status() != Status::Waiting {
        assert!(start.elapsed() < Duration::from_secs(10));
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Put the card where the Approve button is.
    let approve = h.get_by_label("Approve").rect();
    let world = h.state().app.view.to_world(h.state().app.viewport.min, egui::pos2(approve.min.x - 40.0, approve.min.y - 40.0));
    h.state_mut().app.session.dispatch("node.move", &json!({"canvas": canvas, "id": node, "pos": {"x": world.x, "y": world.y}})).unwrap();
    h.run_steps(2);
    h.get_by_label("Approve").click();
    h.run_steps(2);
    let title = h.state().app.session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().title.clone();
    assert_eq!(title, "Sea", "the click reached Approve, not the card");
    assert!(h.state().app.selection.is_empty(), "the card wasn't selected");
}
