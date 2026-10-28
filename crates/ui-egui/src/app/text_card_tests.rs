//! The text generation card (#84): it shows its active answer above the composer, and a wire
//! into the prompt locks the prompt while keeping its draft. Nothing here calls a provider.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::{generation_catalog::GenerationCatalog, run_cmds::RunCatalog};
use nodecraft_graph::{Edge, Endpoint, Generation};
use nodecraft_types::EdgeId;

const C: CanvasId = CanvasId::from_bytes([84; 16]);
const N: NodeId = NodeId::from_bytes([85; 16]);
const S: NodeId = NodeId::from_bytes([86; 16]);
const ANSWER: &str = "Rain on the tin roof, a slow drum for the sleepless.";

const MOCK_TEXT: &str = r#"
[[model]]
id = "mock-writer"
provider = "mock"
provider_model = "mock-text"
label = "Mock writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A text card, with an active answer when `answered`, and a text source wired into its prompt
/// when `wired`.
fn scene(answered: bool, wired: bool) -> Harness<'static, NodeCraftApp> {
    let mut h = Harness::builder()
        .with_size(Vec2::new(1200.0, 850.0))
        .with_max_steps(120)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app(answered, wired));
    h.run();
    h
}

fn app(answered: bool, wired: bool) -> NodeCraftApp {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_TEXT).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([87; 16]), "Text card").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::TEXT_KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text("mock-writer".into())), ("prompt".into(), Value::Text("A haiku about rain".into()))].into(),
        )
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 400.0, y: 120.0 };
    if answered {
        let id = nodecraft_types::GenerationId::from_bytes([88; 16]);
        node.history.push(Generation::new(id, vec![Value::Text(ANSWER.into())]));
        node.active = Some(id);
    }
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    if wired {
        let mut source = kinds
            .get("source.text")
            .unwrap()
            .instantiate_instance(S, Default::default(), &[("text".into(), Value::Text("A limerick about snow".into()))].into())
            .unwrap();
        source.pos = nodecraft_graph::Position { x: 60.0, y: 120.0 };
        Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([89; 16]),
            from: Endpoint { node: S, port: "out".into() },
            to: Endpoint { node: N, port: "prompt".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    NodeCraftApp::from_session(session, C).unwrap()
}

fn card(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

#[test]
fn a_text_card_shows_its_active_answer_above_the_composer() {
    let h = scene(true, false);
    let answer = h.get_by_label(ANSWER).rect();
    let generate = h.get_by_label("Generate").rect();
    assert!(answer.bottom() <= generate.top(), "the answer sits above the composer: {answer:?} {generate:?}");
    let t = h.state().theme.tokens();
    let tall = card::world_size(card(h.state()), None, t).y;
    let h = scene(false, false);
    assert!(h.query_by_label(ANSWER).is_none(), "a card with no answer shows none");
    h.get_by_label(crate::card::generate::NO_ANSWER);
    let t = h.state().theme.tokens();
    let empty = card::world_size(card(h.state()), None, t).y;
    // The room is there before the first answer, so the card doesn't jump when one streams in (#504).
    assert!(tall - empty < t.card_preview_height, "a text card always has room for its answer; only the history strip is added: {tall} {empty}");
}

#[test]
fn a_wired_prompt_is_locked_and_keeps_its_draft() {
    let h = scene(false, true);
    let wired = format!("Prompt · {}", crate::card::generate::WIRED);
    let field = h.get_by_role_and_label(Role::TextInput, &wired);
    assert!(field.accesskit_node().is_disabled(), "connected text owns the prompt");
    assert_eq!(card(h.state()).params.get("prompt"), Some(&Value::Text("A haiku about rain".into())), "the draft is kept");
    let t = h.state().theme.tokens();
    let plain = scene(false, false);
    assert_eq!(card::world_size(card(h.state()), None, t), card::world_size(card(plain.state()), None, t), "the note takes no extra room");

    assert!(plain.query_by_label_contains(crate::card::generate::WIRED).is_none());
    assert!(!plain.get_by_role_and_label(Role::TextInput, "Prompt").accesskit_node().is_disabled(), "an unwired prompt is editable");
}

#[test]
fn only_the_active_generation_answers() {
    let mut node = app(true, false).session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
    assert_eq!(crate::card::generate::answer_of(&node), Some(ANSWER));
    let other = nodecraft_types::GenerationId::from_bytes([90; 16]);
    node.history.push(Generation::new(other, vec![Value::Text("Snow".into())]));
    assert_eq!(crate::card::generate::answer_of(&node), Some(ANSWER), "a newer result shows only once it is active");
    node.active = Some(other);
    assert_eq!(crate::card::generate::answer_of(&node), Some("Snow"));
    node.active = None;
    assert_eq!(crate::card::generate::answer_of(&node), None, "history without an active result shows no answer");
}

/// A text model that also reads up to eight images.
const MOCK_VISION: &str = r#"
[[model]]
id = "mock-reader"
provider = "mock"
provider_model = "mock-text"
label = "Mock reader"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.modes]]
id = "image-to-text"
output = "text"
inputs = [
  { role = "prompt", modality = "text", min = 1, max = 1, inline = true },
  { role = "image", modality = "image", min = 1, max = 8 },
]
"#;

/// A PNG `w`×`h` of one shade, so each size is its own asset.
fn png(w: u32, h: u32) -> Vec<u8> {
    let pixels = usize::try_from(w * h).unwrap();
    nodecraft_media::encode_png(w, h, &[40, 90, 160, 255].repeat(pixels), &Default::default()).unwrap()
}

/// An answered image-reading text card with an image source wired into its image input for each
/// of `sizes`, in order, and a text source into its prompt.
fn vision(sizes: &[(u32, u32)]) -> Harness<'static, NodeCraftApp> {
    let mut h =
        Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), vision_app(sizes));
    h.run();
    h
}

fn vision_app(sizes: &[(u32, u32)]) -> NodeCraftApp {
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_VISION).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([91; 16]), "Text card").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::TEXT_KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text("mock-reader".into())), ("prompt".into(), Value::Text("Describe these".into()))].into(),
        )
        .unwrap();
    assert!(node.inputs.iter().any(|port| port.name == "image" && port.port_type.modality == nodecraft_types::Modality::Image), "{:?}", node.inputs);
    node.pos = nodecraft_graph::Position { x: 400.0, y: 120.0 };
    let id = nodecraft_types::GenerationId::from_bytes([88; 16]);
    node.history.push(Generation::new(id, vec![Value::Text(ANSWER.into())]));
    node.active = Some(id);
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let source =
        kinds.get("source.text").unwrap().instantiate_instance(S, Default::default(), &[("text".into(), Value::Text("Two photos".into()))].into()).unwrap();
    Change::AddNode { canvas: C, node: source }.apply(&mut project).unwrap();
    let edge =
        Edge { id: EdgeId::from_bytes([89; 16]), from: Endpoint { node: S, port: "out".into() }, to: Endpoint { node: N, port: "prompt".into() }, order: 0 };
    Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let session =
        Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds.clone()).unwrap().with_resources(resources);
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    for (i, (w, h)) in sizes.iter().enumerate() {
        let hash = app.session.import_asset(&png(*w, *h)).unwrap();
        let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
        let n = u8::try_from(i).unwrap();
        let image = NodeId::from_bytes([120 + n; 16]);
        let mut source =
            kinds.get("source.image").unwrap().instantiate_instance(image, Default::default(), &[("asset".into(), Value::Media(asset))].into()).unwrap();
        source.pos = nodecraft_graph::Position { x: 0.0, y: 400.0 + f32::from(n) * 10.0 };
        app.session.dispatch("node.add", &json!({"canvas": C, "node": source})).unwrap();
        let edge = json!({"id": EdgeId::from_bytes([140 + n; 16]), "from": {"node": image, "port": "out"}, "to": {"node": N, "port": "image"}, "order": i});
        app.session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    app
}

/// Frames until `label` shows (the preview worker decodes off the frame).
fn wait_for(h: &mut Harness<'static, NodeCraftApp>, label: &str) -> bool {
    for _ in 0..200 {
        h.run();
        if h.query_by_label(label).is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    false
}

#[test]
fn wired_images_show_as_thumbnails_above_the_answer_without_resizing_the_card() {
    let mut h = vision(&[(8, 6), (4, 4)]);
    assert!(wait_for(&mut h, "Connected image 1 of 2, 8×6"), "the first wired image's thumbnail appears");
    assert!(wait_for(&mut h, "Connected image 2 of 2, 4×4"), "in wire order");
    let first = h.get_by_label("Connected image 1 of 2, 8×6").rect();
    let second = h.get_by_label("Connected image 2 of 2, 4×4").rect();
    let answer = h.get_by_label(ANSWER).rect();
    assert!(first.right() < second.left() && (first.top() - second.top()).abs() < 0.5, "one row: {first:?} {second:?}");
    assert!(first.bottom() <= answer.top(), "above the answer: {first:?} {answer:?}");
    let t = h.state().theme.tokens();
    let zoom = h.state().view.zoom;
    assert!((first.width() - t.connected_thumb * zoom).abs() < 0.5, "a thumbnail is `connected_thumb` square: {first:?}");
    let plain = vision(&[]);
    assert!(plain.query_by_label_contains("Connected image").is_none(), "nothing wired, no row");
    assert_eq!(card::world_size(card(h.state()), None, t), card::world_size(card(plain.state()), None, t), "the row takes no extra room");
    // Connected text is in the prompt, not the row; the media are what the planner would read.
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    let wired = crate::card::connected::assets(canvas, card(h.state()));
    assert_eq!(wired.len(), 2);
    assert!(wired.iter().all(|asset| crate::card::connected::MediaKind::of(asset) == crate::card::connected::MediaKind::Image));
}

#[test]
fn more_wired_images_than_fit_end_in_a_count_of_the_rest() {
    let sizes: Vec<(u32, u32)> = (1..=8).map(|n| (n, 2)).collect();
    let mut h = vision(&sizes);
    assert!(wait_for(&mut h, "Connected image 1 of 8, 1×2"));
    // A 288-point row holds six 40-point squares 6 apart: five pictures and a count of three.
    assert!(h.query_by_label_contains("Connected image 5 of 8").is_some());
    assert!(h.query_by_label_contains("Connected image 6 of 8").is_none(), "the sixth square counts the rest");
    let more = h.get_by_label("3 more connected inputs").rect();
    let last = h.get_by_label_contains("Connected image 5 of 8").rect();
    assert!(last.right() < more.left(), "the count comes last: {last:?} {more:?}");
}

#[test]
fn only_a_usable_result_or_a_source_asset_counts_as_connected() {
    let asset =
        |n: u8| nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
    let catalog = nodecraft_catalog::Catalog::from_toml(MOCK_VISION).unwrap();
    let kinds = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap().registry().unwrap();
    let reader = kinds
        .get(nodecraft_nodes::generation::TEXT_KIND)
        .unwrap()
        .instantiate_instance(N, Default::default(), &[("model".into(), Value::Text("mock-reader".into()))].into())
        .unwrap();
    let image = NodeId::from_bytes([120; 16]);
    let source =
        kinds.get("source.image").unwrap().instantiate_instance(image, Default::default(), &[("asset".into(), Value::Media(asset(5)))].into()).unwrap();
    // The hashes wired into the reader when the image card is `upstream`.
    let wired = |upstream: &nodecraft_graph::Node| {
        let mut project = Project::new(ProjectId::from_bytes([92; 16]), "Scratch").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        Change::AddNode { canvas: C, node: reader.clone() }.apply(&mut project).unwrap();
        Change::AddNode { canvas: C, node: upstream.clone() }.apply(&mut project).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([94; 16]),
            from: Endpoint { node: image, port: "out".into() },
            to: Endpoint { node: N, port: "image".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
        let canvas = project.canvases().get(&C).unwrap();
        crate::card::connected::assets(canvas, canvas.nodes().get(&N).unwrap()).into_iter().map(|a| a.hash.to_string()).collect::<Vec<_>>()
    };
    let hash = |n: u8| nodecraft_types::Blake3::from_bytes([n; 32]).to_string();
    assert_eq!(wired(&source), vec![hash(5)], "a source that hasn't run gives its asset");
    // One that has run gives its recorded result: list items in order, text left out.
    let mut ran = source.clone();
    let id = nodecraft_types::GenerationId::from_bytes([93; 16]);
    ran.history.push(Generation::new(id, vec![Value::List(vec![Value::Media(asset(7)), Value::Text("caption".into()), Value::Media(asset(8))])]));
    ran.active = Some(id);
    assert_eq!(wired(&ran), vec![hash(7), hash(8)]);
    // A result recorded for another port layout isn't used; the source's own asset is.
    let mut stale = ran.clone();
    stale.history[0].outputs.push(Value::Media(asset(9)));
    assert_eq!(wired(&stale), vec![hash(5)]);
}

#[test]
#[ignore = "manual public-safe capture; TEXT84_RENDER_PATH"]
fn text84_visual() {
    let path = std::path::PathBuf::from(std::env::var_os("TEXT84_RENDER_PATH").unwrap());
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app(true, true));
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.step();
    h.step();
    h.render().unwrap().save(path).unwrap();
}

#[test]
#[ignore = "manual public-safe capture; TEXT84_CONNECTED_RENDER_PATH"]
fn text84_connected_visual() {
    let path = std::path::PathBuf::from(std::env::var_os("TEXT84_CONNECTED_RENDER_PATH").unwrap());
    let sizes = [(48, 32), (32, 48), (40, 40), (64, 36), (36, 64), (50, 50), (30, 30), (60, 40)];
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), vision_app(&sizes));
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    assert!(wait_for(&mut h, "Connected image 1 of 8, 48×32"));
    h.step();
    h.render().unwrap().save(path).unwrap();
}
