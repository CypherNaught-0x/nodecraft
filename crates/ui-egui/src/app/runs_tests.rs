//! The app's runs (#409): Generate waits for the card's provider to be set up, quotes, and the
//! run host drives the accepted run against the mock provider, shown on the card. The adapter
//! maker is a test one; nothing here reads the keychain or calls a real provider.
use super::runs::{Make, Runs};
use super::*;
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
use nodecraft_provider_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const N: NodeId = NodeId::from_bytes([92; 16]);

const MOCK_IMAGE: &str = r#"
[[model]]
id = "mock-square"
provider = "mock"
provider_model = "mock-image"
label = "Mock image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A maker that builds `mock` for the `mock` adapter.
fn making(mock: MockProvider) -> Make {
    Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) })
}

fn image(model: MockModel) -> MockProvider {
    MockProvider::new("mock", [("mock-image".to_owned(), model)])
}

fn scene(make: Make) -> Harness<'static, NodeCraftApp> {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_IMAGE).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([93; 16]), "Runs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text("mock-square".into())), ("prompt".into(), Value::Text("A red square".into()))].into(),
        )
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 150.0, y: 120.0 };
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

/// Frames, as the window's loop runs them, until `done`.
fn frames_until(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&NodeCraftApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.run();
        if done(h.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "never got there; notice: {:?}", h.state().notice);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn accept(h: &mut Harness<'static, NodeCraftApp>) {
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
}

fn node(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

fn idle(app: &NodeCraftApp) -> bool {
    app.runs.as_ref().is_some_and(|runs| !runs.is_busy())
}

#[test]
fn generate_sets_up_the_provider_first_then_quotes_and_records_the_run() {
    let mock = image(MockModel::new(MockOutput::Image));
    // The keychain may prompt: the maker answers only once the gate opens, and frames go on.
    let (open, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let inner = making(mock.clone());
    let mut h = scene(Box::new(move |adapter, settings| {
        let _ = gate.lock().unwrap().recv();
        inner(adapter, settings)
    }));
    press(&mut h, Role::Button, "Generate");
    assert!(h.state().run_confirm.is_none(), "no quote before the provider is set up");
    assert_eq!(h.state().notice.as_deref(), Some("Setting up the provider…"));
    frames_until(&mut h, |_| true);
    assert!(h.state().run_confirm.is_none());
    open.send(()).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(h.state().notice, None);
    assert!(mock.requests().is_empty(), "the quote alone sends nothing");
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1, "one accepted run, one request");
    assert!(h.query_by_label("Run failed").is_none(), "notice: {:?}", h.state().notice);
}

#[test]
fn a_running_card_says_so_until_its_run_ends() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mut h = scene(making(image(slow)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.is_running(C, N)));
    h.get_by_label("Running…");
}

#[test]
fn a_failed_run_is_shown_on_its_card_with_the_reason() {
    let mut refusing = MockModel::new(MockOutput::Image);
    refusing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    let mut h = scene(making(image(refusing)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && app.runs.as_ref().is_some_and(|runs| runs.failure(C, N).is_some()));
    h.get_by_label("Run failed");
    assert!(h.state().notice.as_deref().is_some_and(|n| n.starts_with("A run failed")), "{:?}", h.state().notice);
    assert!(node(h.state()).history.is_empty());
}

#[test]
fn a_provider_without_its_key_says_what_to_do_and_asks_again_next_time() {
    let asked = Arc::new(Mutex::new(0u32));
    let count = asked.clone();
    let mut h = scene(Box::new(move |_, _| {
        *count.lock().unwrap() += 1;
        Err(Readiness::NeedsKey { account: "mock".into() })
    }));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.notice.as_deref() != Some("Setting up the provider…"));
    let notice = h.state().notice.clone().unwrap_or_default();
    assert!(notice.contains("needs an API key"), "{notice}");
    assert!(h.state().run_confirm.is_none(), "nothing is quoted for a provider that can't run");
    // The person adds the key; the next Generate reads it again rather than remembering the miss.
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.notice.as_deref() != Some("Setting up the provider…"));
    assert_eq!(*asked.lock().unwrap(), 2);
}

#[test]
#[ignore = "manual public-safe capture; RUNS409_RENDER_DIR"]
fn runs409_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("RUNS409_RENDER_DIR").unwrap());
    let render = |h: Harness<'static, NodeCraftApp>| {
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
        h.run();
        h
    };
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mut h = scene(making(image(slow)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.is_running(C, N)));
    render(h).render().unwrap().save(dir.join("runs409-running.png")).unwrap();
    let mut refusing = MockModel::new(MockOutput::Image);
    refusing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    let mut h = scene(making(image(refusing)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && app.runs.as_ref().is_some_and(|runs| runs.failure(C, N).is_some()));
    render(h).render().unwrap().save(dir.join("runs409-failed.png")).unwrap();
}
