use super::*;
use crate::Session;
use nodecraft_nodes::ModeId;
use nodecraft_nodes::generation::GenerationRole;
use nodecraft_types::ProjectId;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const IMAGE: NodeId = NodeId::from_bytes([3; 16]);
const NEW: NodeId = NodeId::from_bytes([4; 16]);
const EDITOR: NodeId = NodeId::from_bytes([5; 16]);
const CARD: NodeId = NodeId::from_bytes([6; 16]);

fn edge(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": IMAGE, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

fn node(s: &Session, id: NodeId) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&id).unwrap().clone()
}

fn wires(s: &Session) -> Vec<(NodeId, String, NodeId, String)> {
    s.document().canvases().get(&CANVAS).unwrap().edges().values().map(|e| (e.from.node, e.from.port.clone(), e.to.node, e.to.port.clone())).collect()
}

fn use_tool(tool: &str, from: NodeId, port: &str) -> Json {
    json!({"canvas": CANVAS, "tool": tool, "from": {"node": from, "port": port}, "id": NEW, "edges": [edge(1)], "pos": {"x": 400, "y": 0}})
}

fn listed(s: &mut Session, from: NodeId, port: &str) -> Vec<Json> {
    let out = s.dispatch("image.tools", &json!({"canvas": CANVAS, "from": {"node": from, "port": port}})).unwrap();
    out["tools"].as_array().unwrap().clone()
}

fn entry<'a>(tools: &'a [Json], id: &str) -> &'a Json {
    tools.iter().find(|t| t["tool"] == id).unwrap()
}

#[test]
fn lists_every_tool_in_menu_order_with_what_the_catalog_can_do() {
    let mut s = session();
    let tools = listed(&mut s, IMAGE, "out");
    let ids: Vec<&str> = tools.iter().map(|t| t["tool"].as_str().unwrap()).collect();
    assert_eq!(ids, ["inpaint", "expand", "remove_background", "upscale", "split_grid", "split_layers", "grade", "element", "to_3d"]);
    let upscale = entry(&tools, "upscale");
    assert_eq!(upscale["available"], true);
    assert_eq!(upscale["command"], "image.tool");
    assert!(upscale["models"].as_array().unwrap().contains(&json!("fal.esrgan")), "{upscale}");
    assert_eq!(entry(&tools, "to_3d")["available"], true);
    assert_eq!(entry(&tools, "grade")["available"], true);
    assert_eq!(entry(&tools, "expand")["command"], "mask.create");
    assert_eq!(entry(&tools, "element")["command"], "element.create");
    assert_eq!(entry(&tools, "split_grid")["command"], Json::Null);
    // The builtin catalog has no background removal or layer split yet: listed, with why not.
    for id in ["remove_background", "split_layers"] {
        let t = entry(&tools, id);
        assert_eq!(t["available"], false, "{t}");
        assert!(t["reason"].as_str().unwrap().starts_with("no model that can"), "{t}");
        assert_eq!(t["models"], json!([]));
    }
}

#[test]
fn single_image_tools_are_unavailable_on_a_card_that_makes_several() {
    let mut s = session();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": CARD, "kind": KIND, "pos": {"x": 0, "y": 400}})).unwrap();
    let out = node(&s, CARD).outputs.iter().find(|p| p.port_type.modality == Modality::Image && p.port_type.many).unwrap().name.clone();
    let tools = listed(&mut s, CARD, &out);
    for id in ["inpaint", "expand", "grade", "element"] {
        assert_eq!(entry(&tools, id)["available"], false, "{id}");
    }
    assert_eq!(entry(&tools, "upscale")["available"], true, "a card that hasn't made several images can feed a model tool");
    assert_eq!(entry(&tools, "split_grid")["available"], true, "Split into grid takes the first image a card shows");
    let err = s.dispatch("image.tool", &use_tool("grade", CARD, &out)).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Unsupported);
}

#[test]
fn grade_adds_a_color_grade_wired_to_the_image_as_one_step() {
    let mut s = session();
    assert_eq!(s.dispatch("image.tool", &use_tool("grade", IMAGE, "out")).unwrap(), json!({"id": NEW}));
    assert_eq!(node(&s, NEW).kind, GRADE);
    assert_eq!(node(&s, NEW).pos, Position { x: 400.0, y: 0.0 });
    assert_eq!(wires(&s), [(IMAGE, "out".to_owned(), NEW, "image".to_owned())]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document().canvases().get(&CANVAS).unwrap().nodes().len(), 1);
    let mut with_model = use_tool("grade", IMAGE, "out");
    with_model["model"] = json!("fal.esrgan");
    assert_eq!(s.dispatch("image.tool", &with_model).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn upscale_adds_a_card_with_a_model_that_can_and_the_image_in_its_role() {
    let mut s = session();
    let mut p = use_tool("upscale", IMAGE, "out");
    p["model"] = json!("fal.esrgan");
    assert_eq!(s.dispatch("image.tool", &p).unwrap(), json!({"id": NEW, "model": "fal.esrgan"}));
    let card = node(&s, NEW);
    assert_eq!(card.kind, KIND);
    assert_eq!(card.params.get(MODEL), Some(&Value::Text("fal.esrgan".into())));
    assert_eq!(wires(&s), [(IMAGE, "out".to_owned(), NEW, "image".to_owned())]);
    // A model that can't upscale is refused, and nothing changes.
    s.dispatch("edit.undo", &json!({})).unwrap();
    p["model"] = json!("fal.trellis2");
    assert_eq!(s.dispatch("image.tool", &p).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(s.document().canvases().get(&CANVAS).unwrap().nodes().len(), 1);
}

#[test]
fn open_in_3d_adds_a_3d_card() {
    let mut s = session();
    let out = s.dispatch("image.tool", &use_tool("to_3d", IMAGE, "out")).unwrap();
    let model = out["model"].as_str().unwrap().to_owned();
    let card = node(&s, NEW);
    assert_eq!(card.kind, MESH_KIND);
    assert_eq!(card.params.get(MODEL), Some(&Value::Text(model)));
    assert_eq!(wires(&s).len(), 1);
}

#[test]
fn bad_requests_are_refused() {
    let mut s = session();
    let cases = [
        // Not this command's tools.
        use_tool("inpaint", IMAGE, "out"),
        use_tool("split_grid", IMAGE, "out"),
        // Edge counts and the editor.
        {
            let mut p = use_tool("grade", IMAGE, "out");
            p["edges"] = json!([edge(1), edge(2)]);
            p
        },
        {
            let mut p = use_tool("grade", IMAGE, "out");
            p["editor"] = json!(EDITOR);
            p
        },
        {
            let mut p = use_tool("grade", IMAGE, "out");
            p["pos"] = json!({"x": 1e9, "y": 0});
            p
        },
    ];
    for p in cases {
        assert_eq!(s.dispatch("image.tool", &p).unwrap_err().kind, ErrorKind::InvalidInput, "{p}");
    }
    assert_eq!(s.dispatch("image.tool", &use_tool("grade", IMAGE, "nope")).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(s.dispatch("image.tool", &use_tool("grade", NEW, "out")).unwrap_err().kind, ErrorKind::NotFound);
    let mut layers = use_tool("split_layers", IMAGE, "out");
    layers["edges"] = json!([edge(1), edge(2)]);
    layers["editor"] = json!(EDITOR);
    assert_eq!(s.dispatch("image.tool", &layers).unwrap_err().kind, ErrorKind::Unsupported, "no model splits layers yet");
    assert_eq!(s.document().canvases().get(&CANVAS).unwrap().nodes().len(), 1);
}

#[test]
fn a_non_image_output_has_no_image_tools() {
    let mut s = session();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": CARD, "kind": "source.text", "pos": {"x": 0, "y": 400}})).unwrap();
    let out = node(&s, CARD).outputs.first().unwrap().name.clone();
    let err = s.dispatch("image.tools", &json!({"canvas": CANVAS, "from": {"node": CARD, "port": out}})).unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);
}

fn role(name: &str, modality: Modality, min: u16, max: u16) -> GenerationRole {
    GenerationRole { role: name.into(), modality, min, max, inline: false }
}

fn mode(inputs: Vec<GenerationRole>) -> GenerationMode {
    GenerationMode { id: ModeId::new("image-to-layers").unwrap(), inputs, params: None, audio: None, edit: None, document: None }
}

#[test]
fn a_mode_qualifies_when_the_image_is_all_it_needs() {
    assert_eq!(image_role(&mode(vec![role("image", Modality::Image, 1, 1)])), Some("image"));
    assert_eq!(
        image_role(&mode(vec![role("prompt", Modality::Text, 0, 1), role("source", Modality::Image, 1, 4)])),
        Some("source"),
        "an optional prompt is fine"
    );
    assert_eq!(image_role(&mode(vec![role("image", Modality::Image, 1, 1), role("prompt", Modality::Text, 1, 1)])), None, "a required prompt is not");
    assert_eq!(image_role(&mode(vec![role("images", Modality::Image, 4, 12)])), None, "nor several images");
    assert_eq!(image_role(&mode(vec![role("prompt", Modality::Text, 0, 1)])), None, "nor no image");
}

/// Models for the two tools the builtin catalog can't do yet.
const TOOLS: &str = r#"
[[model]]
id = "test.cutout"
provider = "test"
provider_model = "cutout-v1"
label = "Cutout"
description = "Removes backgrounds."
latency_s = 5
quality = 3
pricing = { type = "by_usage", currency = "USD" }
[[model.modes]]
id = "remove-background"
output = "image"
inputs = [{ role = "image", modality = "image", min = 1, max = 1 }]

[[model]]
id = "test.layers"
provider = "test"
provider_model = "layers-v1"
label = "Layers"
description = "Splits an image into layers."
latency_s = 5
quality = 3
pricing = { type = "by_usage", currency = "USD" }
[[model.modes]]
id = "image-to-layers"
output = "image"
inputs = [{ role = "source", modality = "image", min = 1, max = 1 }, { role = "prompt", modality = "text", min = 0, max = 1 }]
"#;

fn session_with_tools() -> Session {
    let mut catalog = nodecraft_catalog::Catalog::builtin().unwrap();
    catalog.merge(nodecraft_catalog::Catalog::from_toml(TOOLS).unwrap()).unwrap();
    let generation = GenerationCatalog::new(crate::run_cmds::RunCatalog::new(catalog).unwrap()).unwrap();
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, crate::commands::Registry::builtins().unwrap(), generation.registry().unwrap()).unwrap();
    generation.install(&mut s);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": IMAGE, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s
}

#[test]
fn split_into_layers_adds_the_card_and_a_layer_editor_fed_by_it_as_one_step() {
    let mut s = session_with_tools();
    let tools = listed(&mut s, IMAGE, "out");
    assert_eq!(entry(&tools, "split_layers")["models"], json!(["test.layers"]));
    let mut p = use_tool("split_layers", IMAGE, "out");
    p["edges"] = json!([edge(1), edge(2)]);
    p["editor"] = json!(EDITOR);
    assert_eq!(s.dispatch("image.tool", &p).unwrap(), json!({"id": NEW, "editor": EDITOR, "model": "test.layers"}));
    assert_eq!(node(&s, NEW).params.get(MODEL), Some(&Value::Text("test.layers".into())));
    let editor = node(&s, EDITOR);
    assert_eq!(editor.kind, layers::ID);
    assert_eq!(editor.pos, beside(Position { x: 400.0, y: 0.0 }, 1.0));
    let out = node(&s, NEW).outputs.first().unwrap().name.clone();
    let mut got = wires(&s);
    got.sort();
    let mut want = vec![(IMAGE, "out".to_owned(), NEW, "source".to_owned()), (NEW, out, EDITOR, "layers".to_owned())];
    want.sort();
    assert_eq!(got, want);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document().canvases().get(&CANVAS).unwrap().nodes().len(), 1, "one undo step");
    assert!(wires(&s).is_empty());
}

#[test]
fn remove_background_adds_a_card_with_the_cutout_model() {
    let mut s = session_with_tools();
    assert_eq!(s.dispatch("image.tool", &use_tool("remove_background", IMAGE, "out")).unwrap(), json!({"id": NEW, "model": "test.cutout"}));
    assert_eq!(node(&s, NEW).kind, KIND);
    assert_eq!(wires(&s), [(IMAGE, "out".to_owned(), NEW, "image".to_owned())]);
}

#[test]
fn model_tools_are_unavailable_once_a_card_made_several_images() {
    let s = session();
    let catalog = s.generation_catalog().unwrap();
    let mut card = node(&s, IMAGE);
    let asset = nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([9; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
    let port = Port { name: "out".into(), port_type: nodecraft_types::PortType { modality: Modality::Image, many: true } };
    card.kind = KIND.into();
    let made = |card: &mut Node, n: usize| {
        let id = nodecraft_types::GenerationId::from_bytes([n as u8; 16]);
        card.history = vec![nodecraft_graph::Generation::new(id, vec![Value::List(vec![Value::Media(asset.clone()); n])])];
        card.active = Some(id);
    };
    made(&mut card, 1);
    assert_eq!(unavailable(Some(catalog), Tool::Upscale, &card, &port), None);
    assert_eq!(unavailable(Some(catalog), Tool::SplitGrid, &card, &port), None);
    made(&mut card, 2);
    assert!(unavailable(Some(catalog), Tool::Upscale, &card, &port).unwrap().contains("several images"));
    assert!(unavailable(Some(catalog), Tool::To3d, &card, &port).is_some());
    assert_eq!(unavailable(Some(catalog), Tool::SplitGrid, &card, &port), None);
    assert!(unavailable(Some(catalog), Tool::Grade, &card, &port).is_some());
}
