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
    let result = Generation { id: g, outputs: vec![Value::Text(format!("result {n}.{serial}"))], input_hash: Some(hash) };
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

/// Replaces node `n`'s active result outputs in place, keeping its id and recorded hash.
fn rewrite_active(project: &mut Project, n: u8, outputs: Vec<Value>) {
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().clone();
    let active = node.active.unwrap();
    node.history.iter_mut().find(|g| g.id == active).unwrap().outputs = outputs;
    Change::SetNode { canvas: C, node }.apply(project).unwrap();
}

#[test]
fn a_result_rewritten_under_the_same_id_makes_downstream_stale() {
    let mut p = project();
    rewrite_active(&mut p, 2, vec![Value::Text("something else".into())]);
    // 2's own inputs didn't change; 3 was built on different content.
    assert_eq!(stale_of(&p), [id(3)].into_iter().collect());
    // Removed and re-added under the same id with other content: the same.
    let mut q = project();
    let node = q.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    let old = node.history.last().unwrap().clone();
    Changeset::new(vec![
        Change::SetActiveGeneration { canvas: C, id: id(2), generation: None },
        Change::RemoveGeneration { canvas: C, id: id(2), generation: old.id },
        Change::AddGeneration { canvas: C, id: id(2), index: 0, generation: Generation { outputs: vec![Value::Text("swapped".into())], ..old.clone() } },
        Change::SetActiveGeneration { canvas: C, id: id(2), generation: Some(old.id) },
    ])
    .apply(&mut q)
    .unwrap();
    assert_eq!(stale_of(&q), [id(3)].into_iter().collect());
}

#[test]
fn an_identical_rerun_upstream_keeps_downstream_fresh() {
    let mut p = project();
    let same = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().history.last().unwrap().outputs.clone();
    let g = generation(200);
    Changeset::new(vec![
        Change::AddGeneration { canvas: C, id: id(2), index: 1, generation: Generation::new(g, same) },
        Change::SetActiveGeneration { canvas: C, id: id(2), generation: Some(g) },
    ])
    .apply(&mut p)
    .unwrap();
    assert!(stale_of(&p).is_empty());
}

/// A two-output generator: the hash of a consumer covers only the output port it reads.
struct Pair(NodeDescriptor);
impl NodeKind for Pair {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("pair").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}

#[test]
fn only_the_selected_upstream_output_counts() {
    let mut kinds = kinds();
    kinds
        .register(Box::new(Pair(NodeDescriptor {
            id: "test.pair".into(),
            label: "Pair".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![Port { name: "a".into(), port_type: TEXT }, Port { name: "b".into(), port_type: TEXT }],
            params: ParamSchema::default(),
            quick_add: None,
        })))
        .unwrap();
    let mut p = project();
    let pair = kinds.get("test.pair").unwrap().descriptor().instantiate(id(5), &Default::default()).unwrap();
    let g = generation(150);
    let reads_b =
        Edge { id: EdgeId::from_bytes([20; 16]), from: Endpoint { node: id(5), port: "b".into() }, to: Endpoint { node: id(3), port: "in".into() }, order: 1 };
    Changeset::new(vec![
        Change::AddNode { canvas: C, node: pair },
        Change::AddGeneration { canvas: C, id: id(5), index: 0, generation: Generation::new(g, vec![Value::Text("A".into()), Value::Text("B".into())]) },
        Change::SetActiveGeneration { canvas: C, id: id(5), generation: Some(g) },
        Change::Connect { canvas: C, edge: reads_b },
    ])
    .apply(&mut p)
    .unwrap();
    stamp(&mut p, &kinds, 3);
    let check = |p: &Project| stale(p.canvases().get(&C).unwrap(), &kinds).unwrap();
    assert!(check(&p).is_empty());
    let mut q = p.clone();
    rewrite_active(&mut q, 5, vec![Value::Text("A changed".into()), Value::Text("B".into())]);
    assert!(check(&q).is_empty(), "3 reads only b");
    rewrite_active(&mut q, 5, vec![Value::Text("A".into()), Value::Text("B changed".into())]);
    assert_eq!(check(&q), [id(3)].into_iter().collect());
}

#[test]
fn a_large_source_fanned_out_is_hashed_once_and_oversized_queries_are_refused() {
    let kinds = kinds();
    let mut p = project();
    // Close to the text node's limit, feeding 2,000 stamped generators.
    let big = "x".repeat(1_000_000);
    let mut source = p.canvases().get(&C).unwrap().nodes().get(&id(1)).unwrap().clone();
    source.params.insert("text".into(), Value::Text(big.clone()));
    let mut changes = vec![Change::SetNode { canvas: C, node: source }];
    let gen_id = |i: u16| NodeId::from_bytes(u128::from(0x1000u32 + u32::from(i)).to_be_bytes());
    for i in 0..2000u16 {
        let node = kinds.get("test.gen").unwrap().descriptor().instantiate(gen_id(i), &Default::default()).unwrap();
        changes.push(Change::AddNode { canvas: C, node });
        let edge = Edge {
            id: EdgeId::from_bytes(u128::from(0x9000u32 + u32::from(i)).to_be_bytes()),
            from: Endpoint { node: id(1), port: "out".into() },
            to: Endpoint { node: gen_id(i), port: "in".into() },
            order: 0,
        };
        changes.push(Change::Connect { canvas: C, edge });
    }
    Changeset::new(changes).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let hash = input_hash(canvas, &kinds, gen_id(0)).unwrap();
    let mut stamps = Vec::new();
    for i in 0..2000u16 {
        let g = GenerationId::from_bytes(u128::from(0x5000u32 + u32::from(i)).to_be_bytes());
        stamps.push(Change::AddGeneration {
            canvas: C,
            id: gen_id(i),
            index: 0,
            generation: Generation { id: g, outputs: vec![Value::Text("r".into())], input_hash: Some(hash) },
        });
        stamps.push(Change::SetActiveGeneration { canvas: C, id: gen_id(i), generation: Some(g) });
    }
    Changeset::new(stamps).apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let (stale_set, hashed) = stale_within(canvas, &kinds, MAX_QUERY_BYTES).unwrap();
    // Only the fixture's 2 and 3 (stamped before the source changed) are stale.
    assert_eq!(stale_set, [id(2), id(3)].into_iter().collect());
    // The megabyte is charged once, not 2,000 times.
    assert!(hashed >= big.len() && hashed < 2 * big.len(), "hashed {hashed} bytes");
    // With a budget smaller than the canvas, the query is refused rather than run.
    assert_eq!(stale_within(canvas, &kinds, 500_000).unwrap_err().kind, ErrorKind::InvalidInput);
}

#[test]
fn results_stamped_under_an_older_recipe_are_stale() {
    let mut p = project();
    // A hash made under another domain tag (an older recipe) can't be told from changed inputs.
    let mut node = p.canvases().get(&C).unwrap().nodes().get(&id(2)).unwrap().clone();
    let active = node.active.unwrap();
    node.history.iter_mut().find(|g| g.id == active).unwrap().input_hash = Some(Blake3::from_bytes(*blake3::hash(b"nodecraft.input-hash.v1 ...").as_bytes()));
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    assert_eq!(stale_of(&p), [id(2), id(3)].into_iter().collect());
}

#[test]
fn a_result_recorded_for_another_port_layout_counts_as_empty() {
    let mut p = project();
    // 2's active result has two outputs for its one port: unusable, like no result.
    rewrite_active(&mut p, 2, vec![Value::Text("a".into()), Value::Text("b".into())]);
    assert_eq!(stale_of(&p), [id(3)].into_iter().collect());
    let empty = {
        let mut q = project();
        Change::SetActiveGeneration { canvas: C, id: id(2), generation: None }.apply(&mut q).unwrap();
        input_hash(q.canvases().get(&C).unwrap(), &kinds(), id(3)).unwrap()
    };
    assert_eq!(input_hash(p.canvases().get(&C).unwrap(), &kinds(), id(3)).unwrap(), empty);
}
