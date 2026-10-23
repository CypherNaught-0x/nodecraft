//! The ComfyUI workflow card (#9): a card placed from an imported workflow shows the ports and
//! parameters its workflow exposes, from the generated body.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_nodes::comfy_workflow::{self, Card, MappedInput, MappedOutput, MappedParam, Mapping};
use nodecraft_types::{Modality, ParamControl, ParamSpec};

const C: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([9; 16]);
const CARD: NodeId = NodeId::from_bytes([9; 16]);

/// What importing a small image-to-image workflow stores: its image loader as a port, its prompt
/// as a parameter and its Save Image node as the result.
fn card() -> Card {
    let workflow = r#"{
      "1": {"class_type": "LoadImage", "inputs": {"image": "example.png"}},
      "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a red fox"}},
      "3": {"class_type": "SaveImage", "inputs": {"images": ["1", 0], "filename_prefix": "NodeCraft"}}
    }"#;
    let prompt = ParamSpec {
        name: "prompt".into(),
        label: "Workflow prompt".into(),
        description: None,
        control: ParamControl::Text { multiline: true, max_bytes: 4096 },
        required: false,
        default: Some(Value::Text("a red fox".into())),
        visible_if: None,
    };
    Card {
        workflow: workflow.into(),
        mapping: Mapping {
            inputs: vec![MappedInput { node: "1".into(), input: "image".into(), port: "photo".into(), modality: Modality::Image }],
            params: vec![MappedParam { node: "2".into(), input: "text".into(), spec: prompt }],
            outputs: vec![MappedOutput { node: "3".into(), label: "Save Image".into(), modality: Modality::Image }],
            partners: Vec::new(),
        },
    }
}

fn app() -> NodeCraftApp {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([9; 16]), "Workflow").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds.get(comfy_workflow::ID).unwrap().instantiate_instance(CARD, card().config().unwrap(), &Default::default()).unwrap();
    node.pos = nodecraft_graph::Position { x: 150.0, y: 100.0 };
    nodecraft_graph::Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    NodeCraftApp::from_session(session, C).unwrap()
}

#[test]
fn a_workflow_card_shows_what_its_workflow_exposes() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.state_mut().selection.only(CARD);
    h.run();
    assert!(h.query_by_label("Workflow prompt").is_some(), "the exposed prompt is a parameter");
    assert!(h.query_by_label("Model").is_none(), "its model is fixed, so the card doesn't show it");
    let app = h.state();
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    assert_eq!(node.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["photo"]);
    assert_eq!(node.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["image"]);
}

#[test]
#[ignore = "manual public-safe capture; COMFY9_RENDER_DIR"]
fn comfy_workflow9_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("COMFY9_RENDER_DIR").unwrap());
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.state_mut().selection.only(CARD);
    h.run();
    h.render().unwrap().save(dir.join("comfy-workflow9-card.png")).unwrap();
}
