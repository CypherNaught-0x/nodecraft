//! Video cards (#559): a model setting the wired mode doesn't send is hidden on the card, and
//! comes back when the wire that chose that mode is removed.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_engine::import::{Incoming, import_files};
use nodecraft_engine::run_cmds::RunCatalog;

const C: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([59; 16]);
const CARD: NodeId = NodeId::from_bytes([3; 16]);
const FRAME: NodeId = NodeId::from_bytes([4; 16]);
const WIRE: nodecraft_types::EdgeId = nodecraft_types::EdgeId::from_bytes([20; 16]);

fn catalog() -> nodecraft_catalog::Catalog {
    nodecraft_catalog::Catalog::from_toml(
        r#"[[model]]
id = "fixture.clip"
provider = "mock"
provider_model = "mock-video"
label = "Fixture clip"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.05 USD" }
[[model.modes]]
id = "text-to-video"
output = "video"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.modes]]
id = "image-to-video"
output = "video"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }, { role = "first_frame", modality = "image", min = 1, max = 1 }]
params = ["seed"]
[[model.params.params]]
name = "duration"
label = "Clip length"
control = { type = "unsigned", min = "2", max = "10", step = "1" }
default = { type = "unsigned", value = "5" }
[[model.params.params]]
name = "seed"
label = "Seed"
control = { type = "unsigned", min = "0", max = "4294967295", step = "1" }
"#,
    )
    .unwrap()
}

fn app() -> NodeCraftApp {
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([59; 16]), "Video").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("fixture.clip".into())), ("prompt".into(), Value::Text("A paper boat".into()))].into();
    let mut card = kinds.get(nodecraft_nodes::generation::VIDEO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    card.pos = nodecraft_graph::Position { x: 150.0, y: 100.0 };
    nodecraft_graph::Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    let png = nodecraft_media::encode_png(4, 4, &[90; 4 * 4 * 4], &Default::default()).unwrap();
    import_files(&mut session, C, nodecraft_graph::Position { x: -300.0, y: 100.0 }, 200.0, &[Incoming { name: "frame.png", bytes: &png }], &[FRAME]).unwrap();
    NodeCraftApp::from_session(session, C).unwrap()
}

fn visible(app: &NodeCraftApp) -> BTreeSet<String> {
    let node = app.session.document().canvases().get(&C).unwrap().nodes().get(&CARD).unwrap();
    admission::admit(&app.session, node, None).unwrap().visible
}

#[test]
fn a_setting_the_wired_mode_does_not_send_is_hidden_until_the_wire_goes() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.step();
    assert!(visible(h.state()).is_superset(&["duration".to_owned(), "seed".to_owned()].into()), "text to video sends every setting");
    assert!(h.query_by_label_contains("Clip length").is_some());

    let edge = json!({"id": WIRE, "from": {"node": FRAME, "port": "out"}, "to": {"node": CARD, "port": "first_frame"}, "order": 0});
    h.state_mut().session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    h.step();
    let shown = visible(h.state());
    assert!(!shown.contains("duration"), "image to video takes no duration: {shown:?}");
    assert!(shown.contains("seed") && shown.contains("prompt"), "the mode's own settings and the card's prompt stay: {shown:?}");
    assert!(h.query_by_label_contains("Clip length").is_none());
    // The value is kept, only hidden: removing the wire shows it again unchanged.
    h.state_mut().session.dispatch("edge.disconnect", &json!({"canvas": C, "id": WIRE})).unwrap();
    h.step();
    assert!(visible(h.state()).contains("duration"));
    assert!(h.query_by_label_contains("Clip length").is_some());
}
