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

#[test]
fn a_downscaled_image_card_announces_source_dimensions() {
    let mut h = harness();
    let rgba: Vec<u8> = (0..1024 * 768).flat_map(|pixel| [(pixel % 251) as u8, ((pixel / 1024) % 251) as u8, 90, 255]).collect();
    let png = nodecraft_media::encode_png(1024, 768, &rgba, &Default::default()).unwrap();
    let canvas = h.state().canvas;
    import_files(
        &mut h.state_mut().session,
        canvas,
        nodecraft_graph::Position { x: 200.0, y: -110.0 },
        300.0,
        &[Incoming { name: "public-example.png", bytes: &png }],
        &[NodeId::from_bytes([42; 16])],
    )
    .unwrap();
    let source_label = wait_for(&mut h, "Image 1024×768").is_some();
    let thumbnail_label = h.query_by_label("Image 512×384").is_some();
    assert!(source_label || thumbnail_label, "fixture must reach a decoded image, not merely time out");
    assert!(source_label, "the decoded source is 1024×768; old thumbnail label present: {thumbnail_label}");
    assert!(!thumbnail_label);
    assert_eq!(h.state().previews.bytes(), 512 * 384 * 4, "texture accounting still uses the reduced thumbnail");
    let fitted = h
        .output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Mesh(mesh) if mesh.vertices.len() == 4 && mesh.texture_id != egui::TextureId::default() => Some(mesh.calc_bounds()),
            _ => None,
        })
        .expect("the real card paints the preview image mesh");
    assert!(fitted.width() > 0.0 && fitted.height() > 0.0);
    assert!((fitted.width() * 3.0 - fitted.height() * 4.0).abs() < 0.01, "the painted preview remains 4:3: {fitted:?}");
}

/// Open the actual card context menu and its split submenu using pointer input.
fn open_split_preview(h: &mut Harness<'static, NodeCraftApp>, id: NodeId) {
    let pos = {
        let app = h.state();
        let node = app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&id).unwrap();
        let descriptor = app.session.kinds().get(&node.kind).unwrap().descriptor();
        app.view.rect_to_screen(app.viewport.min, card::world_rect(node, Some(descriptor), app.theme.tokens())).min + Vec2::splat(6.0)
    };
    h.hover_at(pos);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    h.run();
    h.get_by_label_contains("Split into grid").hover();
    h.run();
    h.run();
}

#[test]
fn a_downscaled_split_preview_announces_source_dimensions() {
    let mut h = harness();
    let rgba = vec![90; 1024 * 768 * 4];
    let png = nodecraft_media::encode_png(1024, 768, &rgba, &Default::default()).unwrap();
    let canvas = h.state().canvas;
    let id = NodeId::from_bytes([43; 16]);
    import_files(
        &mut h.state_mut().session,
        canvas,
        nodecraft_graph::Position { x: 200.0, y: -110.0 },
        300.0,
        &[Incoming { name: "public-split-example.png", bytes: &png }],
        &[id],
    )
    .unwrap();
    assert!(wait_for(&mut h, "Image 1024×768").is_some());
    open_split_preview(&mut h, id);
    assert!(h.query_by_label("Image 1024×768, split preview 2 rows by 2 columns").is_some());
    assert!(h.query_by_label("Image 512×384, split preview 2 rows by 2 columns").is_none());
    assert_eq!(h.state().previews.bytes(), 512 * 384 * 4);
    h.key_press(egui::Key::Escape);
    h.run();
    h.run();
    assert!(h.query_by_label_contains("split preview").is_none());
    assert!(h.query_by_label("Image 1024×768").is_some());
}

#[test]
fn a_split_grid_without_asset_dimensions_uses_the_decoded_source_cut_positions() {
    let mut h = harness();
    let rgba = vec![120; 1023 * 767 * 4];
    let png = nodecraft_media::encode_png(1023, 767, &rgba, &Default::default()).unwrap();
    let hash = h.state_mut().session.import_asset(&png).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    assert!(asset.meta.width.is_none() && asset.meta.height.is_none(), "exercise the grid's decoded-size fallback");
    let supplied: nodecraft_types::ParamValues = [("asset".into(), Value::Media(asset))].into_iter().collect();
    let id = NodeId::from_bytes([44; 16]);
    let mut node = h.state().session.kinds().get("source.image").unwrap().instantiate_instance(id, Default::default(), &supplied).unwrap();
    node.pos = nodecraft_graph::Position { x: 200.0, y: -110.0 };
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("node.add", &json!({"canvas":canvas,"node":node})).unwrap();
    assert!(wait_for(&mut h, "Image 1023×767").is_some());
    h.state_mut().split_size = (3, 5);
    open_split_preview(&mut h, id);
    assert!(h.query_by_label("Image 1023×767, split preview 3 rows by 5 columns").is_some());
    let fitted = h
        .output()
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Mesh(mesh) if mesh.vertices.len() == 4 && mesh.texture_id != egui::TextureId::default() => Some(mesh.calc_bounds()),
            _ => None,
        })
        .unwrap();
    let t = h.state().theme.tokens();
    let mut vertical = Vec::new();
    let mut horizontal = Vec::new();
    for clipped in &h.output().shapes {
        if let egui::epaint::Shape::LineSegment { points, stroke } = &clipped.shape {
            if stroke.color != t.accent || stroke.width != t.port_stroke {
                continue;
            }
            let [a, b] = *points;
            if (a.x - b.x).abs() < 0.001 && (a.y - fitted.top()).abs() < 0.001 && (b.y - fitted.bottom()).abs() < 0.001 {
                vertical.push(a.x);
            }
            if (a.y - b.y).abs() < 0.001 && (a.x - fitted.left()).abs() < 0.001 && (b.x - fitted.right()).abs() < 0.001 {
                horizontal.push(a.y);
            }
        }
    }
    vertical.sort_by(f32::total_cmp);
    horizontal.sort_by(f32::total_cmp);
    let (xs, ys) = card::grid_lines(fitted, 3, 5, [1023, 767]);
    assert_eq!(vertical.len(), xs.len());
    assert_eq!(horizontal.len(), ys.len());
    for (painted, expected) in vertical.iter().chain(&horizontal).zip(xs.iter().chain(&ys)) {
        assert!((painted - expected).abs() < 0.001, "painted cut {painted} must use the original decoded dimensions, expected {expected}");
    }
}
