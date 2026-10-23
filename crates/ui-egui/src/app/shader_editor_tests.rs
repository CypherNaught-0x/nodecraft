//! The shader editor (#90), opened from a custom shader card's menu and driven through real
//! pointer and key events. The model is the mock text model behind a fake run host: nothing here
//! calls a real provider.
use super::generate::RunKind;
use super::mask_editor_tests::right_click;
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::{
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_nodes::shader::{StoredShader, stored};
use nodecraft_types::GenerationId;
use std::sync::Mutex;

const C: CanvasId = CanvasId::from_bytes([90; 16]);
const SHADER: NodeId = NodeId::from_bytes([91; 16]);

const WRITER: &str = r#"
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

const SEPIA: &str = "```wgsl\n// param: tone | Tone | 0 | 1 | 0.8\nfn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    let g = dot(color.rgb, vec3<f32>(0.3, 0.59, 0.11));\n    return vec4<f32>(mix(color.rgb, vec3<f32>(g * 1.1, g, g * 0.8), tone()), color.a);\n}\n```";

type Runs = Arc<Mutex<Vec<PreparedRun>>>;

/// A canvas with one custom shader card, a text model, and a run host that keeps what it's given.
fn scene() -> (Harness<'static, NodeCraftApp>, Runs) {
    let snapshot = GenerationCatalog::new(RunCatalog::new(nodecraft_catalog::Catalog::from_toml(WRITER).unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([92; 16]), "Shader editor").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
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
            Ok(json!({"accepted": true}))
        })
        .unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.dispatch("node.create", &json!({"canvas": C, "id": SHADER, "kind": nodecraft_nodes::shader::ID, "pos": {"x": 300, "y": 100}})).unwrap();
    let app = NodeCraftApp::from_session(session, C).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    (h, runs)
}

/// Sends `card`'s runs to the mock provider.
fn route(h: &mut Harness<'static, NodeCraftApp>, card: NodeId) {
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(nodecraft_provider_testkit::mock::MockProvider::standard())).unwrap();
    routing.select(card, "mock", binding.target()).unwrap();
    h.state_mut().session.set_provider_routing(routing).unwrap();
}

/// Opens the editor from the card's menu.
fn opened() -> (Harness<'static, NodeCraftApp>, Runs) {
    let (mut h, runs) = scene();
    for _ in 0..20 {
        if h.query_by_label("Edit shader…").is_none() {
            right_click(&mut h, SHADER);
        }
        h.get_by_label("Edit shader…").click();
        h.run();
        if h.state().shader_editor.is_some() {
            break;
        }
    }
    assert!(h.state().shader_editor.is_some(), "{:?}", h.state().notice);
    (h, runs)
}

fn press(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    let pos = h.get_by_role_and_label(Role::Button, label).rect().center();
    // Fixed steps: a hovered button's tooltip comes and goes while the pointer is down.
    h.hover_at(pos);
    h.run_steps(3);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run_steps(3);
}

/// Focuses a text field the way a person does, by clicking it, and types at its caret.
fn type_into(h: &mut Harness<'static, NodeCraftApp>, label: &str, text: &str) {
    h.get_by_label(label).click();
    h.run();
    h.get_by_label(label).type_text(text);
    h.run();
}

/// Saves the frame as `name` when NODECRAFT_SHADER_TEST_SHOTS names a directory (PR evidence).
fn shot(h: &mut Harness<'static, NodeCraftApp>, name: &str) {
    if let Some(dir) = std::env::var_os("NODECRAFT_SHADER_TEST_SHOTS").map(std::path::PathBuf::from) {
        h.render().unwrap().save(dir.join(name)).unwrap();
    }
}

fn card_shader(h: &Harness<'static, NodeCraftApp>) -> StoredShader {
    stored(h.state().session.document().canvases().get(&C).unwrap().nodes().get(&SHADER).unwrap()).unwrap()
}

fn apply_disabled(h: &Harness<'static, NodeCraftApp>) -> bool {
    h.get_by_role_and_label(Role::Button, "Apply").accesskit_node().is_disabled()
}

#[test]
fn typing_checks_the_shader_and_apply_stores_it_as_one_undo_step() {
    let (mut h, _) = opened();
    let before = card_shader(&h);
    assert!(h.query_by_label("The shader is valid.").is_some());
    assert!(!apply_disabled(&h));

    // A stray word after the function: the compiler's message shows, and Apply waits for a fix.
    type_into(&mut h, "WGSL source", "oops");
    assert!(h.state().shader_editor.as_ref().unwrap().draft.source.contains("oops"));
    assert!(h.query_by_label("The shader is valid.").is_none(), "the draft is checked as it's typed");
    assert!(apply_disabled(&h));
    assert!(h.query_by_role_and_label(Role::Button, "Fix with a model").is_some(), "a refused shader can be sent for a fix");
    assert_eq!(card_shader(&h), before, "nothing is stored while typing");
    shot(&mut h, "shader90-refused.png");

    press(&mut h, "Revert");
    assert_eq!(h.state().shader_editor.as_ref().unwrap().draft, before);
    assert!(!apply_disabled(&h));

    press(&mut h, "Add parameter");
    assert_eq!(h.state().shader_editor.as_ref().unwrap().draft.params.len(), before.params.len() + 1);
    press(&mut h, "Apply");
    assert_eq!(h.state().shader_editor.as_ref().unwrap().error, None);
    assert_eq!(card_shader(&h).params.len(), before.params.len() + 1);
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(card_shader(&h), before, "one undo puts the old shader back");
}

#[test]
fn a_model_writes_the_shader_after_its_price_is_accepted_and_its_answer_is_stored() {
    let (mut h, runs) = opened();
    type_into(&mut h, "Ask a model", "Make it sepia");
    press(&mut h, "Write with a model");
    assert!(runs.lock().unwrap().is_empty(), "asking alone never spends");
    let (card, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    let request = h.state().session.document().canvases().get(&C).unwrap().nodes().get(&card).unwrap().clone();
    assert_eq!(request.kind, nodecraft_nodes::generation::TEXT_KIND);
    // The fixture routes the mock provider per card, and this card is new: route it and quote
    // again, as running the card by hand would.
    press(&mut h, "Cancel");
    route(&mut h, card);
    h.state_mut().request_runs(vec![card], RunKind::Generate).unwrap();
    h.run_steps(3);
    let accept = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    assert!(accept.contains("0.01"), "the price shows before the run: {accept}");
    press(&mut h, &accept);
    assert_eq!(runs.lock().unwrap().len(), 1, "one accepted run reaches the host");
    assert!(h.query_by_label_contains("Waiting for the model's answer").is_some());

    // The host records the model's answer: the editor stores it as the shader.
    let answer = json!({"id": GenerationId::from_bytes([93; 16]), "outputs": [{"type": "text", "value": SEPIA}]});
    h.state_mut().session.dispatch("run.record", &json!({"canvas": C, "results": [{"node": card, "generation": answer}]})).unwrap();
    h.run();
    let editor = h.state().shader_editor.as_ref().unwrap();
    assert_eq!(editor.pending, None);
    assert_eq!(editor.error, None);
    let shader = card_shader(&h);
    assert_eq!(shader.params.len(), 1);
    assert_eq!(shader.params.first().unwrap().name, "tone");
    assert_eq!(editor.draft, shader, "the editor shows what was stored");
    shot(&mut h, "shader90-written.png");
}

#[test]
fn an_answer_that_isnt_a_shader_says_why_and_offers_a_fix() {
    let (mut h, _) = opened();
    let before = card_shader(&h);
    type_into(&mut h, "Ask a model", "Blur it");
    press(&mut h, "Write with a model");
    let (card, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    press(&mut h, "Cancel");
    let answer = json!({"id": GenerationId::from_bytes([94; 16]), "outputs": [{"type": "text", "value": "```wgsl\nfn effect(color: vec4<f32>) -> f32 { return 1.0; }\n```"}]});
    h.state_mut().session.dispatch("run.record", &json!({"canvas": C, "results": [{"node": card, "generation": answer}]})).unwrap();
    h.run();
    let error = h.state().shader_editor.as_ref().unwrap().error.clone().unwrap();
    assert!(error.contains("isn't a shader") || error.contains("isn't valid"), "{error}");
    assert_eq!(card_shader(&h), before, "a refused answer stores nothing");
    assert!(h.query_by_label(&error).is_some(), "the message is shown");
    assert!(!h.get_by_role_and_label(Role::Button, "Fix with a model").accesskit_node().is_disabled());
}

#[test]
fn the_editor_closes_when_its_card_goes() {
    let (mut h, _) = opened();
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": C, "id": SHADER})).unwrap();
    h.run();
    assert!(h.state().shader_editor.is_none());
}
