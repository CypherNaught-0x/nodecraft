use super::*;
use crate::Session;
use nodecraft_types::{CanvasId, ProjectId};

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const A: NodeId = NodeId::from_bytes([3; 16]);
const B: NodeId = NodeId::from_bytes([4; 16]);
const TEXT: NodeId = NodeId::from_bytes([5; 16]);
const HERO: NodeId = NodeId::from_bytes([6; 16]);
const CARD: NodeId = NodeId::from_bytes([7; 16]);
const USER: NodeId = NodeId::from_bytes([8; 16]);

fn edge(n: u8) -> EdgeId {
    EdgeId::from_bytes([n; 16])
}

fn session() -> Session {
    session_with(Vec::new())
}

fn session_with(nodes: Vec<Node>) -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    for node in nodes {
        Change::AddNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
    }
    let mut s = Session::new(project).unwrap();
    for (id, kind) in [(A, "source.image"), (B, "source.image"), (TEXT, "source.text")] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 0}})).unwrap();
    }
    s
}

fn create_params(name: &str, from: &[NodeId]) -> Json {
    let from: Vec<Json> = from.iter().map(|id| json!({"node": id, "port": "out"})).collect();
    let edges: Vec<EdgeId> = (0..from.len()).map(|n| edge(10 + u8::try_from(n).unwrap())).collect();
    json!({"canvas": CANVAS, "id": HERO, "name": name, "category": "character", "guidelines": "red scarf", "from": from, "edges": edges, "pos": {"x": 400, "y": 0}})
}

fn created() -> Session {
    let mut s = session();
    assert_eq!(s.dispatch("element.create", &create_params("Hero", &[A, B])).unwrap(), json!({"id": HERO}));
    s
}

fn node(s: &Session, id: NodeId) -> Node {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&id).unwrap().clone()
}

fn canvas(s: &Session) -> &Canvas {
    s.document().canvases().get(&CANVAS).unwrap()
}

#[test]
fn create_wires_the_images_in_order_as_one_undo_step() {
    let mut s = created();
    let hero = node(&s, HERO);
    assert_eq!(element::name(&hero), Some("Hero"));
    assert_eq!(hero.params.get(CATEGORY), Some(&Value::Text("character".into())));
    let froms: Vec<NodeId> = image_wires(canvas(&s), HERO).iter().map(|e| e.from.node).collect();
    assert_eq!(froms, vec![A, B]);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!canvas(&s).nodes().contains_key(&HERO), "one undo step");
    assert!(canvas(&s).edges().is_empty());
}

#[test]
fn create_refuses_bad_names_shared_names_text_and_more_than_eight() {
    let mut s = created();
    let mut again = create_params("HERO", &[A]);
    again["id"] = json!(NodeId::from_bytes([9; 16]));
    assert_eq!(s.dispatch("element.create", &again).unwrap_err().kind, ErrorKind::Conflict);
    let mut s = session();
    for name in ["", "two words", "#hero", &"x".repeat(41)] {
        let error = s.dispatch("element.create", &create_params(name, &[A])).unwrap_err();
        assert!(!error.message.contains("two words"), "errors don't echo input");
    }
    assert!(s.dispatch("element.create", &create_params("hero", &[TEXT])).unwrap_err().message.contains("made of images"));
    assert!(s.dispatch("element.create", &create_params("hero", &[A; 9])).is_err());
    let mut mismatched = create_params("hero", &[A, B]);
    mismatched["edges"] = json!([edge(1)]);
    assert!(s.dispatch("element.create", &mismatched).is_err());
    assert!(canvas(&s).nodes().len() == 3, "nothing was added");
    // An Element can start empty and get images wired later.
    assert!(s.dispatch("element.create", &create_params("hero", &[])).is_ok());
}

fn sheet_params(edges: usize) -> Json {
    let edges: Vec<EdgeId> = (0..edges).map(|n| edge(30 + u8::try_from(n).unwrap())).collect();
    json!({"canvas": CANVAS, "element": HERO, "id": CARD, "edges": edges, "pos": {"x": 800, "y": 0}})
}

#[test]
fn a_sheet_card_takes_the_images_and_feeds_the_element() {
    let mut s = created();
    let models = sheet_models(&GenerationCatalog::builtin().unwrap(), 2);
    assert!(!models.is_empty(), "the builtin catalog has a model taking two images");
    assert_eq!(s.dispatch("element.sheet", &sheet_params(3)).unwrap(), json!({"id": CARD}));
    let card = node(&s, CARD);
    assert_eq!(card.kind, KIND);
    let Some(Value::Text(prompt)) = card.params.get(PROMPT) else { panic!("no prompt") };
    assert!(prompt.contains("turnaround") && prompt.ends_with("Keep: red scarf"), "{prompt}");
    let c = canvas(&s);
    let into_card: Vec<NodeId> = {
        let mut wires: Vec<&Edge> = c.edges().values().filter(|e| e.to.node == CARD).collect();
        wires.sort_by_key(|e| e.order);
        wires.iter().map(|e| e.from.node).collect()
    };
    assert_eq!(into_card, vec![A, B]);
    assert!(c.edges().values().any(|e| e.from.node == CARD && e.to.node == HERO && e.to.port == SHEET));
    // One sheet per Element; regenerating is running the card again.
    let mut second = sheet_params(3);
    second["id"] = json!(NodeId::from_bytes([20; 16]));
    assert_eq!(s.dispatch("element.sheet", &second).unwrap_err().kind, ErrorKind::Conflict);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!canvas(&s).nodes().contains_key(&CARD), "one undo step");
}

#[test]
fn a_sheet_needs_images_the_right_edge_count_and_a_fitting_model() {
    let mut s = created();
    assert!(s.dispatch("element.sheet", &sheet_params(2)).unwrap_err().message.contains("edge id"));
    let mut bad_model = sheet_params(3);
    bad_model["model"] = json!("nope");
    assert_eq!(s.dispatch("element.sheet", &bad_model).unwrap_err().kind, ErrorKind::Unsupported);
    let mut empty = session();
    empty.dispatch("element.create", &create_params("hero", &[])).unwrap();
    assert!(empty.dispatch("element.sheet", &sheet_params(1)).is_err());
    let mut not_element = sheet_params(3);
    not_element["element"] = json!(A);
    assert!(s.dispatch("element.sheet", &not_element).is_err());
}

#[test]
fn list_reports_mentions_and_shared_names() {
    let catalog = GenerationCatalog::builtin().unwrap();
    let model = sheet_models(&catalog, 1).into_iter().next().unwrap().0;
    let mut supplied = ParamValues::new();
    supplied.insert(MODEL.into(), Value::Text(model));
    supplied.insert(PROMPT.into(), Value::Text("#hero on a #ff0000 bike".into()));
    let user = catalog.registry().unwrap().get(KIND).unwrap().instantiate_instance(USER, BTreeMap::new(), &supplied).unwrap();
    let mut s = session_with(vec![user]);
    s.dispatch("element.create", &create_params("Hero", &[A, B])).unwrap();
    let listed = s.dispatch("element.list", &json!({"canvas": CANVAS})).unwrap();
    let hero = listed["elements"].as_array().unwrap().first().unwrap().clone();
    assert_eq!(hero["name"], "Hero");
    assert_eq!(hero["images"], 2);
    assert_eq!(hero["mentioned_by"], json!([USER]));
    assert_eq!(hero["shared_name"], false);
    let mut twin = create_params("hero", &[]);
    twin["id"] = json!(NodeId::from_bytes([9; 16]));
    assert!(s.dispatch("element.create", &twin).is_err(), "a shared name is refused on create");
}

#[test]
fn guidelines_cant_make_the_sheet_mention_elements() {
    let mut s = session();
    let mut params = create_params("Hero", &[A]);
    params["guidelines"] = json!("#hero's #ff0000 scarf");
    s.dispatch("element.create", &params).unwrap();
    let prompt = sheet_prompt(&node(&s, HERO));
    assert!(!prompt.contains('#') && prompt.ends_with("Keep: hero's ff0000 scarf"), "{prompt}");
}
