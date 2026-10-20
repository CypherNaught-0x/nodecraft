use crate::{Session, commands::Registry};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Project};
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, NodeId, ProjectId, TechniqueId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const T: TechniqueId = TechniqueId::from_bytes([0x7e; 16]);
const PLACED: NodeId = NodeId::from_bytes([0x50; 16]);
const SEED: NodeId = NodeId::from_bytes([0x60; 16]);

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn node(kinds: &nodecraft_nodes::Registry, n: u8, kind: &str, params: &[(&str, &str)]) -> Node {
    let params = params.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).into()))).collect();
    kinds.get(kind).unwrap().instantiate_instance(id(n), Default::default(), &params).unwrap()
}

/// Text 1 ("a cat") → replace 2 (cat → dog), published as V1 with input "Brief" and Find
/// exposed, and placed as PLACED; text 5 and replace 6 sit on their own.
fn session() -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "App mode").unwrap();
    let wire = Edge {
        id: EdgeId::from_bytes([10; 16]),
        from: Endpoint { node: id(1), port: "out".into() },
        to: Endpoint { node: id(2), port: "text".into() },
        order: 0,
    };
    Changeset::new(vec![
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") },
        Change::AddNode { canvas: C, node: node(&kinds, 1, "source.text", &[("text", "a cat")]) },
        Change::AddNode { canvas: C, node: node(&kinds, 2, "text.replace", &[("find", "cat"), ("replacement", "dog")]) },
        Change::AddNode { canvas: C, node: node(&kinds, 5, "source.text", &[("text", "my cat")]) },
        Change::AddNode { canvas: C, node: node(&kinds, 6, "text.replace", &[]) },
        Change::Connect { canvas: C, edge: wire },
    ])
    .apply(&mut project)
    .unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    let publish = json!({"canvas": C, "published": V1, "seed": id(40), "technique": T, "name": "Swap",
        "inputs": [{"node": id(1), "name": "Brief"}], "outputs": [{"node": id(2), "name": "Swapped"}],
        "params": [{"node": id(2), "param": "find", "name": "Find"}]});
    s.dispatch("technique.publish", &publish).unwrap();
    s.dispatch("technique.place", &json!({"canvas": C, "version": V1, "id": PLACED, "pos": {"x": 100.0, "y": 50.0}})).unwrap();
    s
}

fn form(s: &mut Session) -> Json {
    s.dispatch("app.form", &json!({"canvas": C, "id": PLACED})).unwrap()
}

fn fill(s: &mut Session, values: Json) -> Result<Json, nodecraft_types::Error> {
    s.dispatch("app.input_set", &json!({"canvas": C, "id": PLACED, "port": "brief", "seed": SEED, "values": values}))
}

fn text(value: &str) -> Json {
    json!({"type": "text", "value": value})
}

#[test]
fn the_form_lists_the_techniques_inputs_parameters_and_outputs() {
    let mut s = session();
    let form = form(&mut s);
    assert_eq!((form["name"].clone(), form["version"].clone(), form["ready"].clone()), (json!("Swap"), json!(1), json!(true)));
    let input = &form["inputs"][0];
    assert_eq!((input["port"].clone(), input["name"].clone(), input["feed"].clone()), (json!("brief"), json!("Brief"), json!("example")));
    assert_eq!(input["example"], text("a cat"), "an unfed input shows the version's own value");
    assert_eq!(input["fillable"], json!(true));
    assert_eq!((form["params"][0]["spec"]["label"].clone(), form["params"][0]["value"].clone()), (json!("Find"), text("cat")));
    assert_eq!(form["outputs"][0]["name"], json!("Swapped"));
    assert_eq!(form["results"]["count"], json!(0));
    // Only a technique node has a form.
    assert_eq!(s.dispatch("app.form", &json!({"canvas": C, "id": id(5)})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.enabled("app.form", &json!({"canvas": C, "id": id(99)})).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn filling_an_input_wires_a_source_and_is_one_undo_step() {
    let mut s = session();
    let made = fill(&mut s, json!([text("a fox")])).unwrap();
    let source = made["sources"][0].clone();
    let canvas = s.document().canvases().get(&C).unwrap();
    let source_id: NodeId = serde_json::from_value(source.clone()).unwrap();
    let node = canvas.nodes().get(&source_id).unwrap();
    assert_eq!((node.kind.as_str(), node.title.as_str()), ("source.text", "Brief"));
    assert_eq!(node.pos.x, 100.0 + super::SOURCE_OFFSET_X, "it sits left of the technique node");
    assert!(canvas.edges().values().any(|edge| edge.from.node == source_id && edge.to.node == PLACED && edge.to.port == "brief"));
    let form = form(&mut s);
    assert_eq!((form["inputs"][0]["feed"].clone(), form["inputs"][0]["values"].clone()), (json!("form"), json!([text("a fox")])));

    // Filling again updates the same source in place.
    let again = fill(&mut s, json!([text("a wolf")])).unwrap();
    assert_eq!(again["sources"][0], source);
    assert_eq!(self::form(&mut s)["inputs"][0]["values"], json!([text("a wolf")]));

    // Each fill is one undo step; emptying it removes the source and the example is back.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(self::form(&mut s)["inputs"][0]["values"], json!([text("a fox")]));
    fill(&mut s, json!([])).unwrap();
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&source_id));
    assert_eq!(self::form(&mut s)["inputs"][0]["feed"], json!("example"));
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!s.document().canvases().get(&C).unwrap().nodes().contains_key(&source_id));
}

#[test]
fn a_wired_input_or_a_wrong_value_is_refused() {
    let mut s = session();
    // One value only: the input takes one.
    assert_eq!(fill(&mut s, json!([text("a"), text("b")])).unwrap_err().kind, ErrorKind::InvalidInput);
    // A text input can't take a number.
    assert_eq!(fill(&mut s, json!([{"type": "bool", "value": true}])).unwrap_err().kind, ErrorKind::InvalidInput);
    let unknown = json!({"canvas": C, "id": PLACED, "port": "nope", "seed": SEED, "values": []});
    assert_eq!(s.dispatch("app.input_set", &unknown).unwrap_err().kind, ErrorKind::NotFound);

    // A text source wired only to the input is the form's to fill: the form edits it.
    let connect = |s: &mut Session, e: u8, from: NodeId| {
        let edge = json!({"id": EdgeId::from_bytes([e; 16]), "from": {"node": from, "port": "out"}, "to": {"node": PLACED, "port": "brief"}, "order": 0});
        s.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    };
    connect(&mut s, 20, id(5));
    assert_eq!(form(&mut s)["inputs"][0]["sources"], json!([id(5)]));
    assert_eq!(fill(&mut s, json!([text("a fox")])).unwrap()["sources"], json!([id(5)]));
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("edit.undo", &json!({})).unwrap();

    // An input wired from anything else shows what feeds it and isn't filled from the form.
    let canvas = s.document().canvases().get(&C).unwrap();
    assert!(!canvas.edges().values().any(|edge| edge.to.node == PLACED), "undone");
    connect(&mut s, 21, id(2));
    let form = form(&mut s);
    assert_eq!((form["inputs"][0]["feed"].clone(), form["inputs"][0]["from"].clone()), (json!("wired"), json!([id(2)])));
    assert_eq!(fill(&mut s, json!([text("a fox")])).unwrap_err().kind, ErrorKind::Conflict);
    let enabled = s.enabled("app.input_set", &json!({"canvas": C, "id": PLACED, "port": "brief", "seed": SEED, "values": []}));
    assert_eq!(enabled.unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn a_source_wired_elsewhere_too_is_not_the_forms() {
    let mut s = session();
    fill(&mut s, json!([text("a fox")])).unwrap();
    let canvas = s.document().canvases().get(&C).unwrap();
    let source = canvas.edges().values().find(|edge| edge.to.node == PLACED).unwrap().from.node;
    // Wiring the form's source into something else too makes it part of the graph.
    let edge = json!({"id": EdgeId::from_bytes([21; 16]), "from": {"node": source, "port": "out"}, "to": {"node": id(6), "port": "text"}, "order": 0});
    s.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    assert_eq!(form(&mut s)["inputs"][0]["feed"], json!("wired"));
    assert_eq!(fill(&mut s, json!([])).unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn a_removed_version_blocks_the_form() {
    let mut s = session();
    s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    let form = form(&mut s);
    assert_eq!(form["ready"], json!(false));
    assert!(form["blocked"].as_str().unwrap().contains("removed"));
}

#[test]
fn an_empty_source_is_not_the_forms() {
    let mut s = session();
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut empty = node(&kinds, 7, "source.text", &[]);
    empty.params.remove("text");
    s.dispatch("canvas.paste", &json!({"canvas": C, "nodes": [empty]})).unwrap();
    let edge = json!({"id": EdgeId::from_bytes([22; 16]), "from": {"node": id(7), "port": "out"}, "to": {"node": PLACED, "port": "brief"}, "order": 0});
    s.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    // Values and sources stay in step: the empty node is shown as wired, and the form leaves it.
    let form = form(&mut s);
    assert_eq!((form["inputs"][0]["feed"].clone(), form["inputs"][0]["from"].clone()), (json!("wired"), json!([id(7)])));
    assert_eq!(fill(&mut s, json!([])).unwrap_err().kind, ErrorKind::Conflict);
    assert!(s.document().canvases().get(&C).unwrap().nodes().contains_key(&id(7)));
}
