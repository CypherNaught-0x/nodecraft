use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_types::Value;
use serde_json::json;
use std::sync::Arc;

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

/// Runs frames until `done` and no round is running, for at most [`PATIENCE`] (the work is on
/// another thread).
fn settle(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&NodeCraftApp) -> bool) {
    assert!(until(h, |app| done(app) && app.live.running.is_none()), "live recompute didn't settle");
}

/// How long a test waits for live work. Blurs run unoptimised in test builds, and a busy CI runner
/// has taken half a minute for one full-size render.
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs frames until `done`, for at most [`PATIENCE`]; whether it got there.
fn until(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&NodeCraftApp) -> bool) -> bool {
    let deadline = std::time::Instant::now() + PATIENCE;
    while std::time::Instant::now() < deadline {
        h.step();
        if done(h.state()) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
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

const SOURCE: NodeId = NodeId::from_bytes([80; 16]);
const BLUR: NodeId = NodeId::from_bytes([81; 16]);

/// An image wider than a preview's edge feeding a Blur, with the blur's first result recorded.
fn blurring() -> Harness<'static, NodeCraftApp> {
    blurring_in(Harness::builder())
}

fn blurring_in(builder: egui_kittest::HarnessBuilder<NodeCraftApp>) -> Harness<'static, NodeCraftApp> {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let pixels: Vec<u8> = (0..800u32 * 160).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    let png = nodecraft_media::encode_png(800, 160, &pixels, &nodecraft_media::Limits::default()).unwrap();
    let hash = app.session.import_asset(&png).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    for (id, kind, x) in [(SOURCE, "source.image", -900), (BLUR, "image.blur", -450)] {
        app.session.dispatch("node.create", &json!({"canvas": canvas, "id": id, "kind": kind, "pos": {"x": x, "y": -200}})).unwrap();
    }
    app.session.dispatch("node.param_set", &json!({"canvas": canvas, "id": SOURCE, "name": "asset", "value": Value::Media(asset)})).unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([82; 16]);
    app.session
        .dispatch(
            "edge.connect",
            &json!({"canvas": canvas, "edge": {"id": edge, "from": {"node": SOURCE, "port": "out"}, "to": {"node": BLUR, "port": "image"}, "order": 0}}),
        )
        .unwrap();
    let mut h = builder.with_size(egui::Vec2::new(1400.0, 900.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    settle(&mut h, |app| blurred(app).is_some());
    h
}

fn blurred(app: &NodeCraftApp) -> Option<nodecraft_types::Blake3> {
    let node = app.session.document().canvases().get(&app.canvas)?.nodes().get(&BLUR)?;
    match node.history.iter().find(|g| Some(g.id) == node.active)?.outputs.first()? {
        Value::Media(asset) => Some(asset.hash),
        _ => None,
    }
}

/// The image the preview shows for the blur, and its width.
fn previewed(app: &NodeCraftApp) -> Option<(nodecraft_types::Blake3, Option<u32>)> {
    match app.live.preview(app.canvas, BLUR)?.0.outputs.first()? {
        Value::Media(asset) => Some((asset.hash, asset.meta.width)),
        _ => None,
    }
}

#[test]
fn dragging_a_slider_previews_small_and_releasing_records_the_full_render() {
    let mut h = blurring();
    let (first, revision) = (blurred(h.state()).unwrap(), h.state().session.revision());
    let rect = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Strength").rect();
    let from = rect.center();
    h.drag_at(from);
    h.run();
    h.hover_at(from + egui::Vec2::new(8.0, 0.0));
    // Frames run while the drag holds, until the preview lands.
    until(&mut h, |app| previewed(app).is_some() && app.live.running.is_none());
    let (shown, width) = previewed(h.state()).expect("the dragged blur shows a preview");
    assert_eq!(width, Some(512), "made from the scaled-down image");
    assert_ne!(shown, first);
    assert!(h.state().session.assets().get(&shown).is_none(), "the preview isn't in the project");
    assert_eq!((blurred(h.state()), h.state().session.revision()), (Some(first), revision), "nothing recorded while it drags");
    // Release: the value is recorded, and the full-size render replaces the preview.
    h.drop_at(from + egui::Vec2::new(8.0, 0.0));
    settle(&mut h, |app| blurred(app).is_some_and(|now| now != first));
    assert!(previewed(h.state()).is_none(), "the preview is gone");
    let node = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&BLUR).unwrap();
    let Some(Value::Media(asset)) = node.history.iter().find(|g| Some(g.id) == node.active).and_then(|g| g.outputs.first()) else { panic!("a result") };
    assert_eq!(asset.meta.width, Some(800), "full size");
    // One undo takes the value and its result back together.
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(blurred(h.state()), Some(first));
}

#[test]
fn an_edit_from_elsewhere_during_a_drag_replaces_its_preview() {
    let mut h = blurring();
    let first = blurred(h.state()).unwrap();
    let rect = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Strength").rect();
    let from = rect.center();
    h.drag_at(from);
    h.run();
    h.hover_at(from + egui::Vec2::new(8.0, 0.0));
    until(&mut h, |app| previewed(app).is_some() && app.live.running.is_none());
    assert!(previewed(h.state()).is_some());
    // An agent sets the strength: its round is recorded, and the drag, still held, previews
    // again from the document as it is now.
    let canvas = h.state().canvas;
    let value = Value::Number(nodecraft_types::Number::new(1.0).unwrap());
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": canvas, "id": BLUR, "name": "sigma", "value": value})).unwrap();
    let current = |app: &NodeCraftApp| app.live.preview.as_ref().is_some_and(|preview| preview.base == app.session.revision());
    settle(&mut h, |app| blurred(app).is_some_and(|now| now != first) && current(app));
    let revision = h.state().session.revision();
    h.drop_at(from + egui::Vec2::new(8.0, 0.0));
    settle(&mut h, |app| app.session.revision() != revision && previewed(app).is_none());
}

/// PR evidence: the card before a drag, while it drags (the preview) and after release.
#[test]
#[ignore = "manual public-safe PR evidence"]
fn drag88_visual() {
    let folder = std::path::PathBuf::from(std::env::var_os("LIVE88_RENDER_DIR").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let mut h = blurring_in(Harness::builder().wgpu());
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    let shoot = |h: &mut Harness<'static, NodeCraftApp>, name: &str| {
        for _ in 0..40 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        h.render().unwrap().save(folder.join(name)).unwrap();
    };
    shoot(&mut h, "1-before.png");
    let from = h.get_by_role_and_label(egui::accesskit::Role::Slider, "Strength").rect().center();
    h.drag_at(from);
    h.run();
    h.hover_at(from + egui::Vec2::new(20.0, 0.0));
    until(&mut h, |app| previewed(app).is_some() && app.live.running.is_none());
    shoot(&mut h, "2-dragging.png");
    h.drop_at(from + egui::Vec2::new(20.0, 0.0));
    settle(&mut h, |app| previewed(app).is_none());
    shoot(&mut h, "3-released.png");
}

/// A store holding one granted, enabled text action package whose `warm` action shouts its input.
fn shout_store() -> nodecraft_plugin_store::Store<nodecraft_plugin_store::MemoryBackend> {
    use nodecraft_plugin::json::{self, Json};
    let s = |t: &str| Json::Str(t.to_owned());
    let obj = |f: Vec<(&str, Json)>| Json::Object(f.into_iter().map(|(k, v)| (k.to_owned(), v)).collect());
    let manifest = json::canonical(&obj(vec![
        ("manifest_version", Json::Int(1)),
        ("package_id", s("io.example.text")),
        ("version", s("1.0.0")),
        ("host_protocol", obj(vec![("major", Json::Int(1)), ("min_minor", Json::Int(0)), ("max_minor", Json::Int(0))])),
        ("descriptor_contract", obj(vec![("major", Json::Int(1)), ("minor", Json::Int(0))])),
        ("license", s("MIT")),
        ("runtime", obj(vec![("kind", s("text_actions")), ("schema", Json::Int(1))])),
        ("dependencies", Json::Array(vec![])),
        ("capabilities", obj(vec![("required", Json::Array(vec![])), ("optional", Json::Array(vec![]))])),
        ("registrations", Json::Array(vec![obj(vec![("id", s("warm")), ("kind", s("action")), ("descriptor", s("d/warm.json"))])])),
    ]))
    .unwrap();
    let definition = r#"{"id":"user.shout","inputs":[{"kind":"text","name":"words"}],"label":"Shout","outputs":[{"expr":"upper(words)","kind":"text","name":"out"}],"schema":1,"version":1}"#;
    let bytes =
        nodecraft_plugin_store::archive::build(&[("manifest.json", manifest.as_bytes()), ("LICENSE", b"MIT\n"), ("d/warm.json", definition.as_bytes())], None)
            .unwrap();
    let mut store = nodecraft_plugin_store::Store::open(nodecraft_plugin_store::MemoryBackend::default()).unwrap();
    let trust = nodecraft_plugin::signature::TrustStore::default();
    let id = nodecraft_plugin::id::PackageId::parse("io.example.text").unwrap();
    store.import(&bytes, &trust, false).unwrap();
    store.grant(&id, &trust).unwrap();
    store.enable(&id, &trust, &Default::default(), &Default::default()).unwrap();
    store
}

#[test]
fn a_plugin_card_that_ran_before_its_plugin_was_published_runs_once_it_is() {
    use nodecraft_engine::plugin_runtime::PluginSnapshot;
    let store = shout_store();
    let mut app = app();
    let canvas = app.canvas;
    let source = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    let shout = NodeId::from_bytes([81; 16]);
    app.session.set_plugins(PluginSnapshot::of(&store)).unwrap();
    app.session.dispatch("node.create", &json!({"canvas": canvas, "id": shout, "kind": "plugin.io.example.text.warm", "pos": {"x": 420, "y": 300}})).unwrap();
    let edge = json!({"id": nodecraft_types::EdgeId::from_bytes([82; 16]), "from": {"node": source, "port": "out"}, "to": {"node": shout, "port": "words"}, "order": 0});
    app.session.dispatch("edge.connect", &json!({"canvas": canvas, "edge": edge})).unwrap();
    // As when a project opens before the plugin worker has published anything.
    app.session.set_plugins(PluginSnapshot::default()).unwrap();
    let shouted = |app: &NodeCraftApp| {
        let node = app.session.document().canvases().get(&app.canvas)?.nodes().get(&shout)?;
        node.history.iter().find(|g| Some(g.id) == node.active)?.outputs.first().cloned()
    };
    let mut h = Harness::builder().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    settle(&mut h, |app| app.live.checked.is_some() && output(app).is_some());
    assert_eq!(shouted(h.state()), None, "nothing ran it");
    // The worker publishes; the window swaps it in, at the same revision.
    let revision = h.state().session.revision();
    h.state_mut().session.set_plugins(PluginSnapshot::of(&store)).unwrap();
    h.state_mut().plugins_changed();
    settle(&mut h, |app| shouted(app).is_some());
    let text = match shouted(h.state()) {
        Some(Value::Text(text)) => text,
        other => panic!("{other:?}"),
    };
    assert!(!text.is_empty() && text == text.to_uppercase(), "{text}");
    assert!(h.state().session.revision() > revision, "recorded");
}
