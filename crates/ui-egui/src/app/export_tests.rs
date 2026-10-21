//! Exporting a result from its chip menu (#557): the save dialog's answer is written at once.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

const NODE: NodeId = NodeId::from_bytes([61; 16]);
const IMAGE: nodecraft_types::GenerationId = nodecraft_types::GenerationId::from_bytes([62; 16]);

/// An image node whose only result is a small PNG, with the bytes.
fn with_image() -> (Harness<'static, NodeCraftApp>, Vec<u8>) {
    let canvas = CanvasId::from_bytes([60; 16]);
    let png = nodecraft_media::encode_png(4, 4, &[120; 64], &Default::default()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([63; 16]), "Export").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = nodecraft_graph::Node::new(NODE, "generation.image");
    node.outputs =
        vec![nodecraft_graph::Port { name: "out".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: false } }];
    // Content-addressed, so a scratch session names the asset the real one will store.
    let hash = Session::new(Project::new(ProjectId::from_bytes([64; 16]), "Scratch").unwrap()).unwrap().import_asset(&png).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    node.history.push(nodecraft_graph::Generation::new(IMAGE, vec![Value::Media(asset)]));
    node.active = Some(IMAGE);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.import_asset(&png).unwrap();
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    (h, png)
}

fn export(h: &mut Harness<'static, NodeCraftApp>, answer: Option<std::path::PathBuf>) {
    h.state_mut().exports.answer = answer;
    h.get_by_label("Result 1 of 1, active").click_secondary();
    h.run();
    h.get_by_label("Export…").click();
    h.run();
    h.run();
}

#[test]
fn the_chip_menu_exports_a_result_where_the_dialog_says() {
    let (mut h, png) = with_image();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.png");
    let revision = h.state().session.revision();

    export(&mut h, None);
    assert!(!path.exists(), "a cancelled dialog writes nothing");
    assert_eq!(h.state().notice, None);

    export(&mut h, Some(path.clone()));
    assert_eq!(std::fs::read(&path).unwrap(), png, "{:?}", h.state().notice);
    assert_eq!(h.state().notice.as_deref(), Some("Exported take.png."));
    // The dialog asked before replacing, so a file there is replaced.
    std::fs::write(&path, b"older").unwrap();
    export(&mut h, Some(path.clone()));
    assert_eq!(std::fs::read(&path).unwrap(), png);
    assert_eq!(h.state().session.revision(), revision, "exporting doesn't edit the project");

    // A folder that went away says so instead of writing elsewhere.
    export(&mut h, Some(dir.path().join("gone").join("take.png")));
    assert_eq!(h.state().notice.as_deref(), Some("the export folder doesn't exist"));
}

#[test]
fn only_media_results_offer_export() {
    assert_eq!(crate::history::suggested_name("image/png"), "result.png");
    assert_eq!(crate::history::suggested_name("model/gltf-binary"), "result.glb");
    assert_eq!(crate::history::suggested_name("application/x-unknown"), "result.bin");
    let node = nodecraft_graph::Node::new(NODE, "generation.text");
    let text = nodecraft_graph::Generation::new(IMAGE, vec![Value::Text("words".into())]);
    assert!(crate::history::export_targets(&node, &text).is_empty());
}
