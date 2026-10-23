use crate::Session;
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_types::{AssetRef, CanvasId, ErrorKind, MediaMeta, NodeId, ParamValues, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMG: NodeId = NodeId::from_bytes([3; 16]);
const TXT: NodeId = NodeId::from_bytes([4; 16]);
const GONE: NodeId = NodeId::from_bytes([5; 16]);
const DECK: NodeId = NodeId::from_bytes([9; 16]);

/// A 2×2 blue PNG.
fn blue() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[0, 0, 255, 255].repeat(4), &nodecraft_media::Limits::default()).unwrap()
}

/// An image source, a text source and a 200×100 white deck with one slide drawing a red box,
/// the image, the text, a source whose node is gone, and a text layer.
fn session() -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let png = blue();
    let asset = AssetRef {
        hash: nodecraft_types::Blake3::from_bytes(*blake3::hash(&png).as_bytes()),
        mime: "image/png".parse().unwrap(),
        meta: MediaMeta { width: Some(2), height: Some(2), ..MediaMeta::default() },
    };
    for (id, kind, x, param) in [(IMG, "source.image", 0.0, ("asset", Value::Media(asset))), (TXT, "source.text", 400.0, ("text", Value::Text("Hello".into())))]
    {
        let mut node = kinds.get(kind).unwrap().instantiate_instance(id, BTreeMap::new(), &ParamValues::new()).unwrap();
        node.pos = Position { x, y: 0.0 };
        node.params.insert(param.0.into(), param.1);
        Change::AddNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    }
    let mut s = Session::new(project).unwrap();
    s.import_asset(&png).unwrap();
    s.dispatch("deck.create", &json!({"canvas": CANVAS, "id": DECK, "pos": {"x": 900, "y": 0}})).unwrap();
    for (name, value) in [("width", 200), ("height", 100)] {
        s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": DECK, "name": name, "value": {"type": "unsigned", "value": value.to_string()}})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": DECK, "name": "background", "value": {"type": "text", "value": "#ffffff"}})).unwrap();
    // Layers are placed by their centre; these are given by their top-left corner.
    let layer = |id: &str, x: u32, y: u32, w: u32, content: Json| json!({"id": id, "name": id, "x": x + w / 2, "y": y + 25, "width": w, "height": 50, "content": content});
    let source = |origin: NodeId| json!({"kind": "source", "origin": origin.to_string(), "nth": 0});
    let layers = [
        layer("box", 0, 0, 50, json!({"kind": "shape", "shape": "rectangle", "fill": "#ff0000"})),
        layer("image", 50, 0, 50, source(IMG)),
        layer("said", 100, 0, 100, source(TXT)),
        layer("gone", 0, 50, 100, source(GONE)),
        layer("title", 100, 50, 100, json!({"kind": "text", "text": "Hi", "size": 30, "color": "#000000"})),
    ];
    s.dispatch("deck.set", &json!({"canvas": CANVAS, "id": DECK, "slides": [{"id": "one", "layers": layers}, {"id": "two"}]})).unwrap();
    s
}

fn params(path: &std::path::Path, extra: Json) -> Json {
    let mut p = json!({"canvas": CANVAS, "id": DECK, "path": path});
    if let (Json::Object(p), Json::Object(extra)) = (&mut p, extra) {
        p.extend(extra);
    }
    p
}

fn pixels(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let (_, image) = nodecraft_media::decode_image(png, &nodecraft_media::Limits::default()).unwrap();
    let rgba = image.into_rgba8();
    (rgba.width(), rgba.height(), rgba.into_raw())
}

#[test]
fn a_slide_exports_as_the_presenter_draws_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.png");
    let mut s = session();
    let revision = s.revision();
    let out = s.dispatch("deck.export", &params(&path, json!({"format": "png", "slide": "one"}))).unwrap();
    let png = std::fs::read(&path).unwrap();
    assert_eq!(out, json!({"bytes": png.len(), "slides": 1, "missing": [{"slide": "one", "layer": "gone"}]}));
    assert_eq!(s.revision(), revision, "exporting doesn't edit the document");

    let (width, height, rgba) = pixels(&png);
    assert_eq!((width, height), (200, 100), "the slide is drawn at the deck's size");
    let at = |x: u32, y: u32| <[u8; 4]>::try_from(&rgba[((y * width + x) * 4) as usize..][..4]).unwrap();
    assert_eq!(at(25, 25), [255, 0, 0, 255], "the shape");
    assert_eq!(at(75, 25), [0, 0, 255, 255], "the image source");
    assert_eq!(at(50, 75), [255, 255, 255, 255], "the gone source is an empty box…");
    assert_ne!(at(0, 75), [255, 255, 255, 255], "…with an outline");
    let inked = |x0: u32, y0: u32| (x0..x0 + 100).flat_map(|x| (y0..y0 + 50).map(move |y| (x, y))).filter(|&(x, y)| at(x, y)[0] < 128).count();
    assert!(inked(100, 0) > 20, "the text source's text is drawn");
    assert!(inked(100, 50) > 20, "the text layer is drawn");

    // The same slide always exports to the same bytes.
    let again = dir.path().join("again.png");
    s.dispatch("deck.export", &params(&again, json!({"format": "png", "slide": "one"}))).unwrap();
    assert_eq!(std::fs::read(&again).unwrap(), png);
}

#[test]
fn a_deck_exports_as_one_pdf_page_per_slide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.pdf");
    let mut s = session();
    let out = s.dispatch("deck.export", &params(&path, json!({"format": "pdf"}))).unwrap();
    assert_eq!(out["slides"], json!(2));
    let pdf = std::fs::read(&path).unwrap();
    let limits = nodecraft_media::DocumentLimits::default();
    assert_eq!(nodecraft_media::probe_document(&pdf, &limits).unwrap().pages, 2);
    let page = nodecraft_media::Document::open(&pdf, &limits).unwrap().render(0, 200, [255; 4]).unwrap();
    assert_eq!((page.width, page.height), (200, 100));
    assert_eq!(&page.rgba[((25 * 200 + 25) * 4)..][..4], [255, 0, 0, 255]);

    let again = dir.path().join("again.pdf");
    s.dispatch("deck.export", &params(&again, json!({"format": "pdf"}))).unwrap();
    assert_eq!(std::fs::read(&again).unwrap(), pdf, "exports are deterministic");
    let one = dir.path().join("one.pdf");
    assert_eq!(s.dispatch("deck.export", &params(&one, json!({"format": "pdf", "slide": "two"}))).unwrap()["missing"], json!([]));
}

#[test]
fn bad_exports_are_refused_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.png");
    let mut s = session();
    let refused = |s: &mut Session, p: Json| s.dispatch("deck.export", &p).unwrap_err().kind;
    assert_eq!(refused(&mut s, params(&path, json!({"format": "png"}))), ErrorKind::InvalidInput, "a PNG needs a slide");
    assert_eq!(refused(&mut s, params(&path, json!({"format": "png", "slide": "nope"}))), ErrorKind::NotFound);
    assert_eq!(refused(&mut s, params(&path, json!({"format": "gif", "slide": "one"}))), ErrorKind::InvalidInput);
    assert_eq!(refused(&mut s, params(std::path::Path::new("relative.png"), json!({"format": "png", "slide": "one"}))), ErrorKind::InvalidInput);
    let other = json!({"canvas": CANVAS, "id": IMG, "format": "pdf", "path": path});
    assert_eq!(refused(&mut s, other), ErrorKind::InvalidInput, "only a deck has slides");
    assert!(!path.exists());

    std::fs::write(&path, b"mine").unwrap();
    let kept = s.dispatch("deck.export", &params(&path, json!({"format": "png", "slide": "one"}))).unwrap_err();
    assert_eq!(kept.kind, ErrorKind::Conflict);
    assert_eq!(std::fs::read(&path).unwrap(), b"mine");
    s.dispatch("deck.export", &params(&path, json!({"format": "png", "slide": "one", "overwrite": true}))).unwrap();
    assert_ne!(std::fs::read(&path).unwrap(), b"mine");
}

#[test]
fn a_pdf_past_the_pixel_budget_is_refused() {
    let mut s = session();
    for (name, value) in [("width", 4096), ("height", 4096)] {
        s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": DECK, "name": name, "value": {"type": "unsigned", "value": value.to_string()}})).unwrap();
    }
    let slides: Vec<Json> = (0..20).map(|n| json!({"id": format!("s{n}")})).collect();
    s.dispatch("deck.set", &json!({"canvas": CANVAS, "id": DECK, "slides": slides})).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let error = s.dispatch("deck.export", &params(&dir.path().join("big.pdf"), json!({"format": "pdf"}))).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("fewer slides"), "{}", error.message);
}
