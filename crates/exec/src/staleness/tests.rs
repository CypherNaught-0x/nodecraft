use super::*;
use nodecraft_graph::{Canvas, Change, Changeset, Edge, Endpoint, Generation, Port, Position, Project};
use nodecraft_nodes::{JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
use nodecraft_types::{CanvasId, EdgeId, GenerationId, Modality, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, VisibleIf};

const C: CanvasId = CanvasId::from_bytes([1; 16]);
const TEXT: PortType = PortType { modality: Modality::Text, many: false };
const LIST: PortType = PortType { modality: Modality::Text, many: true };

/// A generator stand-in: a list text input, a text output, a `style` parameter and a `detail`
/// parameter that is only visible when `style` is "fine".
struct Gen(NodeDescriptor);
impl NodeKind for Gen {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("gen").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let spec = |name: &str, default: &str, visible_if: Option<VisibleIf>| ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        control: ParamControl::Text { multiline: false, max_bytes: 1024 },
        required: false,
        default: Some(Value::Text(default.into())),
        visible_if,
    };
    let detail_if = Some(VisibleIf { parameter: "style".into(), equals: Value::Text("fine".into()) });
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.gen".into(),
            label: "Gen".into(),
            category: Category::Generation,
            inputs: vec![Port { name: "in".into(), port_type: LIST }],
            outputs: vec![Port { name: "out".into(), port_type: TEXT }],
            params: ParamSchema { params: vec![spec("style", "plain", None), spec("detail", "none", detail_if)] },
            quick_add: None,
        })))
        .unwrap();
    kinds
}
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn generation(n: u8) -> GenerationId {
    GenerationId::from_bytes([n; 16])
}

/// 1 (text "fox") → 2 (gen) → 3 (gen), and 4 (text "owl") on its own. 2 and 3 have results
/// stamped with their current input hash.
fn project() -> Project {
    let kinds = kinds();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Stale").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let text = |n: u8, content: &str| {
        let values = [("text".to_owned(), Value::Text(content.into()))].into_iter().collect();
        kinds.get("source.text").unwrap().descriptor().instantiate(id(n), &values).unwrap()
    };
    let generator = |n: u8| kinds.get("test.gen").unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap();
    let mut changes = vec![
        Change::AddNode { canvas: C, node: text(1, "fox") },
        Change::AddNode { canvas: C, node: generator(2) },
        Change::AddNode { canvas: C, node: generator(3) },
        Change::AddNode { canvas: C, node: text(4, "owl") },
    ];
    for (e, (from, to)) in [(10u8, (1u8, 2u8)), (11, (2, 3))] {
        changes.push(Change::Connect { canvas: C, edge: edge(e, from, to, 0) });
    }
    Changeset::new(changes).apply(&mut project).unwrap();
    for n in [2u8, 3] {
        stamp(&mut project, &kinds, n);
    }
    project
}
fn edge(e: u8, from: u8, to: u8, order: u32) -> Edge {
    Edge { id: EdgeId::from_bytes([e; 16]), from: Endpoint { node: id(from), port: "out".into() }, to: Endpoint { node: id(to), port: "in".into() }, order }
}
/// Give node `n` a fresh result recording its current input hash (what the driver will do).
fn stamp(project: &mut Project, kinds: &Registry, n: u8) {
    let hash = input_hash(project.canvases().get(&C).unwrap(), kinds, id(n)).unwrap();
    let serial = project.canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().history.len();
    let g = generation(n.wrapping_mul(16).wrapping_add(u8::try_from(serial).unwrap()));
    let result = Generation { id: g, outputs: vec![Value::Text(format!("result {n}"))], input_hash: Some(hash) };
    Changeset::new(vec![
        Change::AddGeneration { canvas: C, id: id(n), index: serial, generation: result },
        Change::SetActiveGeneration { canvas: C, id: id(n), generation: Some(g) },
    ])
    .apply(project)
    .unwrap();
}
fn stale_of(project: &Project) -> BTreeSet<NodeId> {
    stale(project.canvases().get(&C).unwrap(), &kinds()).unwrap()
}
fn set_param(project: &mut Project, n: u8, name: &str, value: &str) {
    Change::SetParam { canvas: C, id: id(n), name: name.into(), value: Some(Value::Text(value.into())) }.apply(project).unwrap();
}

#[test]
fn fresh_results_are_not_stale_and_unrelated_edits_keep_them_fresh() {
    let mut p = project();
    assert!(stale_of(&p).is_empty());
    // Moving, renaming and editing an unconnected node change nothing.
    Changeset::new(vec![
        Change::MoveNode { canvas: C, id: id(2), pos: Position { x: 900.0, y: 40.0 } },
        Change::SetNodeTitle { canvas: C, id: id(3), title: "Renamed".into() },
    ])
    .apply(&mut p)
    .unwrap();
    set_param(&mut p, 4, "text", "a different owl");
    assert!(stale_of(&p).is_empty());
}

#[test]
fn a_parameter_edit_makes_the_node_and_everything_downstream_stale() {
    let mut p = project();
    set_param(&mut p, 2, "style", "bold");
    assert_eq!(stale_of(&p), [id(2), id(3)].into_iter().collect());
    // Re-running 2 refreshes it; 3 was built on 2's old result, so it stays stale.
    stamp(&mut p, &kinds(), 2);
    assert_eq!(stale_of(&p), [id(3)].into_iter().collect());
    stamp(&mut p, &kinds(), 3);
    assert!(stale_of(&p).is_empty());
}

#[test]
fn hidden_parameters_do_not_count_until_they_show() {
    let mut p = project();
    // `detail` only shows when style is "fine": changing it while hidden changes nothing.
    set_param(&mut p, 2, "detail", "lots");
    assert!(stale_of(&p).is_empty());
    set_param(&mut p, 2, "style", "fine");
    assert!(stale_of(&p).contains(&id(2)));
}

#[test]
fn upstream_source_content_and_result_changes_propagate() {
    let mut p = project();
    set_param(&mut p, 1, "text", "a red fox");
    assert_eq!(stale_of(&p), [id(2), id(3)].into_iter().collect());

    let mut p = project();
    // Picking another result on 2 (e.g. a variation) makes 3 stale, not 2.
    let g = generation(99);
    Changeset::new(vec![
        Change::AddGeneration { canvas: C, id: id(2), index: 1, generation: Generation::new(g, vec![Value::Text("variation".into())]) },
        Change::SetActiveGeneration { canvas: C, id: id(2), generation: Some(g) },
    ])
    .apply(&mut p)
    .unwrap();
    assert_eq!(stale_of(&p), [id(3)].into_iter().collect());
}

#[test]
fn rewiring_and_reordering_inputs_changes_the_hash() {
    let p = project();
    let kinds = kinds();
    let canvas = p.canvases().get(&C).unwrap();
    let before = input_hash(canvas, &kinds, id(2)).unwrap();
    // A second noodle into the list input.
    let mut q = p.clone();
    Change::Connect { canvas: C, edge: edge(12, 4, 2, 1) }.apply(&mut q).unwrap();
    let two = input_hash(q.canvases().get(&C).unwrap(), &kinds, id(2)).unwrap();
    assert_ne!(two, before);
    // The same two noodles in the other order hash differently again.
    let mut r = p.clone();
    Changeset::new(vec![
        Change::Disconnect { canvas: C, id: EdgeId::from_bytes([10; 16]) },
        Change::Connect { canvas: C, edge: edge(12, 4, 2, 0) },
        Change::Connect { canvas: C, edge: edge(13, 1, 2, 1) },
    ])
    .apply(&mut r)
    .unwrap();
    let swapped = input_hash(r.canvases().get(&C).unwrap(), &kinds, id(2)).unwrap();
    assert_ne!(swapped, two);
    // Disconnecting makes it stale too.
    let mut s = p;
    Change::Disconnect { canvas: C, id: EdgeId::from_bytes([10; 16]) }.apply(&mut s).unwrap();
    assert!(stale_of(&s).contains(&id(2)));
}

#[test]
fn results_without_a_recorded_hash_and_nodes_without_results_are_never_stale_themselves() {
    let mut p = project();
    // A legacy result on 3 (no hash) replaces the stamped one.
    let g = generation(77);
    Changeset::new(vec![
        Change::AddGeneration { canvas: C, id: id(3), index: 1, generation: Generation::new(g, vec![Value::Text("legacy".into())]) },
        Change::SetActiveGeneration { canvas: C, id: id(3), generation: Some(g) },
    ])
    .apply(&mut p)
    .unwrap();
    // 3 is unknown, not stale; editing 3's own parameters doesn't change that...
    set_param(&mut p, 3, "style", "bold");
    assert!(stale_of(&p).is_empty());
    // ...but a stale upstream still marks it, since it was built on what 2 lends.
    set_param(&mut p, 2, "style", "bold");
    assert_eq!(stale_of(&p), [id(2), id(3)].into_iter().collect());
    // Text sources have no results and are never stale.
    assert!(!stale_of(&p).contains(&id(1)));
    assert!(input_hash(p.canvases().get(&C).unwrap(), &kinds(), id(42)).is_err());
}

#[test]
fn configuration_counts_and_nodes_their_kind_rejects_are_unknown() {
    let mut p = project();
    // A configuration change (what a dynamic kind's schema and work come from) makes it stale.
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    node.config.insert("workflow".into(), Value::Text("v2".into()));
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    assert!(stale_of(&p).contains(&id(2)));
    // A node its kind rejects (a parameter too long for its control) has no hash: unknown, so
    // it isn't stale itself, though a stale upstream still marks it.
    let mut q = project();
    let mut bad = q.canvases().get(&C).unwrap().nodes().get(&id(3)).unwrap().clone();
    bad.params.insert("style".into(), Value::Text("x".repeat(2000)));
    Change::SetNode { canvas: C, node: bad }.apply(&mut q).unwrap();
    assert!(input_hash(q.canvases().get(&C).unwrap(), &kinds(), id(3)).is_err());
    assert!(stale_of(&q).is_empty());
}
