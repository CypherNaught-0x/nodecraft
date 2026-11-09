use super::*;
use nodecraft_engine::settings::AgentEndpoint;
use nodecraft_engine::settings::ProviderSettings;
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
    let missing = worker::endpoint(&AgentSettings::default(), &[ProviderSettings::default()], &keys).unwrap_err();
    assert!(missing.message.contains("anthropic"), "{missing:?}");
    let account = nodecraft_engine::secrets::Account::new("anthropic").unwrap();
    nodecraft_engine::secrets::SecretStore::set(&keys, &account, &nodecraft_engine::secrets::Secret::new("sk-test").unwrap()).unwrap();
    let endpoint = worker::endpoint(&AgentSettings::default(), &[ProviderSettings::default()], &keys).unwrap();
    assert_eq!(endpoint.id, "anthropic");
    let local = AgentSettings { endpoint: AgentEndpoint::LmStudio, base_url: Some("http://10.0.0.2:1234".into()), ..AgentSettings::default() };
    let endpoint = worker::endpoint(&local, &[ProviderSettings::default()], &keys).unwrap();
    assert_eq!((endpoint.base_url.as_str(), endpoint.key.is_none()), ("http://10.0.0.2:1234/v1", true));
}

#[test]
fn the_agent_follows_a_custom_base_url_or_refuses() {
    let keys = nodecraft_engine::secrets::MemoryStore::default();
    for name in ["anthropic", "openai"] {
        let account = nodecraft_engine::secrets::Account::new(name).unwrap();
        nodecraft_engine::secrets::SecretStore::set(&keys, &account, &nodecraft_engine::secrets::Secret::new("sk-test").unwrap()).unwrap();
    }
    let openai = AgentSettings { endpoint: AgentEndpoint::Openai, ..AgentSettings::default() };
    assert_eq!(worker::endpoint(&openai, &[ProviderSettings::default()], &keys).unwrap().base_url, "https://api.openai.com/v1", "nothing set: unchanged");
    let mut providers = ProviderSettings::default();
    providers.base_urls.insert("openai".into(), "http://127.0.0.1:4000/v1".into());
    providers.base_urls.insert("anthropic".into(), "ftp://gateway.example/v1".into());
    assert_eq!(worker::endpoint(&openai, std::slice::from_ref(&providers), &keys).unwrap().base_url, "http://127.0.0.1:4000/v1");
    let refused = worker::endpoint(&AgentSettings::default(), std::slice::from_ref(&providers), &keys).unwrap_err();
    assert!(refused.message.contains("custom base URL") && !refused.message.contains("gateway.example"), "{refused:?}");
    // A key set up for OpenAI Images' custom base URL doesn't go to OpenAI's own API either.
    let mut images = ProviderSettings::default();
    images.base_urls.insert("openai-images".into(), "https://gateway.example/v1".into());
    assert!(worker::endpoint(&openai, std::slice::from_ref(&images), &keys).is_err());
    // A custom base gets the custom-endpoint client's base; the views must agree.
    let mut local = ProviderSettings::default();
    local.base_urls.insert("openai".into(), "http://127.0.0.1:4000/v1".into());
    let (_, base) = worker::routed(&openai, &[local.clone(), local.clone()], &keys).unwrap();
    assert_eq!(base.unwrap().as_str(), "http://127.0.0.1:4000/v1");
    // The file now unreadable (defaults) never undoes what was loaded at startup, nor the reverse.
    let defaults = ProviderSettings::default();
    for views in [[local.clone(), defaults.clone()], [defaults.clone(), local.clone()]] {
        let refused = worker::endpoint(&openai, &views, &keys).unwrap_err();
        assert!(refused.message.contains("restart"), "{refused:?}");
    }
    let mut images_now = ProviderSettings::default();
    images_now.base_urls.insert("openai-images".into(), "https://gateway.example/v1".into());
    assert!(worker::endpoint(&openai, &[defaults, images_now], &keys).is_err(), "either view's shared-key rule refuses");
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
    crate::shell::Shell { app: NodeCraftApp::headless().unwrap(), agent: panel, channel: None, storage_window: None, storage_window_pending: None, mcp: None }
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
fn agent959_mentions_and_endpoint_have_accessible_names() {
    use egui::accesskit::Role;
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };
    let (panel, _script) = panel(AgentSettings::default());
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(frame, shell(panel));
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, "Mention a node");
    h.get_by_role_and_label(Role::Button, "Settings").click();
    h.run_steps(3);
    assert!(h.state().agent.draft.is_some());
    let endpoint = h.get_by_role(Role::ComboBox);
    assert!(endpoint.accesskit_node().label().unwrap_or_default().contains("Endpoint"), "{endpoint:?}");
}

#[test]
fn agent959_busy_clear_and_settings_keep_their_explanation() {
    use egui::accesskit::Role;
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };
    let (panel, _script) = panel(AgentSettings::default());
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(frame, shell(panel));
    h.ctx.style_mut(|style| {
        style.interaction.tooltip_delay = 0.0;
        style.interaction.tooltip_grace_time = 0.0;
        style.interaction.show_tooltips_only_when_still = false;
    });
    h.state_mut().agent.input = "Wait for the scripted reply".into();
    let ctx = h.ctx.clone();
    let shell = h.state_mut();
    shell.agent.apply(vec![Intent::Ask], &mut shell.app, &ctx);
    h.run_steps(3);
    for label in ["Clear", "Settings"] {
        let control = h.get_by_role_and_label(Role::Button, label);
        assert!(control.accesskit_node().is_disabled());
        let at = control.rect().center();
        h.hover_at(at);
        h.run_steps(4);
        h.get_by_label("Available once the agent has finished; cancel its current request first");
    }
    assert_eq!(h.state().agent.agent().status(), Status::Thinking);
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

/// While the person's save is still writing (#367), the agent's next step waits in the worker
/// instead of being run and refused as busy, which the model would retry as paid turns; once
/// the save has settled it runs, and the conversation goes on with one request per step.
#[test]
fn a_reply_waits_for_a_background_save_instead_of_spending_turns_on_busy() {
    #[derive(Default)]
    struct Gate(Mutex<Vec<nodecraft_engine::save_runner::SaveJob>>);
    impl nodecraft_engine::save_runner::SaveRunner for Gate {
        fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
            self.0.lock().unwrap().push(job);
            Ok(())
        }
    }
    let base = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
    let storage = nodecraft_ui_egui::app::storage::StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: None };
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(storage)).unwrap();
    let node = first_node(&app);
    let (mut panel, script) = panel(AgentSettings::default());
    panel.apply(vec![Intent::Mode(Mode::Auto)], &mut app, &ctx);
    let gate = Arc::new(Gate::default());
    app.session.set_save_runner(gate.clone());
    let file = base.path().join("agent.ncraft");
    assert_eq!(app.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap(), "background": true})).unwrap()["saving"], true);
    panel.input = "Call it Sea".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    let rename = json!({"id": "node.title", "params": {"canvas": app.canvas, "id": node, "title": "Sea"}});
    script.release.send(tool("command_run", rename)).unwrap();
    let title = |app: &NodeCraftApp| app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&node).unwrap().title.clone();
    // The reply has come back to the worker; frames go on and nothing runs or is asked again.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(300) {
        panel.logic(&mut app, &ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_ne!(title(&app), "Sea");
    assert_eq!(panel.agent().status(), Status::Thinking, "the step is still to come");
    assert_eq!(script.sent.lock().unwrap().len(), 1, "no turn was spent on a busy refusal");
    // The save finishes; the step runs on the saved project and the model is asked once more.
    std::mem::take(&mut *gate.0.lock().unwrap()).into_iter().for_each(nodecraft_engine::save_runner::SaveJob::run);
    assert_eq!(app.session.poll_file().unwrap().unwrap()["saved"], true);
    script.release.send(say("Renamed.")).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| said(p, "Renamed."));
    assert_eq!(title(&app), "Sea");
    assert_eq!(script.sent.lock().unwrap().len(), 2);
}

/// During a background open (or recovery) every change is refused too (#367): the agent's
/// replies wait in the worker, and Approve waits until the open has finished rather than
/// running the approved steps into busy refusals.
#[test]
fn approval_and_replies_wait_for_a_background_open() {
    #[derive(Default)]
    struct Gate(Mutex<Vec<nodecraft_engine::save_runner::SaveJob>>);
    impl nodecraft_engine::save_runner::SaveRunner for Gate {
        fn spawn(&self, job: nodecraft_engine::save_runner::SaveJob) -> Result<(), nodecraft_engine::save_runner::SaveJob> {
            self.0.lock().unwrap().push(job);
            Ok(())
        }
    }
    let base = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
    let storage = nodecraft_ui_egui::app::storage::StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: None };
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(storage)).unwrap();
    let node = first_node(&app);
    let file = base.path().join("agent-open.ncraft");
    app.session.dispatch("file.save_as", &json!({"location": file.to_str().unwrap()})).unwrap();
    let (mut panel, script) = panel(AgentSettings::default());
    panel.input = "Call it Sea".into();
    panel.apply(vec![Intent::Ask], &mut app, &ctx);
    let rename = json!({"id": "node.title", "params": {"canvas": app.canvas, "id": node, "title": "Sea"}});
    script.release.send(tool("command_run", rename)).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| p.agent().status() == Status::Waiting);
    // The same file opens again in the background, its job held.
    let gate = Arc::new(Gate::default());
    app.session.set_save_runner(gate.clone());
    assert_eq!(
        app.session.dispatch("file.open", &json!({"location": file.to_str().unwrap(), "discard_dirty": true, "background": true})).unwrap()["opening"],
        true
    );
    let title = |app: &NodeCraftApp| app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&node).unwrap().title.clone();
    panel.apply(vec![Intent::Approve], &mut app, &ctx);
    for _ in 0..20 {
        panel.logic(&mut app, &ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(panel.agent().status(), Status::Waiting, "the approval is still to be given");
    assert_ne!(title(&app), "Sea");
    assert_eq!(script.sent.lock().unwrap().len(), 1, "no turn was spent on a busy refusal");
    std::mem::take(&mut *gate.0.lock().unwrap()).into_iter().for_each(nodecraft_engine::save_runner::SaveJob::run);
    assert_eq!(app.session.poll_file().unwrap().unwrap()["opened"], true);
    panel.apply(vec![Intent::Approve], &mut app, &ctx);
    assert_eq!(title(&app), "Sea", "approved on the opened project");
    script.release.send(say("Renamed.")).unwrap();
    pump_until(&mut panel, &mut app, &ctx, |p| said(p, "Renamed."));
    assert_eq!(script.sent.lock().unwrap().len(), 2);
}

/// The agent's settings screen (#1063): choosing an endpoint and a model is saved as one settings
/// write when Save is pressed, used for the next message, and kept in the settings file. A bad
/// address or a missing model is refused with a reason and nothing is saved; Cancel drops the form.
#[test]
fn the_settings_screen_chooses_the_endpoint_and_model_and_refuses_what_wont_work() {
    use egui_kittest::{Harness, kittest::Queryable};
    let dir = tempfile::tempdir().unwrap();
    let location = nodecraft_engine::settings::Location::Dir(dir.path().to_path_buf());
    let (store, _) = nodecraft_engine::settings::open(location.clone());
    let (panel, script) = panel(AgentSettings::default());
    let ctx = egui::Context::default();
    let app = NodeCraftApp::with_settings(&ctx, store).unwrap();
    let shell = crate::shell::Shell { app, agent: panel, channel: None, storage_window: None, storage_window_pending: None, mcp: None };
    let mut h = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).build_ui_state(frame, shell);
    h.run_steps(2);
    let click = |h: &mut Harness<'_, crate::shell::Shell>, label: &str| {
        h.get_by_label(label).click();
        h.run_steps(2);
    };
    click(&mut h, "Settings");
    assert!(h.state().agent.draft.is_some(), "the form opens");
    // Ollama with no model: refused, nothing saved.
    h.state_mut().agent.draft.as_mut().unwrap().endpoint = AgentEndpoint::Ollama;
    h.run_steps(2);
    assert!(h.query_by_label("Server address").is_some(), "a local server's address can be set");
    click(&mut h, "Save");
    assert!(h.query_by_label_contains("Choose a model for Ollama").is_some());
    assert_eq!(h.state().app.agent_settings().endpoint, AgentEndpoint::Anthropic, "nothing saved");
    // A model, but an address that isn't http(s): refused too.
    assert!(h.query_by_label("Model").is_some());
    let draft = h.state_mut().agent.draft.as_mut().unwrap();
    draft.model = "llama3.2".into();
    draft.base_url = "file:///etc/passwd".into();
    h.run_steps(2);
    click(&mut h, "Save");
    assert!(h.query_by_label_contains("must start with http:// or https://").is_some());
    assert_eq!(h.state().app.agent_settings().endpoint, AgentEndpoint::Anthropic, "still nothing saved");
    // A good address: saved at once, the form closes, and the agent asks that model.
    h.state_mut().agent.draft.as_mut().unwrap().base_url = "http://127.0.0.1:11434".into();
    h.run_steps(1);
    click(&mut h, "Save");
    assert!(h.state().agent.draft.is_none(), "saved, the form closes");
    let saved = h.state().app.agent_settings();
    assert_eq!((saved.endpoint, saved.model(), saved.base_url()), (AgentEndpoint::Ollama, Some("llama3.2"), Some("http://127.0.0.1:11434")));
    assert_eq!(h.state().agent.agent().config().model, "llama3.2");
    // Cancel drops a changed form.
    click(&mut h, "Settings");
    h.state_mut().agent.draft.as_mut().unwrap().endpoint = AgentEndpoint::Openai;
    // The form loses its address field: lay it out again before pressing.
    h.run_steps(2);
    click(&mut h, "Cancel");
    assert!(h.state().agent.draft.is_none());
    assert_eq!(h.state().app.agent_settings().endpoint, AgentEndpoint::Ollama, "Cancel saves nothing");
    // The next message goes to the saved endpoint and model.
    h.state_mut().agent.input = "Hello".into();
    let mut shell = h.into_state();
    let ctx = egui::Context::default();
    shell.agent.apply(vec![Intent::Ask], &mut shell.app, &ctx);
    let start = Instant::now();
    while script.sent.lock().unwrap().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    let (settings, request) = script.sent.lock().unwrap().first().cloned().unwrap();
    assert_eq!((settings.endpoint, request.model.as_str()), (AgentEndpoint::Ollama, "llama3.2"));
    // And it is in the settings file already: Save wrote it.
    let (reopened, _) = nodecraft_engine::settings::open(location);
    assert_eq!(reopened.get().agent.endpoint, AgentEndpoint::Ollama);
    assert_eq!(reopened.get().agent.model(), Some("llama3.2"));
}

#[test]
fn the_sidebar_leaves_a_small_window_half_its_canvas_and_keeps_its_bounds() {
    use egui_kittest::Harness;
    let tokens = nodecraft_ui_egui::theme::ThemeKind::Dark.tokens();
    for (size, opens) in [(egui::vec2(640.0, 400.0), None), (egui::vec2(1200.0, 800.0), Some(tokens.agent_width))] {
        let (mut panel, _script) = panel(AgentSettings::default());
        panel.open = false;
        let mut h = Harness::builder().with_size(size).build_ui_state(frame, shell(panel));
        h.run_steps(2);
        let full = canvas_width(h.state());
        h.state_mut().agent.open = true;
        h.run_steps(2);
        let sidebar = full - canvas_width(h.state());
        // A small window gives it half of the window's space; a large one its usual width.
        let expected = opens.unwrap_or(full * tokens.agent_max_fraction);
        // At most half the window (#1115), and never narrower than its controls need.
        assert!(sidebar <= size.x * tokens.agent_max_fraction + 1.0, "{size:?}: {sidebar}");
        assert!(sidebar >= tokens.agent_min_width - 1.0, "{size:?}: {sidebar}");
        assert!((sidebar - expected).abs() < 1.0, "{size:?}: {sidebar} vs {expected}");
    }
    // The bounds themselves, for windows from tiny to hostile.
    for width in [0.0, 300.0, 640.0, 1440.0, f32::NAN, f32::INFINITY] {
        let (default, range) = sidebar_width(width, tokens);
        assert!(range.min <= range.max && range.min == tokens.agent_min_width, "{width}");
        assert!(range.contains(default), "{width}: {default} outside {range:?}");
    }
}
