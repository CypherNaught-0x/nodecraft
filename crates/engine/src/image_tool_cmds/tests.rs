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
    for id in ["inpaint", "expand", "grade", "split_grid", "element"] {
        assert_eq!(entry(&tools, id)["available"], false, "{id}");
    }
    assert_eq!(entry(&tools, "upscale")["available"], true, "a model tool takes a list one by one");
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
    GenerationMode { id: ModeId::new("image-to-layers").unwrap(), inputs, params: None, audio: None, edit: None }
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
