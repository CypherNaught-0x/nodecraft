//! Generate from a card (#83): every run is quoted and confirmed before `run.start`, which
//! reaches a fake run host routed to the mock provider. Nothing here calls a real provider.
mod reasons655;

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

/// A public mock image model: a priced call with an inline prompt and an aspect choice; an image
/// model with an edit mode; and a text model for Improve prompt (#582).
const MOCK_IMAGE: &str = r#"
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
default = { type = "number", value = 0.7 }
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
default = { type = "number", value = 0.7 }
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
fn a_quote_with_reused_results_explains_free_reuse_and_cached_only_runs() {
    let (mut h, runs) = scene("mock-square");
    press(&mut h, Role::Button, "Generate");
    let mut quote = h.state_mut().session.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "nodes": [N]})).unwrap();
    // Supply a reuse-bearing quote to this UI test without changing the mock provider or
    // teaching the single-card scene an unrelated graph; engine tests cover actual reuse.
    quote["reused"] = json!([N, NodeId::from_bytes([86; 16])]);
    h.state_mut().run_confirm.as_mut().unwrap().set_quote(&quote);
    h.run();
    assert!(h.query_by_label_contains("2 reused, free").is_some(), "the quote distinguishes free reused results from priced steps");
    assert!(h.query_by_label_contains("Generate for").is_some(), "a priced mixed run still offers its quoted price");

    quote["steps"] = json!([]);
    quote["steps_total"] = json!(0);
    quote["totals"] = json!({});
    h.state_mut().run_confirm.as_mut().unwrap().set_quote(&quote);
    h.run();
    assert!(h.query_by_label_contains("Uses cached results where needed").is_some(), "no host work is implied when every node reuses");
    assert!(h.query_by_role_and_label(Role::Button, "Continue without generating").is_some());
    assert!(runs.lock().unwrap().is_empty(), "quoting never sends work to the host");
    press(&mut h, Role::Button, "Cancel");
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
fn an_already_active_cached_result_confirms_without_promising_an_edit_or_running() {
    let (mut h, runs) = scene("mock-square");
    h.state_mut().session.dispatch("node.cache_set", &json!({"canvas": C, "id": N, "enabled": true})).unwrap();
    let hash = {
        let app = h.state();
        let canvas = app.session.document().canvases().get(&C).unwrap();
        nodecraft_exec::staleness::input_hash(canvas, app.session.kinds(), N).unwrap()
    };
    let generation = nodecraft_graph::Generation {
        id: nodecraft_types::GenerationId::from_bytes([87; 16]),
        outputs: vec![Value::Text("An answer".into())],
        input_hash: Some(hash),
        items: Vec::new(),
        settings: Vec::new(),
        incomplete: None,
    };
    h.state_mut().session.dispatch("run.record", &json!({"canvas": C, "results": [{"node": N, "generation": generation}]})).unwrap();
    let revision = h.state().session.revision();
    let quote = h.state_mut().session.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "nodes": [N]})).unwrap();
    assert_eq!((quote["reused"].clone(), quote["steps"].clone()), (json!([N]), json!([])));
    press(&mut h, Role::Button, "Generate");
    assert!(h.query_by_label_contains("1 reused, free").is_some());
    assert!(h.query_by_label_contains("results already active stay in place").is_some(), "the dialog must not promise an edit for a no-op");
    assert!(h.query_by_label_contains("Applies cached results only").is_none(), "the old wording incorrectly promises a change");
    press(&mut h, Role::Button, "Continue without generating");
    assert_eq!(h.state().session.revision(), revision, "the already-active result is a no-op");
    assert!(h.state().run_confirm.is_none());
    assert!(runs.lock().unwrap().is_empty(), "no run reaches the host");
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
fn the_variations_counter_sets_the_count_and_the_quote_prices_every_variation() {
    let (mut h, runs) = scene("mock-square");
    assert!(h.get_by_label("Fewer variations").accesskit_node().is_disabled(), "one is the fewest");
    press(&mut h, Role::Button, "More variations");
    press(&mut h, Role::Button, "More variations");
    let stored = node(h.state()).config.get(nodecraft_exec::variations::VARIATIONS_CONFIG).cloned();
    assert_eq!(stored, Some(Value::Unsigned(nodecraft_types::Unsigned(3))));
    assert!(h.query_by_label("3×").is_some());
    press(&mut h, Role::Button, "Generate");
    let accept = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    assert!(accept.contains("0.12"), "three variations cost three requests: {accept}");
    press(&mut h, Role::Button, "Cancel");
    press(&mut h, Role::Button, "Fewer variations");
    press(&mut h, Role::Button, "Fewer variations");
    assert_eq!(node(h.state()).config.get(nodecraft_exec::variations::VARIATIONS_CONFIG), None, "one is stored as nothing");
    assert!(runs.lock().unwrap().is_empty());
}

#[test]
#[ignore = "manual public-safe capture; VARIATIONS81_RENDER_DIR"]
fn variations81_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("VARIATIONS81_RENDER_DIR").unwrap());
    let (mut h, _) = scene("mock-square");
    press(&mut h, Role::Button, "More variations");
    press(&mut h, Role::Button, "More variations");
    let app = h.into_state();
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.state_mut().selection.only(N);
    h.run();
    h.render().unwrap().save(dir.join("variations81-card.png")).unwrap();
    press(&mut h, Role::Button, "Generate");
    h.render().unwrap().save(dir.join("variations81-confirm.png")).unwrap();
}

fn toolbar_slider(h: &Harness<'static, NodeCraftApp>) -> egui::Rect {
    h.get_all_by_role(Role::Slider)
        .filter(|node| node.accesskit_node().label().as_deref() == Some("Strength"))
        .min_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .unwrap()
        .rect()
}

#[test]
fn showing_a_strength_the_slider_snaps_edits_nothing() {
    // 0.7 snaps to 0.7000000000000001 on a 0.05 step.
    let (mut h, _) = scene("mock-edit");
    let revision = h.state().session.revision();
    h.state_mut().selection.only(N);
    for _ in 0..4 {
        h.run();
    }
    toolbar_slider(&h);
    assert_eq!(h.state().session.revision(), revision, "selecting the card never edits it");
    assert_eq!(node(h.state()).params.get("strength").and_then(|v| if let Value::Number(n) = v { Some(n.get()) } else { None }), Some(0.7));
}

#[test]
fn escape_during_a_strength_drag_sets_nothing() {
    let (mut h, _) = scene("mock-edit");
    h.state_mut().selection.only(N);
    h.run();
    let slider = toolbar_slider(&h);
    let revision = h.state().session.revision();
    let (from, to) = (slider.center(), egui::pos2(slider.right() - 1.0, slider.center().y));
    h.hover_at(from);
    h.run();
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.hover_at(to);
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert_eq!(h.state().session.revision(), revision, "Escape cancels the drag");
}

#[test]
fn the_toolbar_strength_slider_sets_the_strength_once_a_drag_ends() {
    let (mut h, runs) = scene("mock-edit");
    h.state_mut().selection.only(N);
    h.run();
    let slider = toolbar_slider(&h);
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
    let mut quote = h.state_mut().session.dispatch("run.quote", &json!({"canvas": C, "mode": "selected", "nodes": [N]})).unwrap();
    quote["reused"] = json!([N, NodeId::from_bytes([86; 16])]);
    quote["steps"] = json!([]);
    quote["steps_total"] = json!(0);
    quote["totals"] = json!({});
    h.state_mut().run_confirm.as_mut().unwrap().set_quote(&quote);
    h.run();
    h.render().unwrap().save(dir.join("generate811-reused.png")).unwrap();
}

#[test]
fn improve_prompt_adds_a_wired_text_card_and_selects_it() {
    let (mut h, runs) = scene("mock-square");
    h.state_mut().selection.only(N);
    h.run();
    let shots = std::env::var_os("NODECRAFT_ASSIST_TEST_SHOTS").map(std::path::PathBuf::from);
    if let Some(dir) = &shots {
        h.render().unwrap().save(dir.join("assist582-before.png")).unwrap();
    }
    press(&mut h, Role::Button, "Improve prompt");
    assert_eq!(h.state().notice, None);
    if let Some(dir) = &shots {
        h.render().unwrap().save(dir.join("assist582-after.png")).unwrap();
    }
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    let assist = canvas.nodes().values().find(|node| node.kind == nodecraft_nodes::generation::TEXT_KIND).unwrap();
    assert_eq!(assist.params.get("model"), Some(&Value::Text("mock-writer".into())));
    assert!(canvas.edges().values().any(|edge| edge.from.node == assist.id && edge.to == Endpoint { node: N, port: "prompt".into() }));
    assert_eq!(h.state().selection.nodes().iter().copied().collect::<Vec<_>>(), [assist.id], "the new card is selected, ready to run");
    assert!(runs.lock().unwrap().is_empty(), "adding the card spends nothing");

    // Now the prompt is wired: the image card's toolbar says why it can't improve it again.
    h.state_mut().selection.only(N);
    h.run();
    assert!(h.get_by_role_and_label(Role::Button, "Improve prompt").accesskit_node().is_disabled());
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    assert_eq!(canvas.nodes().len(), 1, "one undo removes the card and its wire");
    assert!(canvas.edges().is_empty());
}

#[test]
fn improve_prompt_waits_for_a_typed_prompt() {
    let (mut h, _) = scene("mock-square");
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas":C,"id":N,"name":"prompt","value":{"type":"text","value":"  "}})).unwrap();
    h.state_mut().selection.only(N);
    h.run();
    assert!(h.get_by_role_and_label(Role::Button, "Improve prompt").accesskit_node().is_disabled());
    press(&mut h, Role::Button, "Improve prompt");
    assert_eq!(h.state().session.document().canvases().get(&C).unwrap().nodes().len(), 1);
}
