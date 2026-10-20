//! A text card's answer streams in as it is written (#504): the card shows it as it arrives, a
//! cancelled answer can be kept or discarded, and keeping it never replaces the answer shown.
//! The mock provider streams; nothing here reads the keychain or calls a real provider.
use super::runs::{Make, Runs};
use super::*;
use crate::card::generate::{DISCARD, KEEP, NO_ANSWER};
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::{
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_setup::Readiness,
    run_cmds::RunCatalog,
};
use nodecraft_graph::Generation;
use nodecraft_provider_testkit::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_types::GenerationId;
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([50; 16]);
const N: NodeId = NodeId::from_bytes([51; 16]);
const SHOWN: &str = "An answer from before.";
const STREAMED: &str = "Rain on the roof, ";

const MOCK_TEXT: &str = r#"
[[model]]
id = "mock-writer"
provider = "mock"
provider_model = "mock-text"
label = "Mock writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A text model that streams [`STREAMED`] in two pieces, then waits at the provider.
fn streaming() -> MockProvider {
    let mut model = MockModel::new(MockOutput::Text);
    model.polls = u32::MAX;
    MockProvider::new("mock", [("mock-text".to_owned(), model)]).with_stream("mock-text", ["Rain on ", "the roof, "])
}

/// A text card, showing [`SHOWN`] when `answered`.
fn scene(answered: bool) -> Harness<'static, NodeCraftApp> {
    let mock = streaming();
    let make: Make =
        Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) });
    let snapshot = GenerationCatalog::new(RunCatalog::new(nodecraft_catalog::Catalog::from_toml(MOCK_TEXT).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([52; 16]), "Streaming").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::TEXT_KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text("mock-writer".into())), ("prompt".into(), Value::Text("A haiku about rain".into()))].into(),
        )
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 400.0, y: 120.0 };
    if answered {
        let id = GenerationId::from_bytes([53; 16]);
        node.history.push(Generation::new(id, vec![Value::Text(SHOWN.into())]));
        node.active = Some(id);
    }
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut services = Services::default();
    let runs = Runs::with_maker(&mut services, make).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    runs.install(&mut session);
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

fn press(h: &mut Harness<'static, NodeCraftApp>, role: Role, label: &str) {
    let pos = h.get_by_role_and_label(role, label).rect().center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

fn frames_until(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&Harness<'static, NodeCraftApp>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.run();
        if done(h) {
            return;
        }
        assert!(Instant::now() < deadline, "never got there; notice: {:?}", h.state().notice);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn node(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

/// Generates, then cancels once the streamed answer shows on the card.
fn stream_then_cancel(h: &mut Harness<'static, NodeCraftApp>) {
    press(h, Role::Button, "Generate");
    frames_until(h, |h| h.state().run_confirm.is_some());
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
    frames_until(h, |h| h.query_by_label(STREAMED).is_some());
    assert!(h.query_by_label(SHOWN).is_none(), "the streaming answer shows in its place meanwhile");
    assert!(h.query_by_label(KEEP).is_none(), "an answer still arriving can't be kept yet");
    let key = h.state().runs.as_ref().unwrap().jobs().iter().find(|view| view.node == N).unwrap().key;
    h.state_mut().runs.as_mut().unwrap().cancel(key).unwrap();
    frames_until(h, |h| h.query_by_label(KEEP).is_some());
}

#[test]
fn an_answer_streams_onto_the_card_and_a_cancelled_one_can_be_kept_without_replacing_the_shown_one() {
    let mut h = scene(true);
    h.get_by_label(SHOWN);
    let t = *h.state().theme.tokens();
    let before = card::world_size(node(h.state()), None, &t);
    stream_then_cancel(&mut h);
    assert_eq!(card::world_size(node(h.state()), None, &t), before, "the card doesn't jump while the answer streams");
    h.get_by_label("Stopped before it finished");
    assert_eq!(node(h.state()).history.len(), 1, "nothing is recorded until it is kept");
    press(&mut h, Role::Button, KEEP);
    let card = node(h.state());
    assert_eq!(card.history.len(), 2);
    assert!(matches!(&card.history[1].outputs[..], [Value::Text(text)] if text == STREAMED), "{:?}", card.history);
    assert_eq!(card.active, Some(GenerationId::from_bytes([53; 16])), "keeping never replaces the shown answer");
    h.run();
    h.get_by_label(SHOWN);
    assert!(h.query_by_label(KEEP).is_none(), "it is kept once");
}

#[test]
fn a_discarded_answer_leaves_the_card_as_it_was() {
    let mut h = scene(false);
    h.get_by_label(NO_ANSWER);
    stream_then_cancel(&mut h);
    press(&mut h, Role::Button, DISCARD);
    h.run();
    assert!(node(h.state()).history.is_empty());
    h.get_by_label(NO_ANSWER);
    assert!(h.query_by_label(STREAMED).is_none());
}

#[test]
#[ignore = "manual public-safe capture; STREAM504_RENDER_DIR"]
fn stream504_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("STREAM504_RENDER_DIR").unwrap());
    let app = scene(true).into_state();
    let mut h =
        Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h.render().unwrap().save(dir.join("before.png")).unwrap();
    stream_then_cancel(&mut h);
    h.render().unwrap().save(dir.join("cancelled.png")).unwrap();
}
