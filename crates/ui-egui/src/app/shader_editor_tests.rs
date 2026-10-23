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
    scene_with(None)
}

/// [`scene`], with a shader library in `shaders` when given.
fn scene_with(shaders: Option<&nodecraft_engine::library::MemoryLibrary>) -> (Harness<'static, NodeCraftApp>, Runs) {
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
    if let Some(store) = shaders {
        nodecraft_engine::shader_library::install(&mut session, Box::new(store.clone())).unwrap();
    }
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
    open(scene())
}

fn open((mut h, runs): (Harness<'static, NodeCraftApp>, Runs)) -> (Harness<'static, NodeCraftApp>, Runs) {
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
    assert!(h.query_by_label("Not applied yet").is_some(), "the draft differs from the card");
    assert_eq!(card_shader(&h), before, "nothing is stored while typing");
    shot(&mut h, "shader90-refused.png");

    press(&mut h, "Revert");
    assert_eq!(h.state().shader_editor.as_ref().unwrap().draft, before);
    assert!(!apply_disabled(&h));
    assert!(h.query_by_label("Not applied yet").is_none());

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

fn prompt_of(h: &Harness<'static, NodeCraftApp>, card: NodeId) -> String {
    match h.state().session.document().canvases().get(&C).unwrap().nodes().get(&card).unwrap().params.get("prompt") {
        Some(Value::Text(text)) => text.clone(),
        other => panic!("no typed prompt: {other:?}"),
    }
}

/// Asks a model to write the shader and declines the price: the request card stays, unrun.
fn asked(h: &mut Harness<'static, NodeCraftApp>, request: &str) -> NodeId {
    type_into(h, "Ask a model", request);
    press(h, "Write with a model");
    let (card, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    press(h, "Cancel");
    card
}

fn record(h: &mut Harness<'static, NodeCraftApp>, card: NodeId, id: u8, text: &str) {
    let answer = json!({"id": GenerationId::from_bytes([id; 16]), "outputs": [{"type": "text", "value": text}]});
    h.state_mut().session.dispatch("run.record", &json!({"canvas": C, "results": [{"node": card, "generation": answer}]})).unwrap();
    h.run();
}

#[test]
fn an_answer_that_doesnt_compile_becomes_the_draft_so_its_message_and_fix_are_about_it() {
    let (mut h, _) = opened();
    let before = card_shader(&h);
    let card = asked(&mut h, "Blur it");
    record(&mut h, card, 94, "```wgsl\nfn effect(color: vec4<f32>) -> f32 { return 1.0; }\n```");
    let editor = h.state().shader_editor.as_ref().unwrap();
    assert!(editor.draft.source.contains("-> f32"), "the refused answer is shown: {}", editor.draft.source);
    assert_eq!(card_shader(&h), before, "a refused answer stores nothing");
    assert!(apply_disabled(&h));
    assert!(h.query_by_label("Not applied yet").is_some());
    assert!(!h.get_by_role_and_label(Role::Button, "Fix with a model").accesskit_node().is_disabled());

    // The fix shows the model the shader it wrote, not the card's.
    press(&mut h, "Fix with a model");
    let (fix, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    assert_ne!(fix, card);
    let asked = prompt_of(&h, fix);
    assert!(asked.contains("-> f32 { return 1.0; }"), "{asked}");
}

#[test]
fn an_answer_with_no_shader_says_why_and_offers_a_fix() {
    let (mut h, _) = opened();
    let before = card_shader(&h);
    let card = asked(&mut h, "Blur it");
    record(&mut h, card, 95, "```wgsl\n// param: tone | Tone | 0 | 1\n```");
    let refusal = h.state().shader_editor.as_ref().unwrap().refusal.clone().unwrap();
    assert!(refusal.contains("param"), "{refusal}");
    assert_eq!(h.state().shader_editor.as_ref().unwrap().draft, before, "nothing readable replaces the draft");
    assert!(h.query_by_label(&refusal).is_some(), "the message is shown");
    press(&mut h, "Fix with a model");
    let (fix, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    assert!(prompt_of(&h, fix).contains(&refusal), "the fix carries why the answer was refused");
}

#[test]
fn a_fix_shows_the_model_the_draft_being_edited() {
    let (mut h, _) = opened();
    type_into(&mut h, "WGSL source", "oops");
    press(&mut h, "Fix with a model");
    let (card, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    assert!(prompt_of(&h, card).contains("oops"), "the typed mistake is what the model sees");
}

#[test]
fn a_declined_request_doesnt_block_asking_again() {
    let (mut h, _) = opened();
    let first = asked(&mut h, "Make it sepia");
    assert!(!h.get_by_role_and_label(Role::Button, "Write with a model").accesskit_node().is_disabled());
    press(&mut h, "Write with a model");
    let (second, _) = h.state().shader_editor.as_ref().unwrap().pending.unwrap();
    assert_ne!(second, first, "a new request card is waited for");
}

#[test]
fn a_request_cards_answer_can_be_used_later_from_its_menu() {
    let (mut h, _) = opened();
    let card = asked(&mut h, "Make it sepia");
    // The editor is closed before the answer arrives, so nothing applies it on its own.
    h.state_mut().shader_editor = None;
    record(&mut h, card, 96, SEPIA);
    assert!(card_shader(&h).params.iter().all(|p| p.name != "tone"), "nothing is stored until asked");
    for _ in 0..20 {
        if h.query_by_label("Use as shader").is_none() {
            right_click(&mut h, card);
        }
        h.get_by_label("Use as shader").click();
        h.run();
        if h.state().shader_editor.is_some() {
            break;
        }
    }
    let editor = h.state().shader_editor.as_ref().unwrap();
    assert_eq!(editor.node, SHADER, "the editor opens on the shader card the answer was for");
    assert_eq!(editor.refusal, None);
    assert_eq!(card_shader(&h).params.first().unwrap().name, "tone");
    assert_eq!(editor.draft, card_shader(&h));
}

#[test]
fn the_editor_closes_when_its_card_goes() {
    let (mut h, _) = opened();
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": C, "id": SHADER})).unwrap();
    h.run();
    assert!(h.state().shader_editor.is_none());
}

const TINT: &str = "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return color * 0.5;\n}\n";

fn pin(h: &Harness<'static, NodeCraftApp>, id: NodeId) -> Option<nodecraft_nodes::shader::Pin> {
    nodecraft_nodes::shader::pinned(h.state().session.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap())
}

#[test]
fn a_shader_is_saved_to_the_library_and_its_card_follows_a_newer_version() {
    let store = nodecraft_engine::library::MemoryLibrary::default();
    let (mut h, _) = open(scene_with(Some(&store)));
    type_into(&mut h, "Save as", "Glow");
    press(&mut h, "Save to library");
    assert!(h.query_by_label("Saved as Glow v1.").is_some(), "{:?}", h.state().shader_editor.as_ref().and_then(|e| e.error.clone()));
    assert_eq!(pin(&h, SHADER).map(|pin| pin.version), Some(1));
    assert!(h.query_by_label("From the library: Glow v1").is_some());
    // Saving again with no change keeps version 1.
    press(&mut h, "Save to library");
    assert!(h.query_by_label("Glow v1 is this shader already.").is_some());
    // Another card saves version 2.
    let other = NodeId::from_bytes([96; 16]);
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": C, "id": other, "kind": nodecraft_nodes::shader::ID, "pos": {"x": 300, "y": 400}})).unwrap();
    session.dispatch("node.shader_set", &json!({"canvas": C, "id": other, "source": TINT})).unwrap();
    session.dispatch("shader.save", &json!({"canvas": C, "id": other, "name": "glow"})).unwrap();
    // Reopened, the editor offers it, and taking it is one undo step.
    press(&mut h, "Revert");
    press(&mut h, "Update to v2");
    assert_eq!((card_shader(&h).source.as_str(), pin(&h, SHADER).map(|pin| pin.version)), (TINT, Some(2)));
    assert_eq!(h.state().shader_editor.as_ref().map(|e| e.draft.source.as_str()), Some(TINT));
    assert!(h.query_by_label("Update to v2").is_none());
    h.state_mut().session.dispatch("edit.undo", &serde_json::Value::Null).unwrap();
    assert_eq!(pin(&h, SHADER).map(|pin| pin.version), Some(1));
}

#[test]
fn saving_needs_a_name_and_an_applied_valid_shader() {
    let store = nodecraft_engine::library::MemoryLibrary::default();
    let (mut h, _) = open(scene_with(Some(&store)));
    let disabled = |h: &Harness<'static, NodeCraftApp>| h.get_by_role_and_label(Role::Button, "Save to library").accesskit_node().is_disabled();
    assert!(disabled(&h), "no name");
    type_into(&mut h, "Save as", "Glow");
    assert!(!disabled(&h));
    type_into(&mut h, "WGSL source", "oops");
    assert!(disabled(&h), "a draft that isn't applied, or valid");
}
