use crate::Session;
use nodecraft_graph::{Canvas, Change, Generation, Node, Port, Project};
use nodecraft_nodes::generation::STYLE_KIND;
use nodecraft_types::{AssetRef, CanvasId, ErrorKind, GenerationId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::{Value as Json, json};

const ALPHA: CanvasId = CanvasId::from_bytes([1; 16]);
const BETA: CanvasId = CanvasId::from_bytes([2; 16]);
const PHOTO: NodeId = NodeId::from_bytes([10; 16]);
const PAINTER: NodeId = NodeId::from_bytes([11; 16]);
const WRITER: NodeId = NodeId::from_bytes([12; 16]);
const SOUND: NodeId = NodeId::from_bytes([20; 16]);
const SOUL: NodeId = NodeId::from_bytes([21; 16]);
const INK: NodeId = NodeId::from_bytes([22; 16]);
const UNTRAINED: NodeId = NodeId::from_bytes([23; 16]);

fn g(n: u8) -> GenerationId {
    GenerationId::from_bytes([n; 16])
}

fn png(shade: u8) -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[shade; 16], &Default::default()).unwrap()
}

fn media(hash: nodecraft_types::Blake3, mime: &str) -> Value {
    Value::Media(AssetRef { hash, mime: mime.parse().unwrap(), meta: Default::default() })
}

fn port(name: &str, modality: Modality, many: bool) -> Port {
    Port { name: name.into(), port_type: PortType { modality, many } }
}

fn style(provider: &str, family: &str, trigger: &str) -> Vec<u8> {
    nodecraft_media::style::encode(&nodecraft_media::style::Style {
        provider: provider.into(),
        trainer: "trainer".into(),
        family: family.into(),
        trigger: trigger.into(),
        weights: format!("https://example.com/{trigger}"),
    })
    .unwrap()
}

fn style_card(id: NodeId, title: &str, asset: Option<Value>) -> Node {
    let mut node = Node::new(id, STYLE_KIND);
    node.title = title.into();
    node.outputs = vec![port("style", Modality::Style, false)];
    if let Some(asset) = asset {
        let made = GenerationId::from_bytes([id.as_bytes()[0].wrapping_add(100); 16]);
        node.history.push(Generation::new(made, vec![asset]));
        node.active = Some(made);
    }
    node
}

/// "Alpha" holds an imported photo and a painter that made two images (the newest reusing the
/// photo) and a writer with a text result; "Beta" holds a sound, a Soul ID, an ink style and a
/// style card not trained yet.
fn session() -> Session {
    let mut scratch = Session::new(Project::new(ProjectId::from_bytes([9; 16]), "Scratch").unwrap()).unwrap();
    let photo = scratch.import_asset(&png(10)).unwrap();
    let painted = scratch.import_asset(&png(200)).unwrap();
    let wav = b"RIFF\x24\0\0\0WAVEfmt ".to_vec();
    let sound = scratch.import_asset(&wav).unwrap();
    let soul = scratch.import_asset(&style("higgsfield", "soul-2", "mara")).unwrap();
    let ink = scratch.import_asset(&style("fal", "flux-1", "inkwash")).unwrap();

    let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Browse").unwrap();
    Change::AddCanvas { canvas: Canvas::new(ALPHA, "Alpha") }.apply(&mut project).unwrap();
    Change::AddCanvas { canvas: Canvas::new(BETA, "Beta") }.apply(&mut project).unwrap();
    let mut source = Node::new(PHOTO, "source.image");
    source.title = "Holiday photo".into();
    source.params.insert("asset".into(), media(photo, "image/png"));
    source.outputs = vec![port("out", Modality::Image, false)];
    let mut painter = Node::new(PAINTER, "test.gen");
    painter.title = "Painter".into();
    painter.outputs = vec![port("out", Modality::Image, true)];
    painter.history.push(Generation::new(g(1), vec![Value::List(vec![media(painted, "image/png")])]));
    painter.history.push(Generation::new(g(2), vec![Value::List(vec![media(photo, "image/png")])]));
    painter.active = Some(g(2));
    let mut writer = Node::new(WRITER, "test.gen");
    writer.title = "Writer".into();
    writer.outputs = vec![port("out", Modality::Text, false)];
    writer.history.push(Generation::new(g(3), vec![Value::Text("é".repeat(200))]));
    let mut speaker = Node::new(SOUND, "source.audio");
    speaker.params.insert("asset".into(), media(sound, "audio/wav"));
    for (canvas, node) in [(ALPHA, source), (ALPHA, painter), (ALPHA, writer), (BETA, speaker)] {
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
    }
    let cards = [
        style_card(SOUL, "Mara", Some(media(soul, "application/vnd.nodecraft.style+json; family=soul-2"))),
        style_card(INK, "Ink wash", Some(media(ink, "application/vnd.nodecraft.style+json; family=flux-1"))),
        style_card(UNTRAINED, "", None),
    ];
    for node in cards {
        Change::AddNode { canvas: BETA, node }.apply(&mut project).unwrap();
    }
    let mut session = Session::new(project).unwrap();
    for bytes in [png(10), png(200), wav, style("higgsfield", "soul-2", "mara"), style("fal", "flux-1", "inkwash")] {
        session.import_asset(&bytes).unwrap();
    }
    session
}

fn list(s: &mut Session, id: &str, params: Json) -> Json {
    let revision = s.revision();
    let listed = s.dispatch(id, &params).unwrap();
    assert_eq!(s.revision(), revision, "{id} is read-only");
    listed
}

fn field<'a>(listed: &'a Json, key: &str, name: &str) -> Vec<&'a Json> {
    listed[key].as_array().unwrap().iter().map(|entry| &entry[name]).collect()
}

#[test]
fn every_asset_is_listed_once_with_where_it_was_found_and_how_often_it_is_used() {
    let mut s = session();
    let listed = list(&mut s, "library.assets", json!({}));
    assert_eq!(listed["total"], 3, "{listed:#}");
    assert_eq!(field(&listed, "assets", "kind"), [&json!("image"), &json!("image"), &json!("audio")]);
    let photo = &listed["assets"][0];
    assert_eq!(photo["label"], "Holiday photo");
    assert_eq!(photo["param"], "asset");
    assert_eq!(photo["uses"], 2, "the photo is the source's parameter and the painter's newest result");
    assert_eq!(photo["source"], json!({"kind": "source.image", "param": "asset"}));
    assert_eq!(photo["stored"], true);
    let painted = &listed["assets"][1];
    assert_eq!((painted["generation"].clone(), painted["port"].clone(), painted["item"].clone()), (json!(g(1)), json!("out"), json!(0)));
    assert_eq!(listed["assets"][2]["canvas_title"], "Beta");
    assert!(listed["assets"].as_array().unwrap().iter().all(|a| a["mime"] != "application/vnd.nodecraft.style+json; family=soul-2"), "styles aren't assets");

    // Filtered by kind, canvas and search, a page at a time.
    assert_eq!(list(&mut s, "library.assets", json!({"kind": "audio"}))["total"], 1);
    assert_eq!(list(&mut s, "library.assets", json!({"canvas": BETA}))["total"], 1);
    assert_eq!(field(&list(&mut s, "library.assets", json!({"query": " PAINT "})), "assets", "label"), [&json!("Painter")]);
    let second = list(&mut s, "library.assets", json!({"offset": 1, "limit": 1}));
    assert_eq!((second["total"].clone(), field(&second, "assets", "label")), (json!(3), vec![&json!("Painter")]));
    assert_eq!(list(&mut s, "library.assets", json!({"offset": u64::MAX}))["assets"], json!([]));

    // An asset's entry is enough to put it on a canvas through the existing commands.
    let card = NodeId::from_bytes([30; 16]);
    s.dispatch("node.create", &json!({"canvas": ALPHA, "id": card, "kind": photo["source"]["kind"], "pos": {"x": 0, "y": 0}})).unwrap();
    let value = json!({"type": "media", "value": {"hash": photo["hash"], "mime": photo["mime"], "meta": photo["meta"]}});
    s.dispatch("node.param_set", &json!({"canvas": ALPHA, "id": card, "name": photo["source"]["param"], "value": value})).unwrap();
    assert_eq!(s.dispatch("library.assets", &json!({})).unwrap()["assets"][0]["uses"], 3);
}

#[test]
fn asset_listing_refuses_bad_requests() {
    let mut s = session();
    for (params, kind) in [
        (json!({"limit": 0}), ErrorKind::InvalidInput),
        (json!({"limit": 201}), ErrorKind::InvalidInput),
        (json!({"kind": "style"}), ErrorKind::InvalidInput),
        (json!({"kind": "text"}), ErrorKind::InvalidInput),
        (json!({"query": "x".repeat(257)}), ErrorKind::InvalidInput),
        (json!({"offset": -1}), ErrorKind::InvalidInput),
        (json!({"canvas": CanvasId::from_bytes([7; 16])}), ErrorKind::NotFound),
        (json!({"surprise": true}), ErrorKind::InvalidInput),
    ] {
        assert_eq!(s.enabled("library.assets", &params).unwrap_err().kind, kind, "{params}");
        assert_eq!(s.dispatch("library.assets", &params).unwrap_err().kind, kind, "{params}");
    }
}

#[test]
fn past_results_are_listed_newest_first_and_restored_through_history_select() {
    let mut s = session();
    let listed = list(&mut s, "library.history", json!({}));
    assert_eq!(listed["total"], 5, "two paintings, a text and two trained styles: {listed:#}");
    let alpha = list(&mut s, "library.history", json!({"canvas": ALPHA}));
    assert_eq!(field(&alpha, "generations", "generation"), [&json!(g(2)), &json!(g(1)), &json!(g(3))]);
    assert_eq!(field(&alpha, "generations", "index"), [&json!(2), &json!(1), &json!(1)]);
    assert_eq!(field(&alpha, "generations", "active"), [&json!(true), &json!(false), &json!(false)]);
    assert_eq!(alpha["generations"][0]["media"]["kind"], "image");
    let text = alpha["generations"][2]["text"].as_str().unwrap();
    assert_eq!(text.chars().count(), super::TEXT_PREVIEW_CHARS + 1, "quoted at a character boundary, with an ellipsis");
    assert!(text.ends_with('…'));

    let one = list(&mut s, "library.history", json!({"canvas": ALPHA, "node": PAINTER, "limit": 1, "offset": 1}));
    assert_eq!((one["total"].clone(), field(&one, "generations", "generation")), (json!(2), vec![&json!(g(1))]));
    let older = &one["generations"][0];
    s.dispatch("history.select", &json!({"canvas": older["canvas"], "id": older["node"], "generation": older["generation"]})).unwrap();
    assert_eq!(list(&mut s, "library.history", json!({"canvas": ALPHA, "node": PAINTER}))["generations"][1]["active"], true);
    // `place` is what history.extract needs to put that value on the canvas.
    let place = &older["place"];
    assert_eq!(place, &json!({"port": "out", "item": 0}));
    let extracted = NodeId::from_bytes([31; 16]);
    let params =
        json!({"canvas": ALPHA, "id": PAINTER, "generation": g(1), "port": place["port"], "item": place["item"], "node": extracted, "pos": {"x": 0, "y": 0}});
    s.dispatch("history.extract", &params).unwrap();
    let writer = &alpha["generations"][2];
    assert_eq!(writer["place"], json!({"port": "out", "item": null}));

    assert_eq!(s.dispatch("library.history", &json!({"node": PAINTER})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("library.history", &json!({"canvas": BETA, "node": PAINTER})).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(field(&list(&mut s, "library.history", json!({"query": "writer"})), "generations", "label"), [&json!("Writer")]);
}

#[test]
fn trained_styles_and_soul_ids_are_listed_across_canvases_with_the_provider_that_keeps_them() {
    let mut s = session();
    let listed = list(&mut s, "library.styles", json!({}));
    assert_eq!(listed["total"], 3, "{listed:#}");
    let by_id = |id: NodeId| listed["styles"].as_array().unwrap().iter().find(|entry| entry["id"] == json!(id)).unwrap().clone();
    let soul = by_id(SOUL);
    assert_eq!((soul["identity"].clone(), soul["kept_by"].clone(), soul["family"].clone()), (json!(true), json!("higgsfield"), json!("soul-2")));
    assert_eq!(soul["canvas"], json!(BETA));
    let ink = by_id(INK);
    assert_eq!((ink["identity"].clone(), ink["kept_by"].clone(), ink["trigger"].clone()), (json!(false), json!("fal"), json!("inkwash")));
    let untrained = by_id(UNTRAINED);
    assert_eq!((untrained["ready"].clone(), untrained["kept_by"].clone()), (json!(false), Json::Null));
    assert_eq!(field(&list(&mut s, "library.styles", json!({"query": "inkwash"})), "styles", "title"), [&json!("Ink wash")]);
    assert_eq!(list(&mut s, "library.styles", json!({"limit": 2}))["styles"].as_array().unwrap().len(), 2);
    assert_eq!(s.dispatch("library.styles", &json!({"canvas": BETA})).unwrap_err().kind, ErrorKind::InvalidInput);
}
