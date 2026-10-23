//! Copying a result from its chip menu (#831): the text or the image's pixels reach egui's
//! clipboard output in the same frame.
use super::*;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

const NODE: NodeId = NodeId::from_bytes([71; 16]);
const IMAGE: nodecraft_types::GenerationId = nodecraft_types::GenerationId::from_bytes([72; 16]);
const TEXT: nodecraft_types::GenerationId = nodecraft_types::GenerationId::from_bytes([73; 16]);
const VIDEO: nodecraft_types::GenerationId = nodecraft_types::GenerationId::from_bytes([74; 16]);

/// A node whose results are a 2×1 PNG, a text and a video, the text active.
fn with_results() -> Harness<'static, NodeCraftApp> {
    let canvas = CanvasId::from_bytes([70; 16]);
    let png = nodecraft_media::encode_png(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255], &Default::default()).unwrap();
    let mut project = Project::new(ProjectId::from_bytes([75; 16]), "Copy").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = nodecraft_graph::Node::new(NODE, "generation.image");
    node.outputs =
        vec![nodecraft_graph::Port { name: "out".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: false } }];
    // Content-addressed, so a scratch session names the assets the real one will store.
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([76; 16]), "Scratch").unwrap()).unwrap();
    let image = nodecraft_types::AssetRef { hash: scratch.import_asset(&png).unwrap(), mime: "image/png".parse().unwrap(), meta: Default::default() };
    let video = nodecraft_types::AssetRef { hash: scratch.import_asset(b"moov").unwrap(), mime: "video/mp4".parse().unwrap(), meta: Default::default() };
    node.history.push(nodecraft_graph::Generation::new(IMAGE, vec![Value::Media(image)]));
    node.history.push(nodecraft_graph::Generation::new(TEXT, vec![Value::Text("a quiet forest".into())]));
    node.history.push(nodecraft_graph::Generation::new(VIDEO, vec![Value::Media(video)]));
    node.active = Some(TEXT);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    session.import_asset(&png).unwrap();
    session.import_asset(b"moov").unwrap();
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.run();
    h
}

/// Presses Copy in the menu of chip `chip` and returns the clipboard commands of that frame.
fn copy(h: &mut Harness<'static, NodeCraftApp>, chip: &str) -> Vec<egui::OutputCommand> {
    h.get_by_label(chip).click_secondary();
    h.run();
    h.get_by_label("Copy").click();
    h.step();
    let commands = h.output().platform_output.commands.clone();
    h.run();
    commands.into_iter().filter(|command| matches!(command, egui::OutputCommand::CopyText(_) | egui::OutputCommand::CopyImage(_))).collect()
}

#[test]
fn the_chip_menu_copies_text_and_image_results() {
    let mut h = with_results();
    let revision = h.state().session.revision();

    let text = copy(&mut h, "Result 2 of 3, active");
    assert!(matches!(text.as_slice(), [egui::OutputCommand::CopyText(t)] if t == "a quiet forest"), "{text:?}");
    assert_eq!(h.state().notice.as_deref(), Some("Copied the text."));

    let image = copy(&mut h, "Result 1 of 3");
    let [egui::OutputCommand::CopyImage(image)] = image.as_slice() else { panic!("{image:?}") };
    assert_eq!(image.size, [2, 1]);
    assert_eq!(image.pixels, vec![egui::Color32::from_rgb(255, 0, 0), egui::Color32::from_rgb(0, 0, 255)]);
    assert_eq!(h.state().notice.as_deref(), Some("Copied the 2×1 image."));
    assert_eq!(h.state().session.revision(), revision, "copying doesn't edit the project");
}

#[test]
fn a_video_result_offers_no_copy() {
    let mut h = with_results();
    h.get_by_label("Result 3 of 3").click_secondary();
    h.run();
    assert!(h.get_by_label("Copy").accesskit_node().is_disabled());
}
