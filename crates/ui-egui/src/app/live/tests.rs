use super::*;
use egui_kittest::Harness;
use nodecraft_types::Value;
use serde_json::json;

const REPLACE: NodeId = NodeId::from_bytes([77; 16]);

fn app() -> NodeCraftApp {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let source = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    app.session.dispatch("node.create", &json!({"canvas": canvas, "id": REPLACE, "kind": "text.replace", "pos": {"x": 420, "y": 0}})).unwrap();
    for (name, value) in [("find", "forest"), ("replacement", "sea")] {
        app.session.dispatch("node.param_set", &json!({"canvas": canvas, "id": REPLACE, "name": name, "value": Value::Text(value.into())})).unwrap();
    }
    let edge = nodecraft_types::EdgeId::from_bytes([78; 16]);
    app.session
        .dispatch(
            "edge.connect",
            &json!({"canvas": canvas, "edge": {"id": edge, "from": {"node": source, "port": "out"}, "to": {"node": REPLACE, "port": "text"}, "order": 0}}),
        )
        .unwrap();
    app
}

fn output(app: &NodeCraftApp) -> Option<String> {
    let node = app.session.document().canvases().get(&app.canvas)?.nodes().get(&REPLACE)?;
    match node.history.iter().find(|g| Some(g.id) == node.active)?.outputs.first()? {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }
}

/// Runs frames until `done` or a few seconds pass (the work is on another thread).
fn settle(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&NodeCraftApp) -> bool) {
    for _ in 0..400 {
        h.step();
        if done(h.state()) && h.state().live.running.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("live recompute didn't settle");
}

#[test]
fn an_edit_recomputes_its_action_and_one_undo_takes_both_back() {
    let mut h = Harness::builder().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    settle(&mut h, |app| output(app).is_some());
    let first = output(h.state()).unwrap();
    assert!(first.contains("sea") && !first.contains("forest"), "{first}");
    assert!(h.state().session.can_undo());
    // Change what to replace with: the result follows, without an extra undo step.
    let canvas = h.state().canvas;
    h.state_mut()
        .session
        .dispatch("node.param_set", &json!({"canvas": canvas, "id": REPLACE, "name": "replacement", "value": Value::Text("lake".into())}))
        .unwrap();
    settle(&mut h, |app| output(app).is_some_and(|o| o.contains("lake")));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.step();
    assert_eq!(output(h.state()).as_deref(), Some(first.as_str()), "the old result is back with the old value");
    // And it stays put: nothing is stale.
    for _ in 0..20 {
        h.step();
    }
    assert_eq!(output(h.state()).as_deref(), Some(first.as_str()));
}

#[test]
fn a_failing_action_is_reported_once_and_not_retried_until_an_edit() {
    let mut app = app();
    let canvas = app.canvas;
    // A split with an empty separator fails.
    let split = NodeId::from_bytes([79; 16]);
    app.session.dispatch("node.create", &json!({"canvas": canvas, "id": split, "kind": "text.split", "pos": {"x": 420, "y": 300}})).unwrap();
    app.session.dispatch("node.param_set", &json!({"canvas": canvas, "id": split, "name": "separator", "value": Value::Text(String::new())})).unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([80; 16]);
    let source = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    app.session
        .dispatch(
            "edge.connect",
            &json!({"canvas": canvas, "edge": {"id": edge, "from": {"node": source, "port": "out"}, "to": {"node": split, "port": "text"}, "order": 0}}),
        )
        .unwrap();
    let mut h = Harness::builder().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    settle(&mut h, |app| app.live.failed.contains_key(&split));
    assert!(h.state().notice.as_deref().is_some_and(|n| n.contains("separator")));
    let revision = h.state().session.revision();
    for _ in 0..20 {
        h.step();
    }
    assert!(h.state().live.running.is_none(), "not retried at the same revision");
    assert_eq!(h.state().session.revision(), revision);
}

#[test]
#[ignore = "manual public-safe PR evidence"]
fn render_live_review_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    // A public-safe gradient with stripes, so the blur shows.
    let (w, h_) = (240u32, 160u32);
    let pixels: Vec<u8> = (0..w * h_)
        .flat_map(|i| {
            let (x, y) = (i % w, i / w);
            let stripe = if (x / 12) % 2 == 0 { 255 } else { 40 };
            [stripe, (y * 255 / h_) as u8, 200, 255]
        })
        .collect();
    let png = nodecraft_media::encode_png(w, h_, &pixels, &nodecraft_media::Limits::default()).unwrap();
    let hash = app.session.import_asset(&png).unwrap();
    let asset = nodecraft_types::AssetRef {
        hash,
        mime: "image/png".parse().unwrap(),
        meta: nodecraft_types::MediaMeta { width: Some(w), height: Some(h_), ..Default::default() },
    };
    let (image, blur) = (NodeId::from_bytes([60; 16]), NodeId::from_bytes([61; 16]));
    app.session.dispatch("node.create", &json!({"canvas": canvas, "id": image, "kind": "source.image", "pos": {"x": 0, "y": 300}})).unwrap();
    app.session.dispatch("node.param_set", &json!({"canvas": canvas, "id": image, "name": "asset", "value": Value::Media(asset)})).unwrap();
    app.session.dispatch("node.create", &json!({"canvas": canvas, "id": blur, "kind": "image.blur", "pos": {"x": 420, "y": 300}})).unwrap();
    app.session
        .dispatch("node.param_set", &json!({"canvas": canvas, "id": blur, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(6.0).unwrap())}))
        .unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([62; 16]);
    app.session
        .dispatch(
            "edge.connect",
            &json!({"canvas": canvas, "edge": {"id": edge, "from": {"node": image, "port": "out"}, "to": {"node": blur, "port": "image"}, "order": 0}}),
        )
        .unwrap();
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    settle(&mut h, |app| app.session.document().canvases().get(&app.canvas).unwrap().nodes().get(&blur).unwrap().active.is_some());
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    for _ in 0..30 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    h.run();
    h.render().unwrap().save(folder.join("live.png")).unwrap();
}

#[test]
fn closing_waits_for_a_recompute_still_running() {
    // #594: a worker left running at exit can be inside a GPU driver as it is torn down.
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let worker = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    let mut live = Live::default();
    live.worker = Some(worker);
    drop(live);
    assert!(done.load(std::sync::atomic::Ordering::SeqCst), "the app closed before its worker finished");
}
