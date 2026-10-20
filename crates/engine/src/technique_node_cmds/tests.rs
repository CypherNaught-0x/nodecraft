use crate::{Session, commands::Registry};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Project};
use nodecraft_nodes::technique;
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, NodeId, ProjectId, TechniqueId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const V2: CanvasId = CanvasId::from_bytes([0x72; 16]);
const OTHER: CanvasId = CanvasId::from_bytes([0x73; 16]);
const T: TechniqueId = TechniqueId::from_bytes([0x7e; 16]);
const PLACED: NodeId = NodeId::from_bytes([0x50; 16]);

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn node(kinds: &nodecraft_nodes::Registry, n: u8, kind: &str, params: &[(&str, &str)]) -> Node {
    let params = params.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).into()))).collect();
    kinds.get(kind).unwrap().instantiate_instance(id(n), Default::default(), &params).unwrap()
}

fn wire(e: u8, from: NodeId, out: &str, to: NodeId, port: &str) -> Change {
    Change::Connect {
        canvas: C,
        edge: Edge { id: EdgeId::from_bytes([e; 16]), from: Endpoint { node: from, port: out.into() }, to: Endpoint { node: to, port: port.into() }, order: 0 },
    }
}

/// Text 1 ("a cat") → replace 2 (cat → dog), published as V1 exposing Find and as V2 with its
/// output renamed and nothing exposed; then text 5 ("my cat") and replace 6 on their own.
fn session() -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Technique nodes").unwrap();
    Changeset::new(vec![
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") },
        Change::AddNode { canvas: C, node: node(&kinds, 1, "source.text", &[("text", "a cat")]) },
        Change::AddNode { canvas: C, node: node(&kinds, 2, "text.replace", &[("find", "cat"), ("replacement", "dog")]) },
        Change::AddNode { canvas: C, node: node(&kinds, 5, "source.text", &[("text", "my cat")]) },
        Change::AddNode { canvas: C, node: node(&kinds, 6, "text.replace", &[]) },
        wire(10, id(1), "out", id(2), "text"),
    ])
    .apply(&mut project)
    .unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    let publish = |published, seed: u8, output: &str, params: Json| {
        json!({"canvas": C, "published": published, "seed": id(seed), "technique": T, "name": "Swap",
            "inputs": [{"node": id(1), "name": "Brief"}], "outputs": [{"node": id(2), "name": output}], "params": params})
    };
    s.dispatch("technique.publish", &publish(V1, 40, "Swapped", json!([{"node": id(2), "param": "find", "name": "Find"}]))).unwrap();
    s.dispatch("technique.publish", &publish(V2, 41, "Result", json!([]))).unwrap();
    let other = json!({"canvas": C, "published": OTHER, "seed": id(42), "technique": TechniqueId::from_bytes([0x7f; 16]), "name": "Other", "outputs": [{"node": id(2)}]});
    s.dispatch("technique.publish", &other).unwrap();
    s
}

fn placed(s: &Session) -> Option<&Node> {
    s.document().canvases().get(&C).unwrap().nodes().get(&PLACED)
}

/// V1 placed, fed by text 5, feeding replace 6, its Find set to "my".
fn session_placed() -> Session {
    let mut s = session();
    s.dispatch("technique.place", &json!({"canvas": C, "version": V1, "id": PLACED, "pos": {"x": 100.0, "y": 50.0}})).unwrap();
    for (e, from, out, to, port) in [(20, id(5), "out", PLACED, "brief"), (21, PLACED, "swapped", id(6), "text")] {
        let edge = json!({"id": EdgeId::from_bytes([e; 16]), "from": {"node": from, "port": out}, "to": {"node": to, "port": port}, "order": 0});
        s.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": C, "id": PLACED, "name": "find", "value": {"type": "text", "value": "my"}})).unwrap();
    s
}

#[test]
fn a_placed_technique_takes_the_versions_ports_and_parameters() {
    let mut s = session();
    let made = s.dispatch("technique.place", &json!({"canvas": C, "version": V1, "id": PLACED, "pos": {"x": 100.0, "y": 50.0}})).unwrap();
    assert_eq!((made["technique"].clone(), made["version"].clone()), (json!(T), json!(1)));
    let node = placed(&s).unwrap();
    assert_eq!((node.kind.as_str(), node.title.as_str()), (technique::ID, "Swap"));
    assert_eq!(node.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["brief"]);
    assert_eq!(node.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["swapped"]);
    assert_eq!(node.params.get("find"), Some(&Value::Text("cat".into())), "a parameter defaults to the published value");
    assert_eq!(technique::interface(node).unwrap().canvas, V1);
    // Placing is one undo step; what isn't a published version can't be placed.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(placed(&s).is_none());
    let place = |version: CanvasId| json!({"canvas": C, "version": version, "id": PLACED, "pos": {"x": 0.0, "y": 0.0}});
    assert_eq!(s.dispatch("technique.place", &place(C)).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.enabled("technique.place", &place(C)).unwrap_err().kind, ErrorKind::InvalidInput);
    s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    assert_eq!(s.dispatch("technique.place", &place(V1)).unwrap_err().kind, ErrorKind::NotFound, "a removed version can't be placed");
    let far = json!({"canvas": C, "version": V2, "id": PLACED, "pos": {"x": f32::MAX, "y": 0.0}});
    assert_eq!(s.dispatch("technique.place", &far).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn upgrading_keeps_what_still_fits_and_takes_off_the_rest() {
    let mut s = session_placed();
    let upgraded = s.dispatch("technique.upgrade", &json!({"canvas": C, "node": PLACED, "version": V2})).unwrap();
    assert_eq!(upgraded["version"], json!(2));
    assert_eq!(upgraded["dropped_params"], json!(["find"]), "V2 exposes no parameters");
    assert_eq!(upgraded["disconnected"], json!(1), "the output was renamed; the input still fits");
    let node = placed(&s).unwrap();
    assert!(node.params.is_empty());
    assert_eq!(node.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["result"]);
    let canvas = s.document().canvases().get(&C).unwrap();
    assert!(canvas.edges().contains_key(&EdgeId::from_bytes([20; 16])) && !canvas.edges().contains_key(&EdgeId::from_bytes([21; 16])));
    // One undo step back to V1, wires and value included; and back again.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(technique::interface(placed(&s).unwrap()).unwrap().version, 1);
    assert_eq!(placed(&s).unwrap().params.get("find"), Some(&Value::Text("my".into())));
    assert_eq!(s.document().canvases().get(&C).unwrap().edges().len(), 3);
    s.dispatch("technique.upgrade", &json!({"canvas": C, "node": PLACED, "version": V2})).unwrap();
    s.dispatch("technique.upgrade", &json!({"canvas": C, "node": PLACED, "version": V1})).unwrap();
    assert_eq!(placed(&s).unwrap().params.get("find"), Some(&Value::Text("cat".into())), "a parameter that comes back takes its default");
    // Only versions of the same technique, and only technique nodes.
    assert_eq!(s.dispatch("technique.upgrade", &json!({"canvas": C, "node": PLACED, "version": OTHER})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("technique.upgrade", &json!({"canvas": C, "node": id(6), "version": V2})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(s.dispatch("technique.upgrade", &json!({"canvas": C, "node": id(99), "version": V2})).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn detaching_puts_the_versions_nodes_in_its_place_wired_as_it_was() {
    let mut s = session_placed();
    let before = s.document().clone();
    let detached = s.dispatch("technique.detach", &json!({"canvas": C, "node": PLACED, "seed": id(60)})).unwrap();
    let nodes: Vec<NodeId> = serde_json::from_value(detached["nodes"].clone()).unwrap();
    assert_eq!(nodes.len(), 1, "the fed input node is replaced by what fed it");
    let canvas = s.document().canvases().get(&C).unwrap();
    assert!(placed(&s).is_none());
    let copy = canvas.nodes().get(&nodes[0]).unwrap();
    assert_eq!(copy.kind, "text.replace");
    assert_eq!(copy.params.get("find"), Some(&Value::Text("my".into())), "the node's value carries over");
    assert_eq!(copy.params.get("replacement"), Some(&Value::Text("dog".into())));
    assert_eq!((copy.pos.x, copy.pos.y), (100.0, 50.0), "laid out where the technique node was");
    let wired = |from: NodeId, to: NodeId| canvas.edges().values().any(|edge| edge.from.node == from && edge.to.node == to);
    assert!(wired(id(5), copy.id) && wired(copy.id, id(6)), "{:?}", canvas.edges());
    assert_eq!(canvas.edges().len(), 3);
    // One undo step; a seed whose ids are taken is refused.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(s.document(), &before);
    s.dispatch("technique.detach", &json!({"canvas": C, "node": PLACED, "seed": id(60)})).unwrap();
    s.dispatch("technique.place", &json!({"canvas": C, "version": V1, "id": PLACED, "pos": {"x": 0.0, "y": 0.0}})).unwrap();
    assert_eq!(s.dispatch("technique.detach", &json!({"canvas": C, "node": PLACED, "seed": id(60)})).unwrap_err().kind, ErrorKind::Conflict);
}

#[test]
fn an_unfed_input_stays_and_a_removed_version_cant_be_detached() {
    let mut s = session();
    s.dispatch("technique.place", &json!({"canvas": C, "version": V1, "id": PLACED, "pos": {"x": 0.0, "y": 0.0}})).unwrap();
    let detached = s.dispatch("technique.detach", &json!({"canvas": C, "node": PLACED, "seed": id(60)})).unwrap();
    assert_eq!(detached["nodes"].as_array().unwrap().len(), 2, "nothing feeds the input, so its node stays with its example");
    s.dispatch("edit.undo", &json!({})).unwrap();
    s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    assert_eq!(s.dispatch("technique.detach", &json!({"canvas": C, "node": PLACED, "seed": id(60)})).unwrap_err().kind, ErrorKind::NotFound);
}
