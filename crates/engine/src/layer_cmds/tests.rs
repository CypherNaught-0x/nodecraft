use super::*;
use crate::Session;
use nodecraft_types::{EdgeId, ProjectId};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([3; 16]);
const B: NodeId = NodeId::from_bytes([4; 16]);
const EDITOR: NodeId = NodeId::from_bytes([5; 16]);

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    for (id, kind, x) in [(A, "source.image", 0), (B, "source.image", 0), (EDITOR, ID, 400)] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": x, "y": 0}})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": EDITOR, "name": "width", "value": Value::Unsigned(nodecraft_types::Unsigned(400))})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": EDITOR, "name": "height", "value": Value::Unsigned(nodecraft_types::Unsigned(200))})).unwrap();
    for (n, from) in [(1u8, A), (2, B)] {
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": INPUT}, "order": n});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }
    s
}

fn list(s: &mut Session) -> Vec<Json> {
    let listed = s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!((&listed["width"], &listed["height"]), (&json!(400), &json!(200)));
    listed["layers"].as_array().unwrap().clone()
}

fn ids(s: &mut Session) -> Vec<String> {
    list(s).iter().map(|l| l["id"].as_str().unwrap().to_owned()).collect()
}

fn title() -> Json {
    json!({"id": "title", "name": "Title", "x": 100, "y": 50, "width": 120, "height": 40, "content": {"kind": "text", "text": "Hi", "size": 30, "color": "#112233"}})
}

fn on(layer: &str) -> Json {
    json!({"canvas": CANVAS, "id": EDITOR, "layer": layer})
}

#[test]
fn each_wired_image_is_listed_as_a_fitted_layer_until_it_is_stored() {
    let mut s = session();
    let (a, b) = (layers::auto_id(&A.to_string(), 0), layers::auto_id(&B.to_string(), 0));
    assert_eq!(ids(&mut s), [a.clone(), b.clone()]);
    // Each input names its origin; an empty source shows no image yet.
    let listed = s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!(listed["inputs"], json!([{"origin": A.to_string(), "image": null}, {"origin": B.to_string(), "image": null}]));
    let first = list(&mut s).remove(0);
    assert_eq!(first["content"], json!({"kind": "source", "origin": A.to_string(), "nth": 0, "fit": true}));
    assert_eq!((&first["x"], &first["width"]), (&json!(200.0), &json!(400.0)));
    // Adding a text layer stores the image layers below it, in place.
    let added = s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    assert_eq!(added, json!({"changed": true, "layer": "title"}));
    assert_eq!(ids(&mut s), [a.clone(), b.clone(), "title".into()]);
    assert_eq!(stored(&node(&s)).unwrap().layers.len(), 3);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().layers.len(), 0, "one undo step");
    // A layer id is unique, and only text and shapes are added.
    s.dispatch("edit.redo", &json!({})).unwrap();
    let again = s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap_err();
    assert_eq!(again.kind, ErrorKind::Conflict);
    let mut image = title();
    image["id"] = json!("img");
    image["content"] = json!({"kind": "source", "origin": "x", "nth": 0});
    assert!(s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": image})).is_err());
}

fn node(s: &Session) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&EDITOR).unwrap().clone()
}

#[test]
fn update_merges_fields_and_respects_the_lock() {
    let mut s = session();
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    let mut change = on("title");
    change["set"] = json!({"x": 10, "rotation": 90, "content": {"text": "Hello", "align": "center"}});
    assert_eq!(s.dispatch("layers.update", &change).unwrap(), json!({"changed": true}));
    let layer = stored(&node(&s)).unwrap().get("title").cloned().unwrap();
    assert_eq!((layer.x, layer.rotation), (10.0, 90.0));
    assert!(matches!(&layer.content, Content::Text { text, size, align: layers::Align::Center, .. } if text == "Hello" && *size == 30.0));
    // The kind and the id stay.
    for set in [json!({"content": {"kind": "shape"}}), json!({"id": "other"}), json!({"opacity": 2})] {
        change["set"] = set;
        assert_eq!(s.dispatch("layers.update", &change).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    // Locked: no moving, but renaming and unlocking-while-moving work.
    change["set"] = json!({"locked": true});
    s.dispatch("layers.update", &change).unwrap();
    change["set"] = json!({"y": 5});
    assert!(s.dispatch("layers.update", &change).is_err());
    change["set"] = json!({"name": "Heading"});
    s.dispatch("layers.update", &change).unwrap();
    change["set"] = json!({"locked": false, "y": 5});
    s.dispatch("layers.update", &change).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().get("title").map(|l| (l.name.as_str(), l.y, l.locked)), Some(("Heading", 5.0, false)));
    // The same value again changes nothing.
    assert_eq!(s.dispatch("layers.update", &change).unwrap(), json!({"changed": false}));
    // Changing an automatic image layer stores it.
    let a = layers::auto_id(&A.to_string(), 0);
    let mut hide = on(&a);
    hide["set"] = json!({"visible": false});
    s.dispatch("layers.update", &hide).unwrap();
    assert_eq!(stored(&node(&s)).unwrap().get(&a).map(|l| l.visible), Some(false));
}

#[test]
fn reorder_and_remove_keep_image_layers_while_they_are_wired() {
    let mut s = session();
    let (a, b) = (layers::auto_id(&A.to_string(), 0), layers::auto_id(&B.to_string(), 0));
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    let mut to = on("title");
    to["index"] = json!(0);
    s.dispatch("layers.reorder", &to).unwrap();
    assert_eq!(ids(&mut s), ["title".to_owned(), a.clone(), b.clone()]);
    let mut top = on(&a);
    top["index"] = json!(64);
    s.dispatch("layers.reorder", &top).unwrap();
    assert_eq!(ids(&mut s), ["title".to_owned(), b.clone(), a.clone()]);
    assert_eq!(s.dispatch("layers.remove", &on(&a)).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("layers.remove", &on("nope")).unwrap_err().kind, ErrorKind::NotFound);
    s.dispatch("layers.remove", &on("title")).unwrap();
    assert_eq!(ids(&mut s), [b.clone(), a.clone()]);
    // Unwiring an image keeps its stored layer listed (not drawn) until it is deleted.
    s.dispatch("edge.disconnect", &json!({"canvas": CANVAS, "id": EdgeId::from_bytes([1; 16])})).unwrap();
    assert_eq!(ids(&mut s), [b.clone(), a.clone()]);
    s.dispatch("layers.remove", &on(&a)).unwrap();
    assert_eq!(ids(&mut s), [b]);
}

#[test]
fn align_moves_by_bounds_against_the_frame_or_the_selection() {
    let mut s = session();
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": title()})).unwrap();
    let mut square = title();
    square["id"] = json!("sq");
    square["x"] = json!(300);
    square["y"] = json!(150);
    square["width"] = json!(20);
    square["height"] = json!(20);
    square["rotation"] = json!(45);
    square["content"] = json!({"kind": "shape", "shape": "rectangle", "fill": "#ff0000"});
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": square})).unwrap();
    let align =
        |s: &mut Session, layers: Json, to: &str| s.dispatch("layers.align", &json!({"canvas": CANVAS, "id": EDITOR, "layers": layers, "to": to})).unwrap();
    // One layer: against the frame.
    align(&mut s, json!(["title"]), "right");
    let title = stored(&node(&s)).unwrap().get("title").cloned().unwrap();
    assert!((title.bounds().2 - 400.0).abs() < 1e-9 && title.y == 50.0, "{:?}", title.bounds());
    // Several: against their joint box, rotation included (the square's corners reach 14.14 px).
    align(&mut s, json!(["title", "sq"]), "top");
    let layers = stored(&node(&s)).unwrap();
    let top = |id: &str| layers.get(id).map(|l| l.bounds().1).unwrap();
    assert!((top("title") - 30.0).abs() < 1e-9 && (top("sq") - 30.0).abs() < 1e-9, "{} {}", top("title"), top("sq"));
    // A locked layer stays and is reported.
    let mut lock = on("sq");
    lock["set"] = json!({"locked": true});
    s.dispatch("layers.update", &lock).unwrap();
    let result = align(&mut s, json!(["title", "sq"]), "left");
    assert_eq!(result["locked"], json!(["sq"]));
    assert!(s.dispatch("layers.align", &json!({"canvas": CANVAS, "id": EDITOR, "layers": [], "to": "left"})).is_err());
}

#[test]
fn only_a_layer_editor_has_layers_and_bad_layers_are_refused() {
    let mut s = session();
    assert_eq!(s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": A})).unwrap_err().kind, ErrorKind::InvalidInput);
    let mut huge = title();
    huge["width"] = json!(1e9);
    assert!(s.dispatch("node.layers_set", &json!({"canvas": CANVAS, "id": EDITOR, "layers": [huge]})).is_err());
    let set = json!({"canvas": CANVAS, "id": EDITOR, "layers": [title()]});
    assert_eq!(s.dispatch("node.layers_set", &set).unwrap(), json!({"changed": true}));
    assert_eq!(s.dispatch("node.layers_set", &set).unwrap(), json!({"changed": false}));
    // A node whose stored layers are corrupt reports it rather than listing nothing.
    let mut corrupt = node(&s);
    corrupt.config.insert(CONFIG.into(), Value::Text("{\"layers\": 3}".into()));
    let project = s.document().clone();
    let mut project = project;
    Change::SetNode { canvas: CANVAS, node: corrupt }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    assert!(s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).is_err());
}

#[test]
fn a_wire_carrying_several_images_lists_a_layer_for_each_like_a_run() {
    let mut s = session();
    const BLUR: NodeId = NodeId::from_bytes([6; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": BLUR, "kind": "image.blur", "pos": {"x": 0, "y": 300}})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([3; 16]), "from": {"node": BLUR, "port": "out"}, "to": {"node": EDITOR, "port": INPUT}, "order": 3});
    s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    // Without a result the node counts as one image.
    let blur = |nth: u32| layers::auto_id(&BLUR.to_string(), nth);
    assert_eq!(ids(&mut s).last(), Some(&blur(0)));
    let asset =
        |n: u8| nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([n; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
    let generation = nodecraft_graph::Generation {
        id: nodecraft_types::GenerationId::from_bytes([9; 16]),
        outputs: vec![Value::List(vec![Value::Media(asset(1)), Value::Media(asset(2)), Value::Media(asset(3))])],
        input_hash: None,
        items: Vec::new(),
        settings: Vec::new(),
        incomplete: None,
    };
    let mut project = s.document().clone();
    Change::AddGeneration { canvas: CANVAS, id: BLUR, index: 0, generation }.apply(&mut project).unwrap();
    Change::SetActiveGeneration { canvas: CANVAS, id: BLUR, generation: Some(nodecraft_types::GenerationId::from_bytes([9; 16])) }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let listed = ids(&mut s);
    assert_eq!(listed.get(2..), Some([blur(0), blur(1), blur(2)].as_slice()), "{listed:?}");
    let inputs = s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap()["inputs"].clone();
    let images: Vec<Json> = inputs.as_array().unwrap().iter().map(|i| i["image"]["hash"].clone()).collect();
    assert_eq!(images.get(2..).map(<[Json]>::len), Some(3));
    assert_eq!(images.get(3), Some(&serde_json::to_value(asset(2).hash).unwrap()), "the second item's own image");
}

#[test]
fn a_stored_id_that_clashes_with_an_automatic_one_gets_a_free_id() {
    let mut s = session();
    let a = layers::auto_id(&A.to_string(), 0);
    let mut clash = title();
    clash["id"] = json!(a.clone());
    s.dispatch("layers.add", &json!({"canvas": CANVAS, "id": EDITOR, "layer": clash})).unwrap_err();
    // Stored directly, the text takes the id and the image layer gets another.
    s.dispatch("node.layers_set", &json!({"canvas": CANVAS, "id": EDITOR, "layers": [clash]})).unwrap();
    let listed = ids(&mut s);
    assert_eq!(listed.len(), 3, "{listed:?}");
    assert_eq!(listed.iter().collect::<std::collections::BTreeSet<_>>().len(), 3, "unique: {listed:?}");
    // Editing still works.
    let mut hide = on(listed.get(1).unwrap());
    hide["set"] = json!({"visible": false});
    s.dispatch("layers.update", &hide).unwrap();
    assert_eq!(ids(&mut s), listed);
}

#[test]
fn over_the_layer_cap_a_stored_layer_can_still_be_removed() {
    let mut s = session();
    let shapes: Vec<Json> = (0..layers::MAX_LAYERS - 1)
        .map(|n| json!({"id": format!("s{n}"), "name": "S", "x": 10, "y": 10, "width": 5, "height": 5, "content": {"kind": "shape", "shape": "rectangle", "fill": "#000000"}}))
        .collect();
    s.dispatch("node.layers_set", &json!({"canvas": CANVAS, "id": EDITOR, "layers": shapes})).unwrap();
    // 63 stored and two wired images: 65 listed, and an edit that keeps them all says why.
    assert_eq!(ids(&mut s).len(), layers::MAX_LAYERS + 1);
    let mut hide = on("s0");
    hide["set"] = json!({"visible": false});
    let refused = s.dispatch("layers.update", &hide).unwrap_err();
    assert!(refused.message.contains("at most 64 layers"), "{refused:?}");
    s.dispatch("layers.remove", &on("s0")).unwrap();
    assert_eq!(ids(&mut s).len(), layers::MAX_LAYERS);
}

/// Lato under another name, so it isn't the built-in font.
fn lito() -> Vec<u8> {
    let mut bytes = include_bytes!("../../../../assets/fonts/Lato-Regular.ttf").to_vec();
    for (from, to) in [(&b"\0L\0a\0t\0o"[..], &b"\0L\0i\0t\0o"[..]), (b"Lato", b"Lito")] {
        let mut at = 0;
        while let Some(found) = bytes[at..].windows(from.len()).position(|w| w == from) {
            bytes[at + found..at + found + from.len()].copy_from_slice(to);
            at += found + from.len();
        }
    }
    bytes
}

fn font(s: &mut Session, bytes: &[u8]) -> Json {
    let hash = s.import_asset(bytes).unwrap();
    json!({"canvas": CANVAS, "id": EDITOR, "font": {"hash": hash, "mime": "application/octet-stream"}})
}

#[test]
fn a_font_is_added_by_the_name_it_declares_and_removed_by_name() {
    let mut s = session();
    let bytes = lito();
    let params = font(&mut s, &bytes);
    assert_eq!(s.dispatch("layers.font_add", &params).unwrap(), json!({"changed": true, "name": "Lito"}));
    let listed = s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!(listed["fonts"][0]["name"], json!("Lito"));
    assert_eq!(listed["fonts"][0]["font"]["mime"], json!("font/ttf"), "the type the file declares, not the caller's");
    // The same font again changes nothing; a font of the same name replaces it.
    assert_eq!(s.dispatch("layers.font_add", &params).unwrap()["changed"], json!(false));
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(nodecraft_nodes::layers::fonts(&node(&s)).is_empty(), "one undo step");
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(s.dispatch("layers.font_remove", &json!({"canvas": CANVAS, "id": EDITOR, "name": " lito "})).unwrap(), json!({"changed": true}));
    assert!(!node(&s).config.contains_key(nodecraft_nodes::layers::FONTS_CONFIG));
    let gone = s.dispatch("layers.font_remove", &json!({"canvas": CANVAS, "id": EDITOR, "name": "Lito"})).unwrap_err();
    assert_eq!(gone.kind, ErrorKind::NotFound);
}

#[test]
fn only_a_font_the_project_holds_can_be_added() {
    let mut s = session();
    // Not a font, the built-in font, and a font that was never imported.
    let png = font(&mut s, b"\x89PNG\r\n\x1a\n not a font");
    assert_eq!(s.dispatch("layers.font_add", &png).unwrap_err().kind, ErrorKind::InvalidInput);
    let lato = font(&mut s, include_bytes!("../../../../assets/fonts/Lato-Regular.ttf"));
    assert_eq!(s.dispatch("layers.font_add", &lato).unwrap_err().kind, ErrorKind::InvalidInput);
    let missing = json!({"canvas": CANVAS, "id": EDITOR, "font": {"hash": "ab".repeat(32), "mime": "font/ttf"}});
    assert_eq!(s.dispatch("layers.font_add", &missing).unwrap_err().kind, ErrorKind::NotFound);
    // Only a Layer Editor holds fonts.
    let mut on_source = font(&mut s, &lito());
    on_source["id"] = json!(A);
    assert!(s.dispatch("layers.font_add", &on_source).is_err());
    assert!(!node(&s).config.contains_key(nodecraft_nodes::layers::FONTS_CONFIG));
}

#[test]
fn a_node_holds_at_most_sixteen_fonts() {
    let mut s = session();
    let base = lito();
    for n in 0..=layers::MAX_FONTS {
        // Each its own family: "Lita", "Litb", … (one byte changed in each name string).
        let mut bytes = base.clone();
        let digit = b'a' + n as u8;
        for (from, to) in [(&b"\0L\0i\0t\0o"[..], vec![0, b'L', 0, b'i', 0, b't', 0, digit]), (b"Lito", vec![b'L', b'i', b't', digit])] {
            let mut at = 0;
            while let Some(found) = bytes[at..].windows(from.len()).position(|w| w == from) {
                bytes[at + found..at + found + from.len()].copy_from_slice(&to);
                at += found + from.len();
            }
        }
        let params = font(&mut s, &bytes);
        let added = s.dispatch("layers.font_add", &params);
        if n < layers::MAX_FONTS {
            assert_eq!(added.unwrap()["name"], json!(format!("Lit{}", digit as char)));
        } else {
            assert_eq!(added.unwrap_err().kind, ErrorKind::InvalidInput, "the seventeenth is refused");
        }
    }
}

#[test]
fn importing_a_font_file_adds_it_and_a_file_that_isnt_one_stores_nothing() {
    let mut s = session();
    assert_eq!(fonts::import(&mut s, CANVAS, EDITOR, &lito()).unwrap(), "Lito");
    assert_eq!(nodecraft_nodes::layers::fonts(&node(&s)).len(), 1);
    let held = s.assets().len();
    let refused = fonts::import(&mut s, CANVAS, EDITOR, b"\x89PNG\r\n\x1a\n").unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);
    assert_eq!(s.assets().len(), held, "refused before it was stored");
}

#[test]
fn a_damaged_font_list_still_lists_and_the_next_font_change_repairs_it() {
    let s = session();
    let mut damaged = node(&s);
    let bad = Value::List(vec![Value::Text("Bad".into()), Value::Text("not a file".into())]);
    damaged.config.insert(nodecraft_nodes::layers::FONTS_CONFIG.into(), Value::List(vec![Value::Text("stray".into()), bad]));
    let mut project = s.document().clone();
    Change::SetNode { canvas: CANVAS, node: damaged }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let listed = s.dispatch("layers.list", &json!({"canvas": CANVAS, "id": EDITOR})).unwrap();
    assert_eq!(listed["fonts"], json!([]));
    let params = font(&mut s, &lito());
    s.dispatch("layers.font_add", &params).unwrap();
    let Some(Value::List(stored)) = node(&s).config.get(nodecraft_nodes::layers::FONTS_CONFIG).cloned() else { panic!("fonts stored") };
    assert_eq!(stored.len(), 1, "only the readable font is kept");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_asset_larger_than_a_font_may_be_is_refused() {
    use nodecraft_format::{AssetStore, NativeStorage, Snapshot};
    use std::io::Read;
    // Only stored assets can be larger than a font; resident imports stop at 16 MiB.
    let dir = tempfile::tempdir().unwrap();
    let storage = NativeStorage::new(dir.path().join("recovery")).unwrap();
    let project = session().document().clone();
    let mut s = Session::with_storage(Snapshot { project, assets: AssetStore::default(), epoch: 0 }, Box::new(storage)).unwrap();
    let mut big = std::io::repeat(0).take(layers::MAX_FONT_BYTES as u64 + 1);
    let hash = s.import_asset_reader(&mut big, &Default::default(), &Default::default()).unwrap();
    let params = json!({"canvas": CANVAS, "id": EDITOR, "font": {"hash": hash, "mime": "font/ttf"}});
    let refused = s.dispatch("layers.font_add", &params).unwrap_err();
    assert!(refused.message.contains("larger than 16 MiB"), "{refused:?}");
}
