use super::MAX_TECHNIQUE_JOBS;
use crate::{Session, commands::Registry};
use nodecraft_graph::{Canvas, CanvasKind, Change, Changeset, Edge, Endpoint, Generation, ItemKey, Node, Port, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, GenerationId, ItemId, Modality, NodeId, PortType, ProjectId, TechniqueId, Value};
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const V1: CanvasId = CanvasId::from_bytes([0x71; 16]);
const V2: CanvasId = CanvasId::from_bytes([0x72; 16]);
const T: TechniqueId = TechniqueId::from_bytes([0x7e; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };

/// A free text step: text in, text out, with a `style` parameter.
struct Write(NodeDescriptor);
impl NodeKind for Write {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("write").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text("written".into()) }])
    }
}

fn kinds() -> nodecraft_nodes::Registry {
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let style = ParamSpec {
        name: "style".into(),
        label: "Style".into(),
        description: None,
        control: ParamControl::Text { multiline: false, max_bytes: 64 },
        required: false,
        default: None,
        visible_if: None,
    };
    kinds
        .register(Box::new(Write(NodeDescriptor {
            id: "test.write".into(),
            label: "Write".into(),
            category: Category::Action,
            inputs: vec![Port { name: "in".into(), port_type: TEXT }],
            outputs: vec![Port { name: "out".into(), port_type: TEXT }],
            params: ParamSchema { params: vec![style] },
            quick_add: None,
        })))
        .unwrap();
    kinds
}

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn text(kinds: &nodecraft_nodes::Registry, n: u8, value: &str) -> Node {
    kinds.get("source.text").unwrap().descriptor().instantiate(id(n), &[("text".to_owned(), Value::Text(value.into()))].into_iter().collect()).unwrap()
}

fn write(kinds: &nodecraft_nodes::Registry, n: u8) -> Node {
    let mut node = kinds.get("test.write").unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap();
    node.title = format!("Write {n}");
    node
}

fn batch(kinds: &nodecraft_nodes::Registry, n: u8, items: &str) -> Node {
    let mut node = kinds.get(nodecraft_nodes::batch::ID).unwrap().instantiate_instance(id(n), Default::default(), &Default::default()).unwrap();
    node.params.insert("text".into(), Value::Text(items.into()));
    node
}

fn wire(e: u8, from: u8, port: &str, to: u8) -> Change {
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([e; 16]),
            from: Endpoint { node: id(from), port: port.into() },
            to: Endpoint { node: id(to), port: "in".into() },
            order: 0,
        },
    }
}

/// Text 1 → write 2 → write 3; text 5 on its own; batch 6 (1000 items) → write 7 → write 8.
fn session() -> Session {
    let kinds = kinds();
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Techniques").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let items: Vec<String> = (0..1000).map(|n| format!("item {n}")).collect();
    Changeset::new(vec![
        Change::AddNode { canvas: C, node: text(&kinds, 1, "a fox") },
        Change::AddNode { canvas: C, node: write(&kinds, 2) },
        Change::AddNode { canvas: C, node: write(&kinds, 3) },
        Change::AddNode { canvas: C, node: text(&kinds, 5, "unused") },
        Change::AddNode { canvas: C, node: batch(&kinds, 6, &items.join("\n")) },
        Change::AddNode { canvas: C, node: write(&kinds, 7) },
        Change::AddNode { canvas: C, node: write(&kinds, 8) },
        wire(10, 1, "out", 2),
        wire(11, 2, "out", 3),
        wire(12, 6, "item", 7),
        wire(13, 7, "out", 8),
    ])
    .apply(&mut project)
    .unwrap();
    Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap()
}

/// The fixture with `extra` changes applied on top.
fn session_with(extra: Vec<Change>) -> Session {
    let mut s = session();
    let mut project = s.document().clone();
    Changeset::new(extra).apply(&mut project).unwrap();
    s = Session::with_registries(project, Registry::builtins().unwrap(), kinds()).unwrap();
    s
}

fn ports(nodes: &[u8]) -> Json {
    nodes.iter().map(|n| json!({"node": id(*n)})).collect()
}

fn check(s: &mut Session, inputs: &[u8], outputs: &[u8]) -> Json {
    s.dispatch("technique.check", &json!({"canvas": C, "inputs": ports(inputs), "outputs": ports(outputs)})).unwrap()
}

fn publish_params(published: CanvasId, seed: u8) -> Json {
    json!({"canvas": C, "published": published, "seed": id(seed), "technique": T, "name": "Caption", "category": "Text", "tags": ["words"],
        "inputs": [{"node": id(1), "name": "Subject", "description": "What to write about"}], "outputs": [{"node": id(3)}],
        "params": [{"node": id(2), "param": "style"}]})
}

fn problems(checked: &Json) -> Vec<String> {
    serde_json::from_value(checked["problems"].clone()).unwrap()
}

#[test]
fn a_check_traces_the_outputs_back_and_counts_one_run() {
    let mut s = session();
    let revision = s.revision();
    let checked = check(&mut s, &[1], &[3]);
    assert_eq!(checked["ok"], json!(true), "{checked}");
    let nodes: BTreeSet<NodeId> = serde_json::from_value(checked["nodes"].clone()).unwrap();
    assert_eq!(nodes, [1, 2, 3].map(id).into_iter().collect(), "the lone text and the batch chain aren't part of it");
    assert_eq!(checked["jobs"], json!(2), "two write steps; the source isn't a job");
    assert_eq!(checked["inputs"][0]["port"], json!("out"));
    assert_eq!(checked["outputs"][0]["name"], json!("Write 3"), "a port is named after its node's title");
    assert_eq!(s.revision(), revision, "checking changes nothing");
    // Without inputs the text is part of the technique as it is.
    assert_eq!(check(&mut s, &[], &[3])["ok"], json!(true));
}

#[test]
fn a_check_reports_what_keeps_a_selection_from_publishing() {
    let mut s = session();
    assert_eq!(problems(&check(&mut s, &[], &[])), vec!["Pick at least one output.".to_owned()]);
    assert!(problems(&check(&mut s, &[2], &[3]))[0].contains("has incoming wires"));
    assert!(problems(&check(&mut s, &[5], &[3]))[0].contains("doesn't feed any output"));
    assert!(problems(&check(&mut s, &[1], &[1]))[0].contains("both an input and an output"));
    // A selection naming what isn't there is a problem too; check and publish refuse the same.
    let missing = s.dispatch("technique.check", &json!({"canvas": C, "outputs": [{"node": id(99)}]})).unwrap();
    assert!(problems(&missing)[0].contains("is not on this canvas"), "{missing}");
    let port = s.dispatch("technique.check", &json!({"canvas": C, "outputs": [{"node": id(3), "port": "nope"}]})).unwrap();
    assert!(problems(&port)[0].contains("no such output"), "{port}");
    let twice = check(&mut s, &[], &[3, 3]);
    assert!(problems(&twice)[0].contains("Two ports are called"), "{twice}");
    let blank = s.dispatch("technique.check", &json!({"canvas": C, "name": " ", "outputs": ports(&[3])})).unwrap();
    assert_eq!(problems(&blank), vec!["Give the technique a name.".to_owned()]);
    for outputs in [json!([{"node": id(99)}]), json!([{"node": id(3), "port": "nope"}]), ports(&[3, 3])] {
        let mut publish = publish_params(V1, 40);
        (publish["inputs"], publish["params"], publish["outputs"]) = (json!([]), json!([]), outputs);
        assert_eq!(s.dispatch("technique.publish", &publish).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    let many = s.dispatch("technique.check", &json!({"canvas": C, "outputs": ports(&[3; 65])}));
    assert_eq!(many.unwrap_err().kind, ErrorKind::Overflow);
}

#[test]
fn a_batch_that_fans_out_past_the_job_limit_is_refused() {
    let mut s = session();
    // 1000 items through one step fit; through two they make 2000 jobs.
    let one = check(&mut s, &[], &[7]);
    assert_eq!(one["jobs"], json!(MAX_TECHNIQUE_JOBS), "{one}");
    let two = check(&mut s, &[], &[8]);
    assert_eq!(two["ok"], json!(false));
    assert!(problems(&two)[0].contains("more than 1000 jobs"), "{two}");
    assert!(
        s.dispatch("technique.publish", &json!({"canvas": C, "published": V1, "seed": id(40), "technique": T, "name": "Fan", "outputs": ports(&[8])})).is_err()
    );
}

#[test]
fn publishing_freezes_a_copy_and_republishing_adds_the_next_version() {
    let mut s = session();
    let made = s.dispatch("technique.publish", &publish_params(V1, 40)).unwrap();
    assert_eq!(made["version"], json!(1));
    assert_eq!(made["nodes"], json!(3));
    let canvas = s.document().canvases().get(&V1).unwrap().clone();
    assert_eq!(canvas.kind(), CanvasKind::Technique);
    assert_eq!(canvas.title(), "Caption v1");
    assert_eq!(canvas.edges().len(), 2);
    assert!(canvas.nodes().keys().all(|copy| ![1, 2, 3].map(id).contains(copy)), "copies get new ids");
    let info = canvas.technique_info().unwrap();
    assert_eq!((info.name.as_str(), info.inputs[0].name.as_str(), info.params[0].name.as_str()), ("Caption", "Subject", "Style"));
    assert_eq!(info.source, Some(C));
    assert!(canvas.nodes().contains_key(&info.inputs[0].node) && canvas.nodes().contains_key(&info.params[0].node));

    // Editing the source leaves the published version as it was.
    s.dispatch("node.param_set", &json!({"canvas": C, "id": id(1), "name": "text", "value": {"type": "text", "value": "an owl"}})).unwrap();
    assert_eq!(s.document().canvases().get(&V1).unwrap(), &canvas);
    // The published version itself can't be edited, or run in place.
    let copy = info.inputs[0].node;
    let edit = s.dispatch("node.param_set", &json!({"canvas": V1, "id": copy, "name": "text", "value": {"type": "text", "value": "x"}}));
    assert_eq!(edit.unwrap_err().kind, ErrorKind::Conflict);
    let run = s.dispatch("run.quote", &json!({"canvas": V1, "mode": "selected", "nodes": [copy]}));
    assert!(run.is_err());
    assert_eq!(s.enabled("run.quote", &json!({"canvas": V1, "mode": "selected", "nodes": [copy]})).unwrap_err().kind, ErrorKind::Unsupported);

    let again = s.dispatch("technique.publish", &publish_params(V2, 41)).unwrap();
    assert_eq!(again["version"], json!(2));
    let listed = s.dispatch("technique.list", &json!({})).unwrap();
    let versions: Vec<u64> = listed["techniques"].as_array().unwrap().iter().map(|t| t["version"].as_u64().unwrap()).collect();
    assert_eq!(versions, vec![1, 2]);
    assert_eq!(listed["techniques"][0]["inputs"], json!(["Subject"]));
    // Publishing is one undo step.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!s.document().canvases().contains_key(&V2));
    let shown = s.dispatch("technique.show", &json!({"canvas": V1})).unwrap();
    assert_eq!(shown["version"], json!(1));
    assert_eq!(shown["nodes"].as_array().unwrap().len(), 3);
    let removed = s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    assert_eq!(removed["version"], json!(1));
    assert!(s.dispatch("technique.list", &json!({})).unwrap()["techniques"].as_array().unwrap().is_empty());
    // What's left keeps the number taken: it can't be removed twice, and the next version is 2.
    let stub = s.dispatch("technique.show", &json!({"canvas": V1})).unwrap();
    assert_eq!((stub["retired"].clone(), stub["version"].clone(), stub["nodes"].clone()), (json!(true), json!(1), json!([])));
    assert_eq!(s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(check(&mut s, &[1], &[3])["version"], json!(1), "a check without a technique id starts a new one");
    let checked = s.dispatch("technique.check", &json!({"canvas": C, "technique": T, "outputs": ports(&[3])})).unwrap();
    assert_eq!(checked["version"], json!(2));
    assert_eq!(s.dispatch("technique.publish", &publish_params(V2, 41)).unwrap()["version"], json!(2), "a removed number isn't reused");
    assert_eq!(s.dispatch("technique.remove", &json!({"canvas": C})).unwrap_err().kind, ErrorKind::InvalidInput, "only published versions are removed");
}

#[test]
fn publish_refuses_bad_parameters_and_taken_ids() {
    let mut s = session();
    let mut bad = publish_params(V1, 40);
    bad["params"] = json!([{"node": id(2), "param": "nope"}]);
    assert_eq!(s.dispatch("technique.publish", &bad).unwrap_err().kind, ErrorKind::InvalidInput);
    bad["params"] = json!([{"node": id(7), "param": "style"}]);
    assert_eq!(s.dispatch("technique.publish", &bad).unwrap_err().kind, ErrorKind::InvalidInput, "node 7 isn't traced from the output");
    let mut taken = publish_params(C, 40);
    taken["published"] = json!(C);
    assert_eq!(s.dispatch("technique.publish", &taken).unwrap_err().kind, ErrorKind::Conflict);
    let mut blank = publish_params(V1, 40);
    blank["name"] = json!("  ");
    assert!(s.dispatch("technique.publish", &blank).is_err());
    // A seed reused for a second version would repeat ids; the whole publish is refused.
    s.dispatch("technique.publish", &publish_params(V1, 40)).unwrap();
    assert!(s.dispatch("technique.publish", &publish_params(V2, 40)).is_err());
    assert!(!s.document().canvases().contains_key(&V2));
    // A technique isn't published from another technique or a sheet.
    let from = json!({"canvas": V1, "published": V2, "seed": id(42), "technique": T, "name": "Again", "outputs": []});
    assert_eq!(s.dispatch("technique.publish", &from).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn a_check_reports_what_the_planner_blocks() {
    let kinds = kinds();
    let mut gone = write(&kinds, 20);
    gone.kind = "test.gone".into();
    let mut s = session_with(vec![Change::AddNode { canvas: C, node: gone }, Change::AddNode { canvas: C, node: write(&kinds, 21) }, wire(14, 20, "out", 21)]);
    let checked = check(&mut s, &[], &[21]);
    assert_eq!(checked["ok"], json!(false));
    assert!(problems(&checked)[0].contains("isn't available"), "{checked}");
    assert_eq!(checked["jobs"], Json::Null);
}

#[test]
fn duplicate_parameter_names_are_refused_alike_by_check_and_publish() {
    let mut s = session();
    let params = json!([{"node": id(2), "param": "style"}, {"node": id(3), "param": "style"}]);
    let checked = s.dispatch("technique.check", &json!({"canvas": C, "outputs": ports(&[3]), "params": params})).unwrap();
    assert_eq!(checked["ok"], json!(false), "both are labelled Style");
    let mut publish = publish_params(V1, 40);
    publish["params"] = params;
    assert_eq!(s.dispatch("technique.publish", &publish).unwrap_err().kind, ErrorKind::InvalidInput);
    // Named apart, they're fine.
    let named = json!([{"node": id(2), "param": "style"}, {"node": id(3), "param": "style", "name": "Final style"}]);
    assert_eq!(s.dispatch("technique.check", &json!({"canvas": C, "outputs": ports(&[3]), "params": named})).unwrap()["ok"], json!(true));
}

#[test]
fn removing_a_version_undoes_and_redoes_whole() {
    let mut s = session();
    s.dispatch("technique.publish", &publish_params(V1, 40)).unwrap();
    let published = s.document().canvases().get(&V1).unwrap().clone();
    s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    assert!(s.document().canvases().get(&V1).unwrap().nodes().is_empty());
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert_eq!(s.document().canvases().get(&V1).unwrap(), &published);
    s.dispatch("edit.redo", &Json::Null).unwrap();
    assert_eq!(s.dispatch("technique.show", &json!({"canvas": V1})).unwrap()["retired"], json!(true));
    // Undoing past the publish takes the stub away too.
    s.dispatch("edit.undo", &Json::Null).unwrap();
    s.dispatch("edit.undo", &Json::Null).unwrap();
    assert!(!s.document().canvases().contains_key(&V1));
    s.dispatch("edit.redo", &Json::Null).unwrap();
    assert_eq!(s.document().canvases().get(&V1).unwrap(), &published);
}

#[test]
fn published_and_retired_versions_round_trip_through_a_project_file() {
    let mut s = session();
    s.dispatch("technique.publish", &publish_params(V1, 40)).unwrap();
    s.dispatch("technique.publish", &publish_params(V2, 41)).unwrap();
    s.dispatch("technique.remove", &json!({"canvas": V1})).unwrap();
    let snapshot = nodecraft_format::Snapshot { project: s.document().clone(), assets: Default::default(), epoch: 0 };
    let decoded = nodecraft_format::decode(&nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    assert_eq!(decoded.project, snapshot.project);
    let mut reopened = Session::with_registries(decoded.project, Registry::builtins().unwrap(), kinds()).unwrap();
    let third = publish_params(CanvasId::from_bytes([0x73; 16]), 42);
    assert_eq!(reopened.dispatch("technique.publish", &third).unwrap()["version"], json!(3));
}

#[test]
fn an_example_a_batch_fanned_out_stays_behind() {
    let kinds = kinds();
    let mut plain = write(&kinds, 2);
    let mut fanned = write(&kinds, 7);
    plain.history = vec![Generation::new(GenerationId::from_bytes([0x21; 16]), vec![Value::Text("kept".into())])];
    plain.active = Some(GenerationId::from_bytes([0x21; 16]));
    let mut generation = Generation::new(GenerationId::from_bytes([0x22; 16]), vec![Value::List(vec![Value::Text("one item".into())])]);
    generation.items = vec![vec![ItemKey { batch: id(6), item: ItemId::from_bytes([0x23; 16]) }]];
    fanned.history = vec![generation];
    fanned.active = Some(GenerationId::from_bytes([0x22; 16]));
    let mut s = session_with(vec![
        Change::RemoveNode { canvas: C, id: id(2) },
        Change::RemoveNode { canvas: C, id: id(7) },
        Change::AddNode { canvas: C, node: plain },
        Change::AddNode { canvas: C, node: fanned },
        wire(10, 1, "out", 2),
        wire(11, 2, "out", 3),
        wire(12, 6, "item", 7),
        wire(13, 7, "out", 8),
    ]);
    s.dispatch("technique.publish", &publish_params(V1, 40)).unwrap();
    let examples: Vec<usize> = s.document().canvases().get(&V1).unwrap().nodes().values().map(|node| node.history.len()).collect();
    assert_eq!(examples.iter().sum::<usize>(), 1, "the plain result comes along");
    let fan = json!({"canvas": C, "published": V2, "seed": id(41), "technique": T, "name": "Fan", "outputs": ports(&[7])});
    s.dispatch("technique.publish", &fan).unwrap();
    let copied = s.document().canvases().get(&V2).unwrap();
    assert!(copied.nodes().values().all(|node| node.history.is_empty() && node.active.is_none()), "the fanned result names the source's items");
}
