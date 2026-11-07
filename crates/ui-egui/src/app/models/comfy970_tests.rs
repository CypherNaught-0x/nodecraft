//! The model picker lists the ComfyUI server's own models and the imported workflows (#970),
//! driven with real pointer and key events against a fixture node list.
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::comfy_server_cmds;
use nodecraft_engine::comfy_workflow_cmds::NodeCatalog;
use nodecraft_types::ParamValues;

const C: CanvasId = CanvasId::from_bytes([0x97; 16]);
const A: NodeId = NodeId::from_bytes([1; 16]);
const WORKFLOW: NodeId = NodeId::from_bytes([2; 16]);
const OTHER: CanvasId = CanvasId::from_bytes([0x98; 16]);
const OBJECT_INFO: &str = include_str!("../../../../engine/tests/fixtures/comfy_object_info970.json");
const SDXL_WORKFLOW: &str = r#"{
  "1": {"class_type": "CheckpointLoaderSimple", "inputs": {"ckpt_name": "sd_xl_base_1.0.safetensors"}},
  "2": {"class_type": "CLIPTextEncode", "inputs": {"text": "a fox", "clip": ["1", 1]}},
  "3": {"class_type": "CLIPTextEncode", "inputs": {"text": "", "clip": ["1", 1]}},
  "4": {"class_type": "EmptyLatentImage", "inputs": {"width": 1024, "height": 1024, "batch_size": 1}},
  "5": {"class_type": "KSampler", "inputs": {"model": ["1", 0], "seed": 7, "steps": 20, "cfg": 7.0, "sampler_name": "euler", "scheduler": "normal",
        "positive": ["2", 0], "negative": ["3", 0], "latent_image": ["4", 0], "denoise": 1.0}},
  "6": {"class_type": "VAEDecode", "inputs": {"samples": ["5", 0], "vae": ["1", 2]}},
  "7": {"class_type": "SaveImage", "inputs": {"images": ["6", 0], "filename_prefix": "nc"}}
}"#;

fn read(session: &mut Session, id: u64, info: &str) {
    let nodes = NodeCatalog::parse(info.as_bytes()).unwrap().0;
    comfy_server_cmds::finish(session, id, "studio", true, Ok(Arc::new(nodes).into()));
}

/// One image card on `model` (with `checkpoint`, if given), the server `studio` read, and an
/// imported workflow card titled "Fox portrait" on another canvas.
fn app(model: &str, checkpoint: Option<&str>) -> NodeCraftApp {
    app_of("generation.image", Some(model), checkpoint)
}

fn app_of(kind: &str, model: Option<&str>, checkpoint: Option<&str>) -> NodeCraftApp {
    let mut project = Project::new(ProjectId::from_bytes([0x97; 16]), "Public server models").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddCanvas { canvas: Canvas::new(OTHER, "Workflows") }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let mut values: ParamValues = [("prompt".into(), Value::Text("A lighthouse".into()))].into();
    if let Some(model) = model {
        values.insert("model".into(), Value::Text(model.into()));
    }
    if let Some(checkpoint) = checkpoint {
        values.insert("checkpoint".into(), Value::Text(checkpoint.into()));
    }
    let node = session.kinds().get(kind).unwrap().instantiate_instance(A, Default::default(), &values).unwrap();
    session.dispatch("node.add", &json!({"canvas": C, "node": node})).unwrap();
    read(&mut session, 1, OBJECT_INFO);
    // On another canvas, so the shown canvas keeps its one card in view.
    let import = json!({"canvas": OTHER, "id": WORKFLOW, "pos": {"x": 0, "y": 0}, "workflow": SDXL_WORKFLOW, "title": "Fox portrait"});
    session.dispatch("comfy.workflow.import", &import).unwrap();
    NodeCraftApp::from_session(session, C).unwrap()
}

fn harness(model: &str, checkpoint: Option<&str>) -> Harness<'static, NodeCraftApp> {
    let app = app(model, checkpoint);
    Harness::builder().with_size(Vec2::new(1200.0, 900.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

/// Opens the picker on the card with the pointer and types `query` into its search.
fn search(h: &mut Harness<'static, NodeCraftApp>, query: &str) {
    h.get_by_label("Change model…").click();
    h.run();
    assert!(h.state().model_choice.is_some());
    h.event(egui::Event::Text(query.into()));
    h.run();
}

/// The picker window, so a card on the canvas with the same text never matches.
fn picker<'a>(h: &'a Harness<'static, NodeCraftApp>) -> egui_kittest::Node<'a> {
    h.get_by_role_and_label(egui::accesskit::Role::Window, "Choose a model")
}

fn card(h: &Harness<'static, NodeCraftApp>, id: NodeId) -> Option<nodecraft_graph::Node> {
    h.state().session.document().canvases().get(&C).unwrap().nodes().get(&id).cloned()
}

#[test]
fn a_server_checkpoint_is_listed_under_its_server_and_applied_with_the_pointer() {
    let mut h = harness("openai-images.sunburst", None);
    search(&mut h, "juggernaut");
    crate::app::panel_check::check_window(&h, egui::Id::new("model_picker157"), "Choose a model, searching");
    picker(&h).get_by_label("From ComfyUI server \"studio\"");
    picker(&h).get_by_label("Checkpoints");
    picker(&h).get_by_label("juggernaut_xl_v9.safetensors").click();
    h.run();
    assert!(matches!(h.state().model_choice.as_ref().unwrap().step, Step::Review(_)));
    h.get_by_label("Checkpoint from the ComfyUI server: juggernaut_xl_v9.safetensors");
    let before = h.state().session.document().clone();
    h.get_by_label("Apply model change").click();
    h.run();
    assert!(h.state().model_choice.is_none());
    let node = card(&h, A).unwrap();
    assert_eq!(node.params.get("model"), Some(&Value::Text("comfyui.sdxl".into())));
    assert_eq!(node.params.get("checkpoint"), Some(&Value::Text("juggernaut_xl_v9.safetensors".into())));
    assert!(node.history.is_empty(), "choosing a model never runs it");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "model and checkpoint undo together");
}

#[test]
fn loras_are_grouped_and_shown_unavailable_with_their_reason() {
    let mut h = harness("openai-images.sunburst", None);
    search(&mut h, "detail_tweaker");
    picker(&h).get_by_label("LoRAs");
    let lora = picker(&h).get_by_label("detail_tweaker.safetensors");
    assert!(lora.accesskit_node().is_disabled());
    let before = h.state().session.document().clone();
    lora.click();
    h.run();
    assert!(matches!(h.state().model_choice.as_ref().unwrap().step, Step::Picking(_)), "an unavailable entry can't be chosen");
    assert_eq!(h.state().session.document(), &before);
    assert!(picker(&h).query_all_by_label_contains("Unavailable: A LoRA").next().is_some(), "the reason is shown, not hidden");
}

#[test]
fn an_imported_workflow_is_a_choice_that_places_a_copy() {
    let mut h = harness("openai-images.sunburst", None);
    search(&mut h, "Fox portrait");
    picker(&h).get_by_label("Imported ComfyUI workflows");
    picker(&h).get_by_label("Fox portrait").click();
    h.run();
    assert!(h.state().model_choice.is_none());
    let canvas = h.state().session.document().canvases().get(&C).unwrap().clone();
    let copies: Vec<_> = canvas.nodes().values().filter(|n| n.kind == nodecraft_nodes::comfy_workflow::ID && n.id != WORKFLOW).collect();
    assert_eq!(copies.len(), 1, "one copy beside the card");
    let other = h.state().session.document().canvases().get(&OTHER).unwrap().clone();
    let source = other.nodes().get(&WORKFLOW).unwrap();
    assert_eq!(copies[0].config.get(nodecraft_nodes::comfy_workflow::MAPPING), source.config.get(nodecraft_nodes::comfy_workflow::MAPPING));
    assert_eq!(copies[0].title, "Fox portrait");
    assert_eq!(card(&h, A).unwrap().params.get("model"), Some(&Value::Text("openai-images.sunburst".into())), "the card itself is unchanged");
}

#[test]
fn the_open_picker_follows_a_refresh_and_a_new_default() {
    let mut h = harness("openai-images.sunburst", None);
    search(&mut h, "brand_new");
    assert!(picker(&h).query_by_label("brand_new.safetensors").is_none());
    let refreshed = OBJECT_INFO.replace("\"sd_xl_base_1.0.safetensors\",", "\"sd_xl_base_1.0.safetensors\", \"brand_new.safetensors\",");
    read(&mut h.state_mut().session, 2, &refreshed);
    h.run();
    assert_eq!(h.state().model_choice.as_ref().unwrap().server_read.0, Some(2), "rebuilt from the new read");
    picker(&h).get_by_label("brand_new.safetensors");
    comfy_server_cmds::forget(&mut h.state_mut().session, "studio", true);
    h.run();
    assert!(picker(&h).query_by_label("brand_new.safetensors").is_none(), "another default's list isn't shown");
}

#[test]
fn a_checkpoint_the_server_lacks_is_shown_missing_and_kept() {
    let mut h = harness("comfyui.sdxl", Some("gone.safetensors"));
    let before = h.state().session.document().clone();
    search(&mut h, "gone.safetensors");
    picker(&h).get_by_label("Missing on the server: gone.safetensors");
    assert_eq!(h.state().session.document(), &before, "nothing is substituted");
    assert_eq!(card(&h, A).unwrap().params.get("checkpoint"), Some(&Value::Text("gone.safetensors".into())));
}

#[test]
fn a_text_card_gets_no_server_section() {
    let app = app_of("generation.text", None, None);
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 900.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    search(&mut h, "juggernaut");
    assert!(picker(&h).query_by_label("From ComfyUI server \"studio\"").is_none());
    assert!(picker(&h).query_by_label("juggernaut_xl_v9.safetensors").is_none());
    assert!(picker(&h).query_by_label("Fox portrait").is_none(), "an image workflow isn't offered to a text card");
}
