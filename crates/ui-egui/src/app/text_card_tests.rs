//! The text generation card (#84): it shows its active answer above the composer, and a wire
//! into the prompt locks the prompt while keeping its draft. Nothing here calls a provider.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::{generation_catalog::GenerationCatalog, run_cmds::RunCatalog};
use nodecraft_graph::{Edge, Endpoint, Generation};
use nodecraft_types::EdgeId;

const C: CanvasId = CanvasId::from_bytes([84; 16]);
const N: NodeId = NodeId::from_bytes([85; 16]);
const S: NodeId = NodeId::from_bytes([86; 16]);
const ANSWER: &str = "Rain on the tin roof, a slow drum for the sleepless.";

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

/// A text card, with an active answer when `answered`, and a text source wired into its prompt
/// when `wired`.
fn scene(answered: bool, wired: bool) -> Harness<'static, NodeCraftApp> {
    let mut h = Harness::builder()
        .with_size(Vec2::new(1200.0, 850.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app(answered, wired));
    h.run();
    h
}

fn app(answered: bool, wired: bool) -> NodeCraftApp {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_TEXT).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([87; 16]), "Text card").unwrap();
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
        let id = nodecraft_types::GenerationId::from_bytes([88; 16]);
        node.history.push(Generation::new(id, vec![Value::Text(ANSWER.into())]));
        node.active = Some(id);
    }
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    if wired {
        let mut source = kinds
            .get("source.text")
            .unwrap()
            .instantiate_instance(S, Default::default(), &[("text".into(), Value::Text("A limerick about snow".into()))].into())
            .unwrap();
        source.pos = nodecraft_graph::Position { x: 60.0, y: 120.0 };
        Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([89; 16]),
            from: Endpoint { node: S, port: "out".into() },
            to: Endpoint { node: N, port: "prompt".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    NodeCraftApp::from_session(session, C).unwrap()
}

fn card(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

#[test]
fn a_text_card_shows_its_active_answer_above_the_composer() {
    let h = scene(true, false);
    let answer = h.get_by_label(ANSWER).rect();
    let generate = h.get_by_label("Generate").rect();
    assert!(answer.bottom() <= generate.top(), "the answer sits above the composer: {answer:?} {generate:?}");
    let t = h.state().theme.tokens();
    let tall = card::world_size(card(h.state()), None, t).y;
    let h = scene(false, false);
    assert!(h.query_by_label(ANSWER).is_none(), "a card with no answer shows none");
    let t = h.state().theme.tokens();
    let short = card::world_size(card(h.state()), None, t).y;
    assert!(tall - short >= t.card_preview_height, "the card makes room for its answer: {tall} {short}");
}

#[test]
fn a_wired_prompt_is_locked_and_keeps_its_draft() {
    let h = scene(false, true);
    assert!(h.query_by_label_contains("Connected text replaces this prompt").is_some(), "the card says why its prompt is locked");
    let field = h.get_by_role_and_label(Role::TextInput, "Prompt");
    assert!(field.accesskit_node().is_disabled(), "connected text owns the prompt");
    assert_eq!(card(h.state()).params.get("prompt"), Some(&Value::Text("A haiku about rain".into())), "the draft is kept");

    let h = scene(false, false);
    assert!(h.query_by_label_contains("Connected text replaces this prompt").is_none());
    assert!(!h.get_by_role_and_label(Role::TextInput, "Prompt").accesskit_node().is_disabled(), "an unwired prompt is editable");
}

#[test]
#[ignore = "manual public-safe capture; TEXT84_RENDER_PATH"]
fn text84_visual() {
    let path = std::path::PathBuf::from(std::env::var_os("TEXT84_RENDER_PATH").unwrap());
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app(true, true));
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.step();
    h.step();
    h.render().unwrap().save(path).unwrap();
}
