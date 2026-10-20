use crate::library::{self, MemoryLibrary};
use crate::{Session, commands::Registry};
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Node, Project};
use nodecraft_nodes::technique;
use nodecraft_types::{CanvasId, EdgeId, ErrorKind, NodeId, ProjectId, TechniqueId, Value};
use serde_json::{Value as Json, json};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const V2: CanvasId = CanvasId::from_bytes([0x72; 16]);
const COPY: CanvasId = CanvasId::from_bytes([0x80; 16]);
const T: TechniqueId = TechniqueId::from_bytes([0x7e; 16]);
const PLACED: NodeId = NodeId::from_bytes([0x50; 16]);

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn node(kinds: &nodecraft_nodes::Registry, n: u8, kind: &str, params: &[(&str, &str)]) -> Node {
    let params = params.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).into()))).collect();
    kinds.get(kind).unwrap().instantiate_instance(id(n), Default::default(), &params).unwrap()
}

/// `node` with one result in use, as after a run.
fn ran(mut node: Node) -> Node {
    let generation =
        nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([node.id.as_bytes()[0]; 16]), vec![Value::Text("a dog".into())]);
    node.active = Some(generation.id);
    node.history.push(generation);
    node
}

/// A project with one empty canvas, its library in `store`.
fn empty(store: &MemoryLibrary) -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([8; 16]), "Elsewhere").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    library::install(&mut s, Box::new(store.clone())).unwrap();
    s
}

/// Text 1 ("a cat") → replace 2 (cat → dog, run once), published as V1 exposing Find, and as
/// V2; its library in `store`.
fn publishing(store: &MemoryLibrary) -> Session {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Techniques").unwrap();
    Changeset::new(vec![
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") },
        Change::AddNode { canvas: C, node: node(&kinds, 1, "source.text", &[("text", "a cat")]) },
        Change::AddNode { canvas: C, node: ran(node(&kinds, 2, "text.replace", &[("find", "cat"), ("replacement", "dog")])) },
        Change::Connect {
            canvas: C,
            edge: Edge {
                id: EdgeId::from_bytes([10; 16]),
                from: Endpoint { node: id(1), port: "out".into() },
                to: Endpoint { node: id(2), port: "text".into() },
                order: 0,
            },
        },
    ])
    .apply(&mut project)
    .unwrap();
    let mut s = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap();
    library::install(&mut s, Box::new(store.clone())).unwrap();
    for (published, seed) in [(V1, 40), (V2, 41)] {
        let publish = json!({"canvas": C, "published": published, "seed": id(seed), "technique": T, "name": "Swap",
            "inputs": [{"node": id(1), "name": "Brief"}], "outputs": [{"node": id(2), "name": "Swapped"}], "params": [{"node": id(2), "param": "find", "name": "Find"}]});
        s.dispatch("technique.publish", &publish).unwrap();
    }
    s
}

fn place(version: u32) -> Json {
    json!({"canvas": C, "technique": T, "version": version, "id": PLACED, "pos": {"x": 10.0, "y": 20.0}, "copy": COPY})
}

#[test]
fn a_version_added_in_one_project_is_placed_in_another() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    let added = a.dispatch("library.add", &json!({"canvas": V1})).unwrap();
    assert_eq!(
        (added["added"].clone(), added["technique"]["version"].clone(), added["technique"]["inputs"].clone()),
        (json!(true), json!(1), json!(["Brief"]))
    );
    // Adding it again finds it there.
    assert_eq!(a.dispatch("library.add", &json!({"canvas": V1})).unwrap()["added"], json!(false));
    // Without its example result or the canvas it came from.
    let kept = store.saved().unwrap();
    let kept = kept.get(T, 1).unwrap();
    assert!(kept.nodes().values().all(|node| node.history.is_empty() && node.active.is_none()));
    assert_eq!(kept.technique_info().unwrap().source, None);

    let mut b = empty(&store);
    let listed = b.dispatch("library.list", &json!({})).unwrap();
    assert_eq!(listed["techniques"].as_array().unwrap().len(), 1);
    assert_eq!(listed["techniques"][0]["name"], "Swap");
    let before = b.document().clone();
    let made = b.dispatch("library.place", &place(1)).unwrap();
    assert_eq!((made["canvas"].clone(), made["copied"].clone()), (json!(COPY), json!(true)));
    let copy = &b.document().canvases()[&COPY];
    let info = copy.technique_info().unwrap();
    assert_eq!((info.id, info.version), (T, 1));
    // Its nodes took new ids, and its ports and parameters follow them.
    assert!(copy.nodes().keys().all(|node| !kept.nodes().contains_key(node)));
    assert!(info.inputs.iter().chain(&info.outputs).all(|port| copy.nodes().contains_key(&port.node)));
    assert!(info.params.iter().all(|param| copy.nodes().contains_key(&param.node)));
    let node = &b.document().canvases()[&C].nodes()[&PLACED];
    assert_eq!((node.kind.as_str(), node.title.as_str()), (technique::ID, "Swap"));
    assert_eq!(node.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["brief"]);
    assert!(node.params.contains_key("find"), "{:?}", node.params);
    // It can be detached into the copy's nodes, so it runs what was published.
    let mut detached = b.dispatch("technique.detach", &json!({"canvas": C, "node": PLACED, "seed": id(60)})).unwrap();
    assert_eq!(detached["nodes"].as_array_mut().unwrap().len(), 2);
    b.dispatch("edit.undo", &json!({})).unwrap();
    // Placing it was one undo step.
    b.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(b.document(), &before);
}

#[test]
fn a_project_that_has_the_version_uses_its_own() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    a.dispatch("library.add", &json!({"canvas": V2})).unwrap();
    let made = a.dispatch("library.place", &place(2)).unwrap();
    assert_eq!((made["canvas"].clone(), made["copied"].clone()), (json!(V2), json!(false)));
    assert!(!a.document().canvases().contains_key(&COPY));
    // A removed version keeps its number, so it isn't brought back from the library.
    a.dispatch("node.delete", &json!({"canvas": C, "ids": [PLACED]})).ok();
    a.dispatch("technique.remove", &json!({"canvas": V2})).unwrap();
    let error =
        a.dispatch("library.place", &json!({"canvas": C, "technique": T, "version": 2, "id": id(51), "pos": {"x": 0.0, "y": 0.0}, "copy": COPY})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
}

#[test]
fn placing_refuses_what_it_cant_do() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    a.dispatch("library.add", &json!({"canvas": V1})).unwrap();
    let mut b = empty(&store);
    // Not in the library.
    assert_eq!(b.dispatch("library.place", &place(3)).unwrap_err().kind, ErrorKind::NotFound);
    // A taken canvas id.
    let mut taken = place(1);
    taken["copy"] = json!(C);
    assert_eq!(b.dispatch("library.place", &taken).unwrap_err().kind, ErrorKind::Conflict);
    // Off the canvas.
    let mut far = place(1);
    far["pos"] = json!({"x": 1.0e9, "y": 0.0});
    assert!(b.dispatch("library.place", &far).is_err());
    // Not a published version.
    assert_eq!(a.dispatch("library.add", &json!({"canvas": C})).unwrap_err().kind, ErrorKind::InvalidInput);
    // A session with no library says so.
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let mut bare = Session::with_registries(Project::new(ProjectId::from_bytes([7; 16]), "Bare").unwrap(), Registry::builtins().unwrap(), kinds).unwrap();
    assert_eq!(bare.dispatch("library.list", &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn removing_a_version_leaves_the_projects_that_placed_it() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    a.dispatch("library.add", &json!({"canvas": V1})).unwrap();
    let mut b = empty(&store);
    b.dispatch("library.place", &place(1)).unwrap();
    let removed = b.dispatch("library.remove", &json!({"technique": T, "version": 1})).unwrap();
    assert_eq!(removed["removed"]["version"], json!(1));
    assert!(store.saved().unwrap().get(T, 1).is_none());
    assert!(b.document().canvases().contains_key(&COPY));
    assert_eq!(b.dispatch("library.remove", &json!({"technique": T, "version": 1})).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn a_version_holding_media_stays_in_its_project() {
    let store = MemoryLibrary::default();
    let a = publishing(&store);
    let mut version = a.document().canvases()[&V1].clone();
    let info = version.technique_info().unwrap().clone();
    let mut nodes: Vec<Node> = version.nodes().values().cloned().collect();
    let asset = nodecraft_types::AssetRef { hash: nodecraft_types::Blake3::from_bytes([7; 32]), mime: "image/png".parse().unwrap(), meta: Default::default() };
    nodes[0].params.insert("text".into(), Value::List(vec![Value::Media(asset)]));
    version = Canvas::technique(V1, version.title(), info, nodes, version.edges().values().cloned().collect()).unwrap();
    assert_eq!(library::entry(&version).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn a_placed_version_keeps_its_inputs_examples_and_inlines_in_its_new_project() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    // The input's example, as after a run, with the settings it was made with.
    let mut input = a.document().canvases()[&V1].nodes().values().find(|node| node.kind == "source.text").unwrap().clone();
    let generation = nodecraft_graph::Generation::new(nodecraft_types::GenerationId::from_bytes([0x33; 16]), vec![Value::Text("a cat".into())]);
    input.active = Some(generation.id);
    input.history.push(generation);
    let mut version = a.document().canvases()[&V1].clone();
    let info = version.technique_info().unwrap().clone();
    let nodes = version.nodes().values().map(|node| if node.id == input.id { input.clone() } else { node.clone() }).collect();
    version = Canvas::technique(V1, version.title(), info, nodes, version.edges().values().cloned().collect()).unwrap();
    let kept = library::entry(&version).unwrap();
    let examples: Vec<_> = kept.nodes().values().filter(|node| !node.history.is_empty()).map(|node| node.id).collect();
    assert_eq!(examples, vec![input.id], "only the input keeps its example");
    assert!(kept.nodes()[&input.id].history.iter().all(|g| g.settings.is_empty()));
    assert_eq!(kept.nodes()[&input.id].active, Some(nodecraft_types::GenerationId::from_bytes([0x33; 16])));

    a.dispatch("library.add", &json!({"canvas": V1})).unwrap();
    let mut b = empty(&store);
    b.dispatch("library.list", &json!({})).unwrap();
    b.dispatch("library.place", &place(1)).unwrap();
    let document = b.document();
    let canvas = &document.canvases()[&C];
    let expanded = nodecraft_exec::technique::expand(document.canvases(), canvas, b.kinds()).unwrap();
    assert!(expanded.blockers.is_empty(), "{:?}", expanded.blockers);
    assert_eq!(expanded.inner.values().filter(|owner| **owner == PLACED).count(), 2, "both of the version's nodes inline");
}

#[test]
fn a_version_running_other_techniques_stays_in_its_project() {
    let store = MemoryLibrary::default();
    let mut a = publishing(&store);
    // V1 placed on the canvas, and the canvas published again as a technique that runs it.
    a.dispatch("technique.place", &json!({"canvas": C, "version": V1, "id": PLACED, "pos": {"x": 0.0, "y": 0.0}})).unwrap();
    let outer = CanvasId::from_bytes([0x73; 16]);
    let publish = json!({"canvas": C, "published": outer, "seed": id(42), "technique": TechniqueId::from_bytes([0x7f; 16]), "name": "Outer",
        "outputs": [{"node": PLACED, "name": "Swapped"}]});
    a.dispatch("technique.publish", &publish).unwrap();
    let error = a.dispatch("library.add", &json!({"canvas": outer})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported, "{}", error.message);
    assert!(store.saved().is_none_or(|library| library.versions().count() == 0));
}
