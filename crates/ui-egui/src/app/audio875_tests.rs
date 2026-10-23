//! A speech card plays its sound while it arrives (#875): the job's latest preview shows on the
//! card as view state, the card keeps its size, nothing is recorded until the sound is done, and
//! then the card shows the result of the same job. The mock provider sends the previews; nothing
//! here reads the keychain or calls a real provider. Hearing it needs the real-tool test in
//! `audio/tests.rs`.
use super::runs::{Make, Runs};
use super::*;
use crate::card::NO_SOUND;
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
use nodecraft_provider_testkit::mock::{MockModel, MockOutput, MockProvider};
use nodecraft_providers::Output;
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([87; 16]);
const N: NodeId = NodeId::from_bytes([88; 16]);

const SPEECH: &str = r#"
[[model]]
id = "mock-speech"
provider = "mock"
provider_model = "mock-audio"
label = "Mock speech"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-audio"
output = "audio"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// An MP3 of `len` bytes: an ID3v2.4 header, an MPEG-1 Layer III frame header, then filler.
fn mp3(len: usize) -> Output {
    let mut bytes = b"ID3\x04\0\0\0\0\0\0\xFF\xFB\x90\x64".to_vec();
    bytes.resize(len, 7);
    Output::Bytes { mime: "audio/mpeg".into(), bytes: bytes.into() }
}

/// A speech card with no sound yet, on a model that sends two previews and then takes `polls`.
fn scene(polls: u32) -> Harness<'static, NodeCraftApp> {
    let mut model = MockModel::new(MockOutput::Audio);
    model.polls = polls;
    let mock = MockProvider::new("mock", [("mock-audio".to_owned(), model)]).with_previews("mock-audio", [mp3(1000), mp3(2000)]);
    let make: Make =
        Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) });
    let snapshot = GenerationCatalog::new(RunCatalog::new(nodecraft_catalog::Catalog::from_toml(SPEECH).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([89; 16]), "Speech").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("mock-speech".into())), ("prompt".into(), Value::Text("Hello there".into()))].into();
    let mut node = kinds.get(nodecraft_nodes::generation::AUDIO_KIND).unwrap().instantiate_instance(N, Default::default(), &values).unwrap();
    node.pos = nodecraft_graph::Position { x: 400.0, y: 120.0 };
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

fn preview(app: &NodeCraftApp) -> Option<nodecraft_engine::run_jobs::AudioPreview> {
    app.runs.as_ref().and_then(|runs| runs.audio_preview(C, N)).cloned()
}

fn generate(h: &mut Harness<'static, NodeCraftApp>) {
    press(h, Role::Button, "Generate");
    frames_until(h, |h| h.state().run_confirm.is_some());
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
}

#[test]
fn a_preview_plays_on_the_card_without_touching_the_document_or_its_size() {
    let mut h = scene(u32::MAX);
    h.get_by_label(NO_SOUND);
    let t = *h.state().theme.tokens();
    let before = card::world_size(node(h.state()), None, &t);
    generate(&mut h);
    let (revision, undo) = (h.state().session.revision(), h.state().session.can_undo());
    frames_until(&mut h, |h| preview(h.state()).is_some_and(|p| p.bytes.len() == 2000));
    // No tools in tests: the card's sound body says so, in place of "No sound yet".
    assert!(h.query_by_label(crate::audio::UNCONFIGURED).is_some(), "the preview shows as the card's sound");
    assert!(h.query_by_label(NO_SOUND).is_none());
    assert_eq!(card::world_size(node(h.state()), None, &t), before, "the card doesn't jump when its sound starts");
    assert!(node(h.state()).history.is_empty(), "a sound still arriving isn't recorded");
    assert_eq!((h.state().session.revision(), h.state().session.can_undo()), (revision, undo), "nor is anything else");

    // With tools the card would be playing it: stand in for that player.
    let shown = preview(h.state()).unwrap();
    h.state_mut().audios.test_preview_player(N, shown.hash, shown.take);
    let key = h.state().runs.as_ref().unwrap().jobs().iter().find(|view| view.node == N).unwrap().key;
    h.state_mut().runs.as_mut().unwrap().cancel(key).unwrap();
    frames_until(&mut h, |h| preview(h.state()).is_none() && h.query_by_label(NO_SOUND).is_some());
    assert!(node(h.state()).history.is_empty(), "a cancelled sound leaves the card as it was");
    assert_eq!(h.state().audios.test_counts(), (0, 0, 0, false), "and its preview stops playing: no player, no bytes");
}

#[test]
fn the_card_goes_from_the_preview_to_the_same_jobs_result() {
    let mut h = scene(400);
    generate(&mut h);
    frames_until(&mut h, |h| preview(h.state()).is_some());
    let take = preview(h.state()).unwrap().take;
    frames_until(&mut h, |h| node(h.state()).history.len() == 1);
    h.run();
    let runs = h.state().runs.as_ref().unwrap();
    assert_eq!(runs.audio_preview(C, N), None, "the result plays now");
    let result = crate::audio::audio_of(node(h.state())).unwrap().hash;
    assert_eq!(runs.finished_take(C, N, result), Some(take), "and it is the previewed take's, so the card plays on into it");
    // A sound the job didn't make (the card's earlier one, while this result is held) doesn't.
    assert_eq!(runs.finished_take(C, N, nodecraft_types::Blake3::from_bytes([5; 32])), None, "an earlier sound starts again");
    assert!(h.query_by_label(crate::audio::UNCONFIGURED).is_some(), "the card shows the result's sound");
}
