//! Action node cards (#410): generated from their kinds' schemas.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

fn with_actions() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let source = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    for (i, (kind, x, y)) in [("image.blur", 420.0, -260.0), ("image.grade", 420.0, 120.0), ("text.split", 820.0, -260.0)].into_iter().enumerate() {
        let id = NodeId::from_bytes([90 + i as u8; 16]);
        app.session.dispatch("node.create", &json!({"canvas": canvas, "id": id, "kind": kind, "pos": {"x": x, "y": y}})).unwrap();
        if kind == "text.split" {
            let edge = nodecraft_types::EdgeId::from_bytes([95; 16]);
            app.session
                .dispatch(
                    "edge.connect",
                    &json!({"canvas": canvas, "edge": {"id": edge, "from": {"node": source, "port": "out"}, "to": {"node": id, "port": "text"}, "order": 0}}),
                )
                .unwrap();
        }
    }
    app
}

#[test]
fn action_cards_show_their_generated_controls() {
    let mut h = Harness::builder().with_size(egui::Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), with_actions());
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    // Inspect the next rendered frame; this test does not wait for asynchronous live results.
    h.step();
    for label in ["Blur", "Color grade", "Split text"] {
        assert!(h.query_by_label_contains(label).is_some(), "{label}");
    }
    assert!(h.query_by_label_contains("Brightness").is_some(), "a slider from the schema");
}

#[test]
#[ignore = "manual public-safe PR evidence"]
fn render_action_review_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), with_actions());
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.run();
    h.render().unwrap().save(folder.join("actions.png")).unwrap();
}

fn with_video_actions() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let kinds = [
        nodecraft_nodes::video_edit::TRIM,
        nodecraft_nodes::video_edit::SPEED,
        nodecraft_nodes::video_edit::STITCH,
        nodecraft_nodes::video_edit::REPLACE_AUDIO,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        let id = NodeId::from_bytes([100 + i as u8; 16]);
        let (x, y) = (420.0 + 400.0 * (i % 2) as f32, -260.0 + 380.0 * (i / 2) as f32);
        app.session.dispatch("node.create", &json!({"canvas": canvas, "id": id, "kind": kind, "pos": {"x": x, "y": y}})).unwrap();
    }
    app
}

#[test]
fn video_action_cards_show_their_generated_controls() {
    let mut h = Harness::builder().with_size(egui::Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), with_video_actions());
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.step();
    for label in ["Trim video", "Change speed", "Stitch videos", "Replace sound", "Start (s)", "Speed (×)", "Between videos", "New sound's volume"] {
        assert!(h.query_by_label_contains(label).is_some(), "{label}");
    }
    // The crossfade's length shows only once a crossfade is chosen.
    assert!(h.query_by_label_contains("Crossfade (s)").is_none());
}

#[test]
#[ignore = "manual public-safe PR evidence"]
fn render_video_action_review_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let mut h =
        Harness::builder().wgpu().with_size(egui::Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), with_video_actions());
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.run();
    h.render().unwrap().save(folder.join("video-actions.png")).unwrap();
}
