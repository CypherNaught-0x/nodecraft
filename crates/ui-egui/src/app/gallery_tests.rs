//! A batch-fanned image result on its card (#519): a row of images per batch item.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

/// Frames until `count` widgets carry `label` (the worker decodes off the frame).
fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str, count: usize) -> bool {
    for _ in 0..200 {
        h.run();
        if h.query_all_by_label(label).count() == count {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

#[test]
fn a_fanned_image_result_shows_each_items_images_in_a_row() {
    let canvas = CanvasId::from_bytes([43; 16]);
    let pngs: Vec<Vec<u8>> = [(8u32, 6u32), (6, 8), (4, 4)]
        .iter()
        .map(|&(w, h)| nodecraft_media::encode_png(w, h, &vec![90; (w * h * 4) as usize], &Default::default()).unwrap())
        .collect();
    // Content-addressed, so a scratch session names the assets the real one will store.
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([46; 16]), "Scratch").unwrap()).unwrap();
    let images: Vec<Value> = pngs
        .iter()
        .map(|png| {
            Value::Media(nodecraft_types::AssetRef { hash: scratch.import_asset(png).unwrap(), mime: "image/png".parse().unwrap(), meta: Default::default() })
        })
        .collect();
    let mut project = Project::new(ProjectId::from_bytes([47; 16]), "Gallery").unwrap();
    Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
    let mut node = nodecraft_graph::Node::new(NodeId::from_bytes([44; 16]), "generation.image");
    node.pos = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    node.outputs =
        vec![nodecraft_graph::Port { name: "out".into(), port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Image, many: true } }];
    // The first prompt made two images, the second one.
    let made = nodecraft_types::GenerationId::from_bytes([45; 16]);
    let per_item = Value::List(vec![Value::List(images[..2].to_vec()), Value::List(images[2..].to_vec())]);
    node.history.push(nodecraft_graph::Generation::new(made, vec![per_item]));
    node.active = Some(made);
    Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    for png in &pngs {
        session.import_asset(png).unwrap();
    }
    let app = NodeCraftApp::from_session(session, canvas).unwrap();
    let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    assert!(wait_for(&mut h, "Image 8×6", 1) && wait_for(&mut h, "Image 6×8", 1) && wait_for(&mut h, "Image 4×4", 1), "every image appears");
    let top = |h: &Harness<'static, NodeCraftApp>, label: &str| h.get_by_label(label).rect().top();
    assert_eq!(top(&h, "Image 8×6"), top(&h, "Image 6×8"), "one item's images share a row");
    assert!(top(&h, "Image 4×4") > top(&h, "Image 8×6"), "the next item's start the next row");
    assert!(h.query_by_label_contains("more images").is_none(), "everything fits");
}
