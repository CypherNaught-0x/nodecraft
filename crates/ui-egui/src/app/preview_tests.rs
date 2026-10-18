//! Images on cards (#386), decoded off the frame by the preview worker.
use super::tests::harness;
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::import::{Incoming, import_files};

/// Frames until `label` shows up (the worker decodes off the frame), or `None`.
fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str) -> Option<()> {
    for _ in 0..200 {
        h.run();
        if h.query_by_label(label).is_some() {
            return Some(());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    None
}

#[test]
fn an_image_card_shows_its_picture_once_decoded() {
    let mut h = harness();
    let png = nodecraft_media::encode_png(8, 6, &[90; 8 * 6 * 4], &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let at = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    let id = NodeId::from_bytes([40; 16]);
    import_files(&mut h.state_mut().session, canvas, at, 300.0, &[Incoming { name: "pic.png", bytes: &png }], &[id]).unwrap();
    assert!(wait_for(&mut h, "Image 8×6").is_some(), "the thumbnail appears");
    assert!(h.state().previews.bytes() > 0);
    // The text card shows no image.
    assert_eq!(h.query_all_by_label("Image 8×6").count(), 1);
}

#[test]
fn a_corrupt_image_says_so_without_panicking() {
    let mut h = harness();
    let canvas = h.state().canvas;
    // Bytes that sniff as PNG but don't decode, stored as an Image node's asset.
    let bytes = b"\x89PNG\r\n\x1a\nnot really a png".to_vec();
    let hash = h.state_mut().session.import_asset(&bytes).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let supplied: nodecraft_types::ParamValues = [("asset".to_owned(), Value::Media(asset))].into_iter().collect();
    let mut node = kinds.get("source.image").unwrap().instantiate_instance(NodeId::from_bytes([41; 16]), Default::default(), &supplied).unwrap();
    node.pos = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas, "node": node})).unwrap();
    assert!(wait_for(&mut h, "Can't preview this image").is_some());
}
