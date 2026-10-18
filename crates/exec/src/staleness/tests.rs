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
    let canvas = q.canvases().get(&C).unwrap();
    let kinds = kinds();
    let mut hasher = InputHasher::new(canvas, &kinds, MAX_QUERY_BYTES).unwrap();
    assert!(hasher.hash(canvas, &kinds, id(3), &BTreeMap::new()).unwrap().is_none());
    assert!(hasher.hash(canvas, &kinds, id(2), &BTreeMap::new()).unwrap().is_some(), "a rejected node doesn't exhaust the shared meter");
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

#[test]
fn omitted_large_defaults_are_charged_for_every_distinct_node_before_resolution() {
    let descriptor = NodeDescriptor {
        id: "test.large_default".into(),
        label: "Large default".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema {
            params: vec![ParamSpec {
                name: "prompt".into(),
                label: "Prompt".into(),
                description: None,
                control: ParamControl::Text { multiline: true, max_bytes: 1_000_000 },
                required: false,
                default: Some(Value::Text("x".repeat(600_000))),
                visible_if: None,
            }],
        },
    };
    descriptor.validate().unwrap();
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Gen(descriptor))).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Omitted defaults").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    for n in [10, 11] {
        let node = Node::new(id(n), "test.large_default");
        assert!(node.params.is_empty());
        Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
        stamp(&mut p, &kinds, n);
    }
    let before = p.clone();
    let canvas = p.canvases().get(&C).unwrap();
    let mut query = InputHasher::new(canvas, &kinds, 1_000_000).unwrap();
    query.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap().unwrap();
    assert!(query.meter.hashed >= 600_000 && query.meter.hashed < 601_000);
    assert_eq!(query.hash(canvas, &kinds, id(11), &BTreeMap::new()).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(!query.recipes.contains_key(&id(11)), "second default must be refused before resolving/hashing its recipe");
    let error = stale_within(canvas, &kinds, 1_000_000).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert_eq!(p, before, "budget refusal is read-only");
}

#[test]
fn effective_default_and_stored_payloads_have_identical_hashes_and_admission_boundaries() {
    use nodecraft_types::{AssetRef, MediaMeta, Unsigned};
    for (control, value) in [
        (ParamControl::Text { multiline: true, max_bytes: 1_000_000 }, Value::Text("x".repeat(600_000))),
        (ParamControl::Unsigned { min: None, max: None, step: Unsigned(1) }, Value::Unsigned(Unsigned(u64::MAX))),
        (
            ParamControl::Asset { modality: Modality::Image },
            Value::Media(AssetRef {
                hash: Blake3::from_bytes([0xff; 32]),
                mime: "image/png; comment=\"long quoted MIME\"".parse().unwrap(),
                meta: MediaMeta {
                    width: Some(u32::MAX),
                    height: Some(u32::MAX),
                    duration_ms: Some(u64::MAX),
                    sample_rate: Some(u32::MAX),
                    channels: Some(u16::MAX),
                    page_count: Some(u32::MAX),
                },
            }),
        ),
    ] {
        let mut kinds = Registry::builtins().unwrap();
        kinds
            .register(Box::new(Gen(NodeDescriptor {
                id: "test.default".into(),
                label: "Default".into(),
                category: Category::Generation,
                inputs: vec![],
                outputs: vec![],
                quick_add: None,
                params: ParamSchema {
                    params: vec![ParamSpec {
                        name: "value".into(),
                        label: "Value".into(),
                        description: None,
                        control,
                        required: false,
                        default: Some(value.clone()),
                        visible_if: None,
                    }],
                },
            })))
            .unwrap();
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Admission").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let mut omitted = Node::new(id(10), "test.default");
        omitted.config.insert("configuration".into(), Value::List(vec![value.clone(), Value::Bool(true)]));
        let mut stored = omitted.clone();
        stored.id = id(11);
        stored.params.insert("value".into(), value.clone());
        Changeset::new(vec![Change::AddNode { canvas: C, node: omitted }, Change::AddNode { canvas: C, node: stored }]).apply(&mut project).unwrap();
        let canvas = project.canvases().get(&C).unwrap();
        let mut query = InputHasher::new(canvas, &kinds, MAX_QUERY_BYTES).unwrap();
        let omitted_hash = query.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap().unwrap();
        let required = query.meter.hashed;
        assert_eq!(query.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap().unwrap(), omitted_hash);
        assert_eq!(query.meter.hashed, required, "memoized recipes aren't charged twice");
        let mut exact = InputHasher::new(canvas, &kinds, required).unwrap();
        assert_eq!(exact.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap().unwrap(), omitted_hash);
        assert_eq!(exact.meter.left, 0);
        assert_eq!(exact.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap().unwrap(), omitted_hash, "successful exact admission permits memoized reuse");
        assert_eq!(
            InputHasher::new(canvas, &kinds, required - 1).unwrap().hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap_err().kind,
            ErrorKind::InvalidInput
        );
        let mut stored_query = InputHasher::new(canvas, &kinds, required).unwrap();
        assert_eq!(
            stored_query.hash(canvas, &kinds, id(11), &BTreeMap::new()).unwrap().unwrap(),
            omitted_hash,
            "stored/default representation doesn't change v2 identity"
        );
        assert_eq!(stored_query.meter.hashed, required, "same effective payload is admitted once in either representation");
    }
}

#[test]
fn scalar_and_media_reservations_cover_json_before_serialization_and_keep_v2_bytes() {
    use nodecraft_types::{AssetRef, Integer, MediaMeta, Number, Unsigned};
    let values = [
        Value::Bool(false),
        Value::Integer(Integer(i64::MIN)),
        Value::Unsigned(Unsigned(u64::MAX)),
        Value::Number(Number::new(f64::MAX).unwrap()),
        Value::Media(AssetRef {
            hash: Blake3::from_bytes([0xff; 32]),
            mime: format!("image/png; x=\"{}\"", "a".repeat(900)).parse().unwrap(),
            meta: MediaMeta {
                width: Some(u32::MAX),
                height: Some(u32::MAX),
                duration_ms: Some(u64::MAX),
                sample_rate: Some(u32::MAX),
                channels: Some(u16::MAX),
                page_count: Some(u32::MAX),
            },
        }),
    ];
    for value in values {
        let json = serde_json::to_vec(&value).unwrap();
        let mut meter = Meter { left: MAX_QUERY_BYTES, hashed: 0, refused: false };
        charge_value(&mut meter, &value).unwrap();
        assert!(meter.hashed >= VALUE_COST + json.len(), "scalar/media reservation must cover actual encoded payload");
        let mut writer = Writer::new();
        writer.value(&value, Some(&mut Meter { left: meter.hashed, hashed: 0, refused: false })).unwrap();
        let mut legacy = Writer::new();
        legacy.text("v");
        legacy.bytes(&json);
        assert_eq!(writer.finish(), legacy.finish(), "scalar/media v2 encoding is unchanged");
        let mut insufficient = Meter { left: meter.hashed - 1, hashed: 0, refused: false };
        assert_eq!(Writer::new().value(&value, Some(&mut insufficient)).unwrap_err().kind, ErrorKind::InvalidInput);
    }
}

struct Counted(Gen, std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl NodeKind for Counted {
    fn descriptor(&self) -> &NodeDescriptor {
        self.0.descriptor()
    }
    fn instance<'a>(&'a self, _: &Node) -> Result<std::borrow::Cow<'a, NodeDescriptor>, Error> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(std::borrow::Cow::Borrowed(self.descriptor()))
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        self.0.mode(inputs)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.0.plan(ctx)
    }
}

fn counted_defaults() -> (Project, Registry, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let descriptor = NodeDescriptor {
        id: "test.counted".into(),
        label: "Counted".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema {
            params: vec![ParamSpec {
                name: "prompt".into(),
                label: "Prompt".into(),
                description: None,
                control: ParamControl::Text { multiline: true, max_bytes: 1_000_000 },
                required: false,
                default: Some(Value::Text("x".repeat(600_000))),
                visible_if: None,
            }],
        },
    };
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Counted(Gen(descriptor), calls.clone()))).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Counted").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    for n in [10, 11, 12] {
        Change::AddNode { canvas: C, node: Node::new(id(n), "test.counted") }.apply(&mut p).unwrap();
    }
    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    (p, kinds, calls)
}

#[test]
fn public_budget_refusal_prepares_the_instance_only_once() {
    let (p, kinds, calls) = counted_defaults();
    let canvas = p.canvases().get(&C).unwrap();
    assert_eq!(input_hash_within(canvas, &kinds, id(10), 550_000).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn shared_admission_refusal_stops_further_preparation_but_local_errors_do_not() {
    let (p, kinds, calls) = counted_defaults();
    let canvas = p.canvases().get(&C).unwrap();
    let none = BTreeMap::new();
    let mut hasher = InputHasher::new(canvas, &kinds, 1_000_000).unwrap();
    assert_eq!(hasher.hash(canvas, &kinds, id(99), &none).unwrap_err().kind, ErrorKind::NotFound);
    assert!(hasher.hash(canvas, &kinds, id(10), &none).unwrap().is_some());
    assert!(hasher.hash(canvas, &kinds, id(11), &none).is_err());
    // First: preflight plus authoritative default validation. Second: preflight only.
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(hasher.hash(canvas, &kinds, id(12), &none).is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert!(hasher.hash(canvas, &kinds, id(10), &none).is_err());
}

#[test]
fn feed_index_admission_has_an_exact_boundary() {
    let p = project();
    let kinds = kinds();
    let canvas = p.canvases().get(&C).unwrap();
    // Two resolved edges, each owning target "in" and upstream "out" names.
    let cost = 2 * (VALUE_COST + "in".len() + "out".len());
    assert_eq!(InputHasher::new(canvas, &kinds, cost).unwrap().hashed(), cost);
    assert!(InputHasher::new(canvas, &kinds, cost - 1).is_err());
    let mut hasher = InputHasher::new(canvas, &kinds, MAX_QUERY_BYTES).unwrap();
    let expected = hasher.hash(canvas, &kinds, id(2), &BTreeMap::new()).unwrap();
    let required = hasher.hashed();
    assert_eq!(InputHasher::new(canvas, &kinds, required).unwrap().hash(canvas, &kinds, id(2), &BTreeMap::new()).unwrap(), expected);
    assert!(InputHasher::new(canvas, &kinds, required - 1).unwrap().hash(canvas, &kinds, id(2), &BTreeMap::new()).is_err());
}

struct Validated(Gen, std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl NodeKind for Validated {
    fn descriptor(&self) -> &NodeDescriptor {
        self.0.descriptor()
    }
    fn validate_instance(&self, node: &Node, _: bool) -> Result<nodecraft_nodes::ResolvedParams, Error> {
        self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if node.config.contains_key("reject") {
            return Err(Error::new(ErrorKind::InvalidInput, "custom configuration is incompatible"));
        }
        let value = if node.config.contains_key("large") { "x".repeat(600_000) } else { "authoritative".into() };
        Ok(nodecraft_nodes::ResolvedParams {
            values: [("effective".into(), Value::Text(value))].into_iter().collect(),
            visible: ["effective".into()].into_iter().collect(),
        })
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        self.0.mode(inputs)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.validate_instance(ctx.node, true)?;
        Ok(vec![])
    }
}

#[test]
fn authoritative_custom_validation_controls_rejection_effective_values_and_budget() {
    let mut kinds = kinds();
    let descriptor = NodeDescriptor {
        id: "test.validated".into(),
        label: "Validated".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema::default(),
    };
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    kinds.register(Box::new(Validated(Gen(descriptor.clone()), calls.clone()))).unwrap();
    let mut p = project();
    for n in [10, 11, 12] {
        let mut node = Node::new(id(n), "test.validated");
        node.params.insert("effective".into(), Value::Text("stored".into()));
        if n == 10 {
            node.config.insert("reject".into(), Value::Bool(true));
        }
        if n == 12 {
            node.config.insert("large".into(), Value::Bool(true));
        }
        Change::AddNode { canvas: C, node }.apply(&mut p).unwrap();
    }
    let canvas = p.canvases().get(&C).unwrap();
    let before = p.clone();
    let rejected = input_hash(canvas, &kinds, id(10)).unwrap_err();
    assert_eq!(rejected.message, "custom configuration is incompatible");
    let mut hasher = InputHasher::new(canvas, &kinds, MAX_QUERY_BYTES).unwrap();
    assert!(hasher.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap().is_none());
    let actual = hasher.hash(canvas, &kinds, id(11), &BTreeMap::new()).unwrap().unwrap();
    let mut recipe = Writer::new();
    recipe.text("test.validated");
    recipe.params(&BTreeMap::new()).unwrap();
    recipe.len(1);
    recipe.param("effective", &Value::Text("authoritative".into())).unwrap();
    let mut expected = Writer::new();
    expected.digest(&recipe.finish());
    assert_eq!(actual, Blake3::from_bytes(expected.finish()), "v2 hashes authoritative returned effective values");
    let mut limited = InputHasher::new(canvas, &kinds, 550_000).unwrap();
    assert!(limited.hash(canvas, &kinds, id(12), &BTreeMap::new()).is_err(), "returned hook payload must be admitted before hashing");
    assert_eq!(p, before);
    let mut defaulted = descriptor;
    defaulted.params.params.push(ParamSpec {
        name: "prompt".into(),
        label: "Prompt".into(),
        description: None,
        control: ParamControl::Text { multiline: true, max_bytes: 1_000_000 },
        required: false,
        default: Some(Value::Text("x".repeat(600_000))),
        visible_if: None,
    });
    let mut bounded = Registry::builtins().unwrap();
    bounded.register(Box::new(Validated(Gen(defaulted), calls.clone()))).unwrap();
    calls.store(0, std::sync::atomic::Ordering::SeqCst);
    assert!(input_hash_within(canvas, &bounded, id(11), 550_000).is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "omitted default preflight refuses before custom validation");
}

#[test]
fn maximal_omitted_schema_keeps_canonical_values_and_exact_admission() {
    let mut descriptor = NodeDescriptor {
        id: "test.max_defaults".into(),
        label: "Max defaults".into(),
        category: Category::Generation,
        inputs: vec![],
        outputs: vec![],
        quick_add: None,
        params: ParamSchema::default(),
    };
    let mut expected_values = BTreeMap::new();
    // Every name/default and visibility key is counted once, independent of returned order.
    let mut required = descriptor.id.len();
    for n in 0..256 {
        let name = format!("field{n:03}");
        let text = format!("value{n:03}");
        required += 2 * name.len() + VALUE_COST + text.len();
        let value = Value::Text(text);
        expected_values.insert(name.clone(), value.clone());
        descriptor.params.params.push(ParamSpec {
            name,
            label: "Field".into(),
            description: None,
            control: ParamControl::Text { multiline: false, max_bytes: 100 },
            required: false,
            default: Some(value),
            visible_if: None,
        });
    }
    descriptor.validate().unwrap();
    let mut kinds = Registry::builtins().unwrap();
    kinds.register(Box::new(Gen(descriptor))).unwrap();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Max schema").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    Change::AddNode { canvas: C, node: Node::new(id(10), "test.max_defaults") }.apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let mut recipe = Writer::new();
    recipe.text("test.max_defaults");
    recipe.params(&BTreeMap::new()).unwrap();
    recipe.params(&expected_values).unwrap();
    let mut expected = Writer::new();
    expected.digest(&recipe.finish());
    let expected = Blake3::from_bytes(expected.finish());
    let mut exact = InputHasher::new(canvas, &kinds, required).unwrap();
    assert_eq!(exact.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap(), Some(expected));
    assert_eq!(exact.hashed(), required);
    assert!(InputHasher::new(canvas, &kinds, required - 1).unwrap().hash(canvas, &kinds, id(10), &BTreeMap::new()).is_err());
    let mut node = canvas.nodes().get(&id(10)).unwrap().clone();
    node.params.insert("field000".into(), Value::Text("override!".into()));
    expected_values.insert("field000".into(), Value::Text("override!".into()));
    Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    let canvas = p.canvases().get(&C).unwrap();
    let mut recipe = Writer::new();
    recipe.text("test.max_defaults");
    recipe.params(&BTreeMap::new()).unwrap();
    recipe.params(&expected_values).unwrap();
    let mut expected = Writer::new();
    expected.digest(&recipe.finish());
    let mut exact = InputHasher::new(canvas, &kinds, required + 1).unwrap();
    assert_eq!(exact.hash(canvas, &kinds, id(10), &BTreeMap::new()).unwrap(), Some(Blake3::from_bytes(expected.finish())));
    assert_eq!(exact.hashed(), required + 1, "stored override replaces, rather than adds to, the default reservation");
}

fn diagnostic_project(kinds: &Registry) -> Project {
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Public diagnostic fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for n in [20, 21] {
        let node = kinds.get("test.gen").unwrap().descriptor().instantiate(id(n), &Default::default()).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    project
}

fn diagnostic_results(project: &mut Project) {
    for n in [20, 21] {
        let mut node = project.canvases().get(&C).unwrap().nodes().get(&id(n)).unwrap().clone();
        let generation = GenerationId::from_bytes([n; 16]);
        node.history.push(Generation { id: generation, outputs: vec![], input_hash: Some(Blake3::from_bytes([0; 32])) });
        node.active = Some(generation);
        Change::SetNode { canvas: C, node }.apply(project).unwrap();
    }
}

#[test]
fn report_shares_legacy_sweep_hooks_and_preserves_unknown_and_stale_semantics() {
    let (mut project, kinds, calls) = diagnostic_sources("choose another source");
    diagnostic_results(&mut project);
    let canvas = project.canvases().get(&C).unwrap();
    let legacy = stale(canvas, &kinds).unwrap();
    let legacy_calls = calls.swap(0, std::sync::atomic::Ordering::SeqCst);
    assert!(legacy_calls > 0);
    let report = stale_report(canvas, &kinds).unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), legacy_calls);
    assert_eq!(report.nodes, legacy);
    assert_eq!(report.unknown_total, 2);
    let first = report.unknown.unwrap();
    assert_eq!((first.node, first.cause, first.error.kind), (id(20), id(1), ErrorKind::Unsupported));
    assert!(first.error.message.contains("choose another source"));
    assert!(std::mem::size_of::<HashDiagnostic>() <= DIAGNOSTIC_OUTPUT_OVERHEAD);
    // No recorded hash means no new validation just to manufacture diagnostics.
    let (project, kinds, calls) = diagnostic_sources("unused reason");
    let report = stale_report(project.canvases().get(&C).unwrap(), &kinds).unwrap();
    assert_eq!(report.unknown_total, 0);
    assert_eq!(report.unknown, None);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn report_output_uses_exact_existing_reservation_and_refuses_before_construction() {
    let (mut project, kinds, _) = diagnostic_sources("bounded reason");
    diagnostic_results(&mut project);
    let canvas = project.canvases().get(&C).unwrap();
    let (silent, charged) = query_stale(canvas, &kinds, MAX_QUERY_BYTES, false).unwrap();
    let (report, total) = query_stale(canvas, &kinds, MAX_QUERY_BYTES, true).unwrap();
    let output = MAX_DIAGNOSTIC_BYTES + DIAGNOSTIC_OUTPUT_OVERHEAD;
    assert_eq!(total, charged + output);
    assert_eq!(query_stale(canvas, &kinds, total, true).unwrap().0, report);
    assert!(query_stale(canvas, &kinds, total - 1, true).is_err());
    assert_eq!(query_stale(canvas, &kinds, charged, false).unwrap().0, silent);
    let (canvas, kinds, mut exact) = empty_query(output);
    let diagnostic = exact.reported(id(20), InvalidHash::Router(id(1))).unwrap();
    assert_eq!(diagnostic.cause, id(1));
    assert!(exact.diagnostic_output_bytes > 0);
    assert_eq!(exact.meter.left, 0);
    let (_, _, mut short) = empty_query(output - 1);
    assert!(short.reported(id(20), InvalidHash::Router(id(1))).is_err());
    assert_eq!(short.diagnostic_output_bytes, 0);
    assert!(short.meter.refused);
    assert_eq!(short.hash(&canvas, &kinds, id(99), &BTreeMap::new()).unwrap_err().message, too_large().message);
}

#[test]
fn report_shared_rejected_source_reuses_cache_and_target_rejection_names_target() {
    let (mut project, kinds, calls) = diagnostic_sources("shared rejection");
    diagnostic_results(&mut project);
    Changeset::new(vec![Change::Disconnect { canvas: C, id: EdgeId::from_bytes([2; 16]) }, Change::Connect { canvas: C, edge: edge(2, 1, 21, 0) }])
        .apply(&mut project)
        .unwrap();
    let report = stale_report(project.canvases().get(&C).unwrap(), &kinds).unwrap();
    assert_eq!(report.unknown_total, 2);
    assert_eq!(calls.swap(0, std::sync::atomic::Ordering::SeqCst), 1);
    let mut source = project.canvases().get(&C).unwrap().nodes().get(&id(1)).unwrap().clone();
    let generation = GenerationId::from_bytes([1; 16]);
    source.history.push(Generation { id: generation, outputs: vec![], input_hash: Some(Blake3::from_bytes([0; 32])) });
    source.active = Some(generation);
    Change::SetNode { canvas: C, node: source }.apply(&mut project).unwrap();
    let report = stale_report(project.canvases().get(&C).unwrap(), &kinds).unwrap();
    assert_eq!(report.unknown_total, 3);
    let first = report.unknown.unwrap();
    assert_eq!((first.node, first.cause), (id(1), id(1)));
    assert_eq!(first.error.kind, ErrorKind::Unsupported);
    assert_eq!(first.error.message, "shared rejection");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn report_keeps_known_stale_results_when_an_unrelated_hash_is_unknown() {
    let (mut project, kinds, _) = diagnostic_sources("bad source");
    diagnostic_results(&mut project);
    let source = kinds.get("source.text").unwrap().instantiate_instance(id(2), Default::default(), &Default::default()).unwrap();
    Change::SetNode { canvas: C, node: source }.apply(&mut project).unwrap();
    let canvas = project.canvases().get(&C).unwrap();
    let report = stale_report(canvas, &kinds).unwrap();
    assert_eq!(report.nodes, stale(canvas, &kinds).unwrap());
    assert_eq!(report.nodes, [id(21)].into_iter().collect());
    assert_eq!(report.unknown_total, 1);
    assert_eq!(report.unknown.unwrap().cause, id(1));
}

#[test]
fn diagnostics_identify_invalid_routers() {
    let kinds = kinds();
    let mut project = diagnostic_project(&kinds);
    for (router, target) in [(10, 20), (11, 21)] {
        let mut node = kinds.get("structure.router").unwrap().instantiate_instance(id(router), Default::default(), &Default::default()).unwrap();
        node.config.insert("modality".into(), Value::Text("invalid fixture modality".into()));
        Changeset::new(vec![Change::AddNode { canvas: C, node }, Change::Connect { canvas: C, edge: edge(router, router, target, 0) }])
            .apply(&mut project)
            .unwrap();
    }
    let canvas = project.canvases().get(&C).unwrap();
    for (router, target, other) in [(10, 20, 11), (11, 21, 10)] {
        let error = input_hash(canvas, &kinds, id(target)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert!(error.message.contains(&id(router).to_string()), "identify actual router: {}", error.message);
        assert!(!error.message.contains(&id(other).to_string()));
    }
}

struct DiagnosticSource {
    descriptor: NodeDescriptor,
    calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    reason: String,
}
impl NodeKind for DiagnosticSource {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn validate_instance(&self, node: &Node, _: bool) -> Result<nodecraft_nodes::ResolvedParams, Error> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let kind = if node.id == id(1) { ErrorKind::Unsupported } else { ErrorKind::Conflict };
        Err(Error::new(kind, self.reason.clone())) // Native hook owns this returned allocation.
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![])
    }
}
fn diagnostic_sources(reason: &str) -> (Project, Registry, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let mut kinds = kinds();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    kinds
        .register(Box::new(DiagnosticSource {
            descriptor: NodeDescriptor {
                id: "test.rejected".into(),
                label: "Rejected source".into(),
                category: Category::Source,
                inputs: vec![],
                outputs: vec![Port { name: "out".into(), port_type: TEXT }],
                params: ParamSchema::default(),
                quick_add: None,
            },
            calls: calls.clone(),
            reason: reason.into(),
        }))
        .unwrap();
    let mut project = diagnostic_project(&kinds);
    for (source, target) in [(1, 20), (2, 21)] {
        let node = kinds.get("test.rejected").unwrap().descriptor().instantiate(id(source), &Default::default()).unwrap();
        Changeset::new(vec![Change::AddNode { canvas: C, node }, Change::Connect { canvas: C, edge: edge(source, source, target, 0) }])
            .apply(&mut project)
            .unwrap();
    }
    (project, kinds, calls)
}

#[test]
fn diagnostics_preserve_source_identity_reason_and_kind() {
    let (project, kinds, _) = diagnostic_sources("choose a supported source configuration");
    let canvas = project.canvases().get(&C).unwrap();
    for (source, target, kind) in [(1, 20, ErrorKind::Unsupported), (2, 21, ErrorKind::Conflict)] {
        let error = input_hash(canvas, &kinds, id(target)).unwrap_err();
        assert_eq!(error.kind, kind, "preserve source rejection category");
        assert!(error.message.contains(&id(source).to_string()));
        assert!(error.message.contains("choose a supported source configuration"));
    }
}

#[test]
fn bounded_rejection_cache_and_output_copy_only_once_after_admission() {
    let native_reason = format!("{}TAIL_MARKER", "bad\n\t\u{202e}🙂".repeat(500));
    let (project, kinds, calls) = diagnostic_sources(&native_reason);
    let canvas = project.canvases().get(&C).unwrap();
    let mut query = InputHasher::new(canvas, &kinds, MAX_QUERY_BYTES).unwrap();
    assert!(query.hash(canvas, &kinds, id(1), &BTreeMap::new()).unwrap().is_none());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let reason = &query.recipes.get(&id(1)).unwrap().as_ref().err().unwrap().reason;
    assert!(reason.starts_with("bad   🙂"));
    assert!(reason.ends_with('…'));
    assert!(reason.len() <= MAX_REJECTION_REASON_BYTES);
    assert!(!reason.contains("TAIL_MARKER"));
    assert!(!reason.chars().any(|c| c.is_control() || c == '\u{202e}'));
    let copied = query.diagnostic_cache_bytes;
    assert_eq!(copied, reason.len());
    assert!(copied > 0);
    let charged = query.hashed();
    for _ in 0..3 {
        assert!(query.hash(canvas, &kinds, id(1), &BTreeMap::new()).unwrap().is_none());
    }
    assert_eq!(query.hashed(), charged, "memoized invalid recipe adds no record charge");
    assert_eq!(query.diagnostic_cache_bytes, copied);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let HashResult::Invalid(failure) = query.hash_detailed(canvas, &kinds, id(20), &BTreeMap::new()).unwrap() else { panic!("source must be rejected") };
    let error = query.diagnostic(failure).unwrap();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains(&id(1).to_string()));
    assert!(error.message.len() < MAX_DIAGNOSTIC_BYTES);
    assert_eq!(query.diagnostic_output_bytes, error.message.len());
    assert!(query.diagnostic_output_bytes > 0);
    assert_eq!(query.diagnostic_cache_bytes, copied);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "formatting/source query does not repeat native hook");
    assert!(query.hash(canvas, &kinds, id(21), &BTreeMap::new()).unwrap().is_none());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2, "different rejected source owns its own record");
}

fn empty_query(budget: usize) -> (Canvas, Registry, InputHasher) {
    let canvas = Canvas::new(C, "Empty");
    let kinds = Registry::default();
    let query = InputHasher::new(&canvas, &kinds, budget).unwrap();
    (canvas, kinds, query)
}
#[test]
fn diagnostic_record_and_output_exact_budgets_refuse_before_copy_and_stay_sticky() {
    let record = CACHE_REJECTION_OVERHEAD + 3; // é + normalized newline = 3 UTF-8 bytes.
    let (_, _, mut exact) = empty_query(record);
    let cached = exact.cache_rejection(Error::new(ErrorKind::Conflict, "é\n")).unwrap();
    assert_eq!(cached.reason, "é ");
    assert_eq!(cached.kind, ErrorKind::Conflict);
    assert_eq!(exact.meter.left, 0);
    assert_eq!(exact.diagnostic_cache_bytes, 3);
    let (canvas, kinds, mut refused) = empty_query(record - 1);
    assert!(refused.cache_rejection(Error::new(ErrorKind::Conflict, "é\n")).is_err());
    assert_eq!(refused.diagnostic_cache_bytes, 0);
    assert!(refused.meter.refused);
    let error = refused.hash(&canvas, &kinds, id(99), &BTreeMap::new()).unwrap_err();
    assert_eq!(error.message, too_large().message, "shared refusal overrides later memo/NotFound requests");
    let output = MAX_DIAGNOSTIC_BYTES + DIAGNOSTIC_OUTPUT_OVERHEAD;
    for kind in [InvalidHash::Target(id(1)), InvalidHash::Source(id(1))] {
        for enough in [false, true] {
            let (_, _, mut query) = empty_query(record + output - usize::from(!enough));
            let rejection = query.cache_rejection(Error::new(ErrorKind::Conflict, "é\n")).unwrap();
            query.recipes.insert(id(1), Err(rejection));
            let result = query.diagnostic(kind);
            if enough {
                let error = result.unwrap();
                assert_eq!(error.kind, ErrorKind::Conflict);
                assert_eq!(query.meter.left, 0);
                assert_eq!(query.diagnostic_output_bytes, error.message.len());
                assert!(query.diagnostic_output_bytes > 0);
            } else {
                assert!(result.is_err());
                assert_eq!(query.diagnostic_output_bytes, 0);
                assert!(query.meter.refused);
                assert_eq!(query.diagnostic_cache_bytes, 3, "earlier admitted cache remains accounted");
            }
        }
    }
    for enough in [false, true] {
        let (_, _, mut query) = empty_query(output - usize::from(!enough));
        let result = query.diagnostic(InvalidHash::Router(id(10)));
        assert_eq!(result.is_ok(), enough);
        if enough {
            assert!(result.unwrap().message.contains(&id(10).to_string()));
            assert_eq!(query.meter.left, 0);
            assert!(query.diagnostic_output_bytes > 0);
        } else {
            assert_eq!(query.diagnostic_output_bytes, 0);
        }
    }
}

#[test]
fn diagnostic_prefix_bounds_unicode_line_and_bidi_controls_without_full_message_copy() {
    for (reason, expected) in
        [("plain error", "plain error"), ("", "the node kind rejected this node"), ("x\u{2028}\u{2029}\u{061c}\u{200e}\u{200f}\u{202a}\u{2066}y", "x       y")]
    {
        let (_, _, mut query) = empty_query(10_000);
        let got = query.cache_rejection(Error::new(ErrorKind::Unknown, reason)).unwrap();
        assert_eq!(got.kind, ErrorKind::Unknown);
        assert_eq!(got.reason, expected);
    }
    for (reason, len) in [("x".repeat(253), 253), ("x".repeat(254), 256), ("🙂".repeat(64), 255), ("é".repeat(128), 255)] {
        let (_, _, mut query) = empty_query(10_000);
        let got = query.cache_rejection(Error::new(ErrorKind::InvalidInput, reason)).unwrap();
        assert_eq!(got.reason.len(), len);
        assert_eq!(query.diagnostic_cache_bytes, len);
    }
}

#[test]
fn detailed_source_choice_is_deterministic_and_default_validation_reason_is_retained() {
    let (mut project, rejected_kinds, _) = diagnostic_sources("native rejection");
    // List orders are unique; the earlier feed wins even when its source ID is larger.
    Changeset::new(vec![
        Change::Disconnect { canvas: C, id: EdgeId::from_bytes([1; 16]) },
        Change::Connect { canvas: C, edge: edge(1, 1, 20, 1) },
        Change::Connect { canvas: C, edge: edge(3, 2, 20, 0) },
    ])
    .apply(&mut project)
    .unwrap();
    let error = input_hash(project.canvases().get(&C).unwrap(), &rejected_kinds, id(20)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(error.message.contains(&id(2).to_string()));
    let mut kinds = kinds();
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.defaultsource".into(),
            label: "Default source".into(),
            category: Category::Source,
            inputs: vec![],
            outputs: vec![Port { name: "out".into(), port_type: TEXT }],
            quick_add: None,
            params: ParamSchema {
                params: vec![ParamSpec {
                    name: "setting".into(),
                    label: "Setting".into(),
                    description: None,
                    required: false,
                    default: None,
                    visible_if: None,
                    control: ParamControl::Text { multiline: false, max_bytes: 1 },
                }],
            },
        })))
        .unwrap();
    let mut project = diagnostic_project(&kinds);
    let mut node = Node::new(id(12), "test.defaultsource");
    node.outputs = vec![Port { name: "out".into(), port_type: TEXT }];
    node.params.insert("setting".into(), Value::Text("long".into()));
    Changeset::new(vec![Change::AddNode { canvas: C, node }, Change::Connect { canvas: C, edge: edge(12, 12, 20, 0) }]).apply(&mut project).unwrap();
    let canvas = project.canvases().get(&C).unwrap();
    let direct = input_hash(canvas, &kinds, id(12)).unwrap_err();
    let upstream = input_hash(canvas, &kinds, id(20)).unwrap_err();
    assert_eq!(upstream.kind, direct.kind);
    assert!(upstream.message.ends_with(&direct.message));
    assert!(upstream.message.contains(&id(12).to_string()));
}
