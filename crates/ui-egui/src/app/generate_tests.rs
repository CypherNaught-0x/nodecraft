//! Generate from a card (#83): every run is quoted and confirmed before `run.start`, which
//! reaches a fake run host routed to the mock provider. Nothing here calls a real provider.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_engine::{
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_types::EdgeId;
use std::sync::Mutex;

const C: CanvasId = CanvasId::from_bytes([81; 16]);
const N: NodeId = NodeId::from_bytes([82; 16]);
type Runs = Arc<Mutex<Vec<PreparedRun>>>;

/// A public mock image model: a priced call with an inline prompt and an aspect choice.
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
[[model.params.params]]
name = "aspect"
label = "Aspect"
control = { type = "choice", options = [{ value = "1:1", label = "1:1" }, { value = "16:9", label = "16:9" }] }
default = { type = "text", value = "1:1" }

[[model]]
id = "mock-edit"
provider = "mock"
provider_model = "mock-image"
label = "Mock edit"
description = "Public deterministic test fixture with an edit mode"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.modes]]
id = "edit"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "image", modality = "image", min = 1, max = 4 }]
[[model.params.params]]
name = "n"
label = "Images"
control = { type = "choice", options = [{ value = "1", label = "1" }, { value = "2", label = "2" }, { value = "4", label = "4" }] }
default = { type = "text", value = "1" }
[[model.params.params]]
name = "strength"
label = "Strength"
control = { type = "slider", min = 0.0, max = 1.0, step = 0.05 }
default = { type = "number", value = 0.5 }
[model.parameter_contract]
version = { major = 1, minor = 0 }
namespace = "mock-edit"
owner = "model"
[[model.parameter_contract.rules]]
param = "n"
meaning = "generated_image_count"
[[model.parameter_contract.rules]]
param = "strength"
unit = "ratio"
meaning = "image_strength"
[[model.parameter_contract.schema.params]]
name = "n"
label = "Images"
control = { type = "choice", options = [{ value = "1", label = "1" }, { value = "2", label = "2" }, { value = "4", label = "4" }] }
default = { type = "text", value = "1" }
[[model.parameter_contract.schema.params]]
name = "strength"
label = "Strength"
control = { type = "slider", min = 0.0, max = 1.0, step = 0.05 }
default = { type = "number", value = 0.5 }
"#;

fn scene(model: &str) -> (Harness<'static, NodeCraftApp>, Runs) {
    scene_with(model, false)
}

/// `failing`: the host takes the run and then reports an error, as a host that lost its
/// connection after submitting would.
fn scene_with(model: &str, failing: bool) -> (Harness<'static, NodeCraftApp>, Runs) {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_IMAGE).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([83; 16]), "Generate from a card").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("A red square".into()))].into(),
        )
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 150.0, y: 120.0 };
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: nodecraft_exec::cost::Budgets::default(), spent: nodecraft_exec::cost::Spent::default() });
    let runs: Runs = Arc::default();
    let sink = runs.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |run| {
            sink.lock().unwrap().push(run);
            if failing { Err(Error::new(ErrorKind::Internal, "the run host lost its connection")) } else { Ok(json!({"accepted": true})) }
        })
        .unwrap();
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(nodecraft_provider_testkit::mock::MockProvider::standard())).unwrap();
    routing.select(N, "mock", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    let app = NodeCraftApp::from_session(session, C).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    (h, runs)
}

fn press(h: &mut Harness<'static, NodeCraftApp>, role: Role, label: &str) {
    let rect = h.get_by_role_and_label(role, label).rect();
    let pos = rect.center();
    // A real pointer is over a control for a frame before it presses it.
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

fn node(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

#[test]
fn the_submit_arrow_quotes_first_and_starts_only_once_accepted() {
    let (mut h, runs) = scene("mock-square");
    press(&mut h, Role::Button, "Generate");
    assert!(runs.lock().unwrap().is_empty(), "pressing Generate alone never spends");
    let accept = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    assert!(accept.contains("0.04"), "the price is shown before the run: {accept}");
    press(&mut h, Role::Button, "Cancel");
    assert!(h.state().run_confirm.is_none());
    assert!(runs.lock().unwrap().is_empty(), "a cancelled quote starts nothing");

    press(&mut h, Role::Button, "Generate");
    press(&mut h, Role::Button, &accept);
    assert_eq!(runs.lock().unwrap().len(), 1, "exactly one accepted run reaches the host");
    assert!(h.state().run_confirm.is_none());
    assert_eq!(h.state().notice, None);
}

#[test]
fn an_edit_after_the_quote_closes_nothing_silently_and_never_starts_the_old_run() {
    let (mut h, runs) = scene("mock-square");
    press(&mut h, Role::Button, "Generate");
    h.state_mut().session.dispatch("node.title", &json!({"canvas": C, "id": N, "title": "Renamed"})).unwrap();
    h.run();
    assert!(h.query_by_label_contains("changed after this price was quoted").is_some());
    assert!(h.query_by_label_contains("Generate for").is_none(), "a stale quote can't be accepted");
    press(&mut h, Role::Button, "Quote again");
    assert!(h.query_by_label_contains("Generate for").is_some());
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn enter_on_the_card_generates_and_f2_still_renames() {
    let (mut h, runs) = scene("mock-square");
    let card = "Rename A RED SQUARE";
    h.get_by_role_and_label(Role::Button, card).focus();
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().run_confirm.is_some(), "Enter on a focused generation card asks to generate");
    assert!(h.query_by_role_and_label(Role::Button, card).is_some(), "and does not start a rename");
    press(&mut h, Role::Button, "Cancel");
    h.get_by_role_and_label(Role::Button, card).focus();
    h.run();
    h.key_press(egui::Key::F2);
    h.run();
    h.event(egui::Event::Text("Square".into()));
    h.run();
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).title, "Square");
    assert!(h.state().run_confirm.is_none(), "the Enter that commits a rename does not generate");
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn a_failed_start_never_offers_the_same_price_again() {
    let (mut h, runs) = scene_with("mock-square", true);
    press(&mut h, Role::Button, "Generate");
    let accept = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(&mut h, Role::Button, &accept);
    assert_eq!(runs.lock().unwrap().len(), 1, "the host took the run before it failed");
    assert!(h.query_by_label_contains("lost its connection").is_some(), "the error shows in the dialog");
    assert!(h.query_by_label_contains("may have started").is_some());
    assert!(h.query_by_label_contains("Generate for").is_none(), "a second click can't start a second paid run");
    press(&mut h, Role::Button, "Close");
    assert!(h.state().run_confirm.is_none());
    assert_eq!(runs.lock().unwrap().len(), 1);
}

#[test]
fn cmd_enter_in_the_prompt_saves_it_and_quotes_the_edited_prompt() {
    let (mut h, runs) = scene("mock-square");
    h.get_by_role_and_label(Role::TextInput, "Prompt").focus();
    h.run();
    h.key_press(egui::Key::End);
    h.event(egui::Event::Text(" on blue".into()));
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    h.run();
    assert_eq!(node(h.state()).params.get("prompt"), Some(&Value::Text("A red square on blue".into())));
    let confirm = h.state().run_confirm.as_ref().expect("Cmd+Enter asks to generate");
    assert_eq!(confirm.revision, h.state().session.revision(), "the quote covers the saved prompt");
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn without_run_resources_generate_reports_why_instead_of_spending() {
    let mut project = Project::new(ProjectId::from_bytes([84; 16]), "No host").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut card = nodecraft_graph::Node::new(N, nodecraft_nodes::generation::KIND);
    card.pos = nodecraft_graph::Position { x: 150.0, y: 120.0 };
    let port = |name: &str, modality| nodecraft_graph::Port { name: name.into(), port_type: nodecraft_types::PortType { modality, many: true } };
    card.inputs = vec![port("prompt", Modality::Text), port("image", Modality::Image), port("mask", Modality::Image)];
    card.outputs = vec![port("out", Modality::Image)];
    card.params.insert("model".into(), Value::Text("openai-images.flare".into()));
    card.params.insert("prompt".into(), Value::Text("A red square".into()));
    card.params.insert("n".into(), Value::Unsigned(nodecraft_types::Unsigned(2)));
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let app = NodeCraftApp::from_session(Session::new(project).unwrap(), C).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    press(&mut h, Role::Button, "Generate");
    assert!(h.state().run_confirm.is_none());
    assert!(h.state().notice.is_some(), "the person is told why nothing ran");

    // The counter steps the model's own image count, found by its meaning tag.
    press(&mut h, Role::Button, "More images");
    assert_eq!(node(h.state()).params.get("n"), Some(&Value::Unsigned(nodecraft_types::Unsigned(3))));
    press(&mut h, Role::Button, "Fewer images");
    press(&mut h, Role::Button, "Fewer images");
    assert_eq!(node(h.state()).params.get("n"), Some(&Value::Unsigned(nodecraft_types::Unsigned(1))));
}

#[test]
fn the_toolbar_over_a_selected_card_opens_the_model_picker() {
    let (mut h, _) = scene("mock-square");
    assert!(h.query_by_label_contains("Model mock-square").is_none(), "no toolbar without a selection");
    h.state_mut().selection.only(N);
    h.run();
    press(&mut h, Role::Button, "Model mock-square");
    assert!(h.state().model_choice.is_some(), "the chip opens the same model picker as the card");
    assert!(h.query_by_label("Expand").is_none(), "nothing to expand before a result");
}

#[test]
fn the_toolbar_sets_the_aspect_ratio_found_by_its_options() {
    let (mut h, runs) = scene("mock-square");
    h.state_mut().selection.only(N);
    h.run();
    // The toolbar's menu is the one above the card's own.
    let combo = h
        .get_all_by_role(Role::ComboBox)
        .filter(|node| node.accesskit_node().label().as_deref() == Some("Aspect"))
        .min_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .unwrap()
        .rect();
    let pos = combo.center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    h.get_by_label("16:9").click();
    h.run();
    assert_eq!(node(h.state()).params.get("aspect"), Some(&Value::Text("16:9".into())));
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn the_toolbar_names_the_mode_the_wired_inputs_choose() {
    let (mut h, _) = scene("mock-edit");
    h.state_mut().selection.only(N);
    h.run();
    assert!(h.query_by_label("Text to image").is_some(), "only a prompt: text to image");
    let png = nodecraft_media::encode_png(16, 8, &[200; 16 * 8 * 4], &Default::default()).unwrap();
    const PICTURE: NodeId = NodeId::from_bytes([84; 16]);
    let at = nodecraft_graph::Position { x: -300.0, y: 120.0 };
    import_files(&mut h.state_mut().session, C, at, 200.0, &[Incoming { name: "pic.png", bytes: &png }], &[PICTURE]).unwrap();
    let port = h
        .state()
        .session
        .kinds()
        .get(&node(h.state()).kind)
        .unwrap()
        .instance_contract(node(h.state()))
        .unwrap()
        .inputs
        .iter()
        .find(|input| input.port != "prompt")
        .unwrap()
        .port
        .clone();
    let edge = json!({"id": EdgeId::from_bytes([85; 16]), "from": {"node": PICTURE, "port": "out"}, "to": {"node": N, "port": port}, "order": 1});
    h.state_mut().session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    h.run();
    assert!(h.query_by_label("Edit").is_some(), "a wired reference image chooses the edit mode");
    assert!(h.query_by_label("Text to image").is_none());
}

#[test]
fn a_counter_steps_through_a_choice_of_counts() {
    let (mut h, runs) = scene("mock-edit");
    press(&mut h, Role::Button, "More images");
    assert_eq!(node(h.state()).params.get("n"), Some(&Value::Text("2".into())));
    press(&mut h, Role::Button, "More images");
    assert_eq!(node(h.state()).params.get("n"), Some(&Value::Text("4".into())));
    press(&mut h, Role::Button, "Fewer images");
    assert_eq!(node(h.state()).params.get("n"), Some(&Value::Text("2".into())));
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn the_toolbar_strength_slider_sets_the_strength_once_a_drag_ends() {
    let (mut h, runs) = scene("mock-edit");
    h.state_mut().selection.only(N);
    h.run();
    let slider = h
        .get_all_by_role(Role::Slider)
        .filter(|node| node.accesskit_node().label().as_deref() == Some("Strength"))
        .min_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .unwrap()
        .rect();
    let revision = h.state().session.revision();
    let (from, to) = (slider.center(), egui::pos2(slider.right() - 1.0, slider.center().y));
    h.hover_at(from);
    h.run();
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    for step in 1..=4 {
        h.hover_at(from.lerp(to, step as f32 / 4.0));
        h.run();
    }
    assert_eq!(h.state().session.revision(), revision, "nothing is set while the drag goes on");
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    let Some(Value::Number(set)) = node(h.state()).params.get("strength") else { panic!("the strength was set") };
    assert!(set.get() > 0.9, "dragged to the right end: {}", set.get());
    assert!(h.state().session.revision() > revision);
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
#[ignore = "manual public-safe capture; GENERATE83_RENDER_DIR"]
fn generate83_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("GENERATE83_RENDER_DIR").unwrap());
    let (h, _) = scene("mock-square");
    let app = h.into_state();
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.state_mut().selection.only(N);
    h.run();
    h.render().unwrap().save(dir.join("generate83-card.png")).unwrap();
    press(&mut h, Role::Button, "Generate");
    h.render().unwrap().save(dir.join("generate83-confirm.png")).unwrap();
}
