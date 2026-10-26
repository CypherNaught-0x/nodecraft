use crate::Session;
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_types::{AssetRef, CanvasId, ErrorKind, MediaMeta, NodeId, ParamValues, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMG: NodeId = NodeId::from_bytes([3; 16]);
const TXT: NodeId = NodeId::from_bytes([4; 16]);
const GONE: NodeId = NodeId::from_bytes([5; 16]);
const LOST: NodeId = NodeId::from_bytes([6; 16]);
const DECK: NodeId = NodeId::from_bytes([9; 16]);

/// A 2×2 blue PNG.
fn blue() -> Vec<u8> {
    nodecraft_media::encode_png(2, 2, &[0, 0, 255, 255].repeat(4), &nodecraft_media::Limits::default()).unwrap()
}

/// An image source, a text source, an image source whose image isn't in the project, and a
/// 200×100 white deck. Slide one draws a red box, the image, the text, a source whose node is
/// gone, and a text layer; slide two draws the lost image.
fn session() -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let png = blue();
    let asset = |bytes: &[u8]| AssetRef {
        hash: nodecraft_types::Blake3::from_bytes(*blake3::hash(bytes).as_bytes()),
        mime: "image/png".parse().unwrap(),
        meta: MediaMeta { width: Some(2), height: Some(2), ..MediaMeta::default() },
    };
    for (id, kind, x, param) in [
        (IMG, "source.image", 0.0, ("asset", Value::Media(asset(&png)))),
        (TXT, "source.text", 400.0, ("text", Value::Text("Hello".into()))),
        (LOST, "source.image", 800.0, ("asset", Value::Media(asset(b"never imported")))),
    ] {
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
    let lost = [layer("lost", 50, 25, 100, source(LOST))];
    s.dispatch("deck.set", &json!({"canvas": CANVAS, "id": DECK, "slides": [{"id": "one", "layers": layers}, {"id": "two", "layers": lost}]})).unwrap();
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
    let missing = json!([{"slide": "one", "layer": "gone"}, {"slide": "two", "layer": "lost"}]);
    assert_eq!(out["missing"], missing, "an image that isn't in the project is an empty box too");
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
    let out = s.dispatch("deck.export", &params(&one, json!({"format": "pdf", "slide": "two"}))).unwrap();
    assert_eq!(out["missing"], json!([{"slide": "two", "layer": "lost"}]));
    let page = nodecraft_media::Document::open(&std::fs::read(&one).unwrap(), &limits).unwrap().render(0, 200, [255; 4]).unwrap();
    let at = |x: usize, y: usize| &page.rgba[((y * 200 + x) * 4)..][..4];
    assert_eq!(at(100, 50), [255, 255, 255, 255], "the lost image's box is empty…");
    assert_ne!(at(50, 50), [255, 255, 255, 255], "…with an outline");
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

/// A test-only renamed bundled OFL font with a different em scale, so using its outlines is
/// observably different from silently drawing the bundled fallback. No new font asset ships.
fn small_lito() -> Vec<u8> {
    let mut bytes = include_bytes!("../../../../assets/fonts/Lato-Regular.ttf").to_vec();
    for (from, to) in [(&b"\0L\0a\0t\0o"[..], &b"\0L\0i\0t\0o"[..]), (b"Lato", b"Lito")] {
        let mut at = 0;
        while let Some(found) = bytes[at..].windows(from.len()).position(|window| window == from) {
            bytes[at + found..at + found + from.len()].copy_from_slice(to);
            at += found + from.len();
        }
    }
    let tables = usize::from(u16::from_be_bytes(bytes[4..6].try_into().unwrap()));
    let head = (0..tables).map(|n| 12 + n * 16).find(|&at| &bytes[at..at + 4] == b"head").unwrap();
    let offset = u32::from_be_bytes(bytes[head + 8..head + 12].try_into().unwrap()) as usize;
    bytes[offset + 18..offset + 20].copy_from_slice(&4096_u16.to_be_bytes());
    assert_eq!(nodecraft_actions::layers::font_info(&bytes).unwrap().name, "Lito");
    bytes
}

fn request_lito(s: &mut Session) {
    s.dispatch("deck.layer_update", &json!({"canvas": CANVAS, "id": DECK, "slide": "one", "layer": "title", "set": {"content": {"font": "Lito"}}})).unwrap();
}

#[test]
fn deck693_export_draws_imported_outlines_in_png_and_pdf_and_undo_restores_fallback() {
    for format in ["png", "pdf"] {
        let dir = tempfile::tempdir().unwrap();
        let mut s = session();
        request_lito(&mut s);
        let before = dir.path().join(format!("before.{format}"));
        s.dispatch("deck.export", &params(&before, json!({"format": format, "slide": "one"}))).unwrap();
        let fallback = std::fs::read(&before).unwrap();
        crate::deck_cmds::fonts::import(&mut s, CANVAS, DECK, &small_lito(), None).unwrap();
        let revision = s.revision();
        let after = dir.path().join(format!("after.{format}"));
        s.dispatch("deck.export", &params(&after, json!({"format": format, "slide": "one"}))).unwrap();
        assert_ne!(blake3::hash(&std::fs::read(&after).unwrap()), blake3::hash(&fallback), "{format} must draw the imported font's different outlines");
        assert_eq!(s.revision(), revision, "drawing does not edit the deck");
        s.dispatch("edit.undo", &json!({})).unwrap();
        let undone = dir.path().join(format!("undone.{format}"));
        s.dispatch("deck.export", &params(&undone, json!({"format": format, "slide": "one"}))).unwrap();
        assert_eq!(std::fs::read(&undone).unwrap(), fallback, "removing the held font restores the same fallback");
    }
}

#[test]
fn deck693_export_reports_each_visible_font_fallback_separately_from_missing_images() {
    let mut s = session();
    request_lito(&mut s);
    let dir = tempfile::tempdir().unwrap();
    let out = s.dispatch("deck.export", &params(&dir.path().join("fallback.png"), json!({"format": "png", "slide": "one"}))).unwrap();
    assert_eq!(out["font_fallbacks"], json!([{"slide": "one", "layer": "title", "font": "Lito", "resolved": "lato"}]));
    assert_eq!(out["missing"], json!([{"slide": "one", "layer": "gone"}]), "text fallbacks are not empty image boxes");
    s.dispatch("deck.layer_update", &json!({"canvas": CANVAS, "id": DECK, "slide": "one", "layer": "title", "set": {"visible": false}})).unwrap();
    let hidden = s.dispatch("deck.export", &params(&dir.path().join("hidden.png"), json!({"format": "png", "slide": "one"}))).unwrap();
    assert!(hidden.get("font_fallbacks").is_none(), "invisible text needs no fallback notice");
    s.dispatch("deck.layer_update", &json!({"canvas": CANVAS, "id": DECK, "slide": "one", "layer": "title", "set": {"visible": true, "opacity": 0.0}}))
        .unwrap();
    let transparent = s.dispatch("deck.export", &params(&dir.path().join("transparent.png"), json!({"format": "png", "slide": "one"}))).unwrap();
    assert!(transparent.get("font_fallbacks").is_none(), "fully transparent text is not drawn");
}

#[test]
fn deck693_export_falls_back_for_missing_or_damaged_held_payloads() {
    for bytes in [None, Some(b"not a font".to_vec())] {
        let mut source = session();
        request_lito(&mut source);
        let mut project = source.document().clone();
        let mut node = project.canvases().get(&CANVAS).unwrap().nodes().get(&DECK).unwrap().clone();
        let hash = nodecraft_types::Blake3::from_bytes(*blake3::hash(bytes.as_deref().unwrap_or(b"missing font")).as_bytes());
        let font = AssetRef { hash, mime: "font/ttf".parse().unwrap(), meta: Default::default() };
        node.config.insert(nodecraft_nodes::layers::FONTS_CONFIG.into(), Value::List(vec![Value::List(vec![Value::Text("Lito".into()), Value::Media(font)])]));
        Change::SetNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
        let mut loaded = Session::new(project).unwrap();
        if let Some(bytes) = bytes {
            loaded.import_asset(&bytes).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let result = loaded.dispatch("deck.export", &params(&dir.path().join("fallback.png"), json!({"format": "png", "slide": "one"}))).unwrap();
        assert_eq!(result["font_fallbacks"], json!([{"slide": "one", "layer": "title", "font": "Lito", "resolved": "lato"}]));
    }
}

#[test]
fn deck693_export_falls_back_for_oversized_indexed_fonts_before_reading() {
    use nodecraft_format::{AssetStore, NativeStorage, ProjectStorage};
    let dir = tempfile::tempdir().unwrap();
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let mut assets = AssetStore::with_context(storage.asset_context().unwrap());
    let hash = assets
        .import_reader(&mut std::io::Cursor::new(vec![0; nodecraft_actions::layers::MAX_FONT_BYTES + 1]), &Default::default(), &Default::default())
        .unwrap();
    assert!(assets.get(&hash).is_none(), "large fixture uses indexed native storage");
    let mut source = session();
    request_lito(&mut source);
    let node = source.document().canvases().get(&CANVAS).unwrap().nodes().get(&DECK).unwrap();
    let deck = crate::deck_cmds::stored(node).unwrap();
    let frame = nodecraft_actions::layers::Frame { width: 200, height: 100, background: [255; 4] };
    let drawn = nodecraft_actions::deck::drawable(&deck.slides[0], frame, |_, _| nodecraft_actions::deck::Shown::Missing);
    let what = super::SlidesExport {
        frame,
        format: super::SlideFormat::Png,
        slides: vec![("one".into(), drawn)],
        missing: vec![],
        fonts: vec![("Lito".into(), AssetRef { hash, mime: "font/ttf".parse().unwrap(), meta: Default::default() })],
    };
    let rendered = super::render(&assets, &what).unwrap();
    assert_eq!(rendered.font_fallbacks, vec![("one".into(), "title".into(), "Lito".into())]);
}
