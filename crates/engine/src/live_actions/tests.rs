use super::*;
use nodecraft_types::{AssetRef, MediaMeta, Value};
use serde_json::json;
use std::collections::BTreeMap;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const SOURCE: NodeId = NodeId::from_bytes([10; 16]);
const BLUR: NodeId = NodeId::from_bytes([11; 16]);

fn png(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height).flat_map(|i| [(i % 251) as u8, 80, 160, 255]).collect();
    nodecraft_media::encode_png(width, height, &pixels, &nodecraft_media::Limits::default()).unwrap()
}

fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    let hash = s.import_asset(&png(16, 12)).unwrap();
    let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta::default() };
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": SOURCE, "kind": "source.image", "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": SOURCE, "name": "asset", "value": Value::Media(asset)})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": BLUR, "kind": "image.blur", "pos": {"x": 400, "y": 0}})).unwrap();
    s
}

fn connect(s: &mut Session) {
    let edge = nodecraft_types::EdgeId::from_bytes([12; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": SOURCE, "port": "out"}, "to": {"node": BLUR, "port": "image"}, "order": 0}}),
    )
    .unwrap();
}

/// Plans, runs and records whatever is pending; whether something was recorded.
fn settle(s: &mut Session) -> bool {
    let Some(job) = pending(s, CANVAS, &BTreeMap::new()).unwrap() else { return false };
    let kinds = Registry::builtins().unwrap();
    let outcome = compute(job, &kinds).unwrap();
    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    apply(s, outcome).unwrap()
}

fn result(s: &Session) -> Option<nodecraft_types::Blake3> {
    let node = s.document().canvases().get(&CANVAS)?.nodes().get(&BLUR)?;
    let active = node.active?;
    match node.history.iter().find(|g| g.id == active)?.outputs.first()? {
        Value::Media(asset) => Some(asset.hash),
        _ => None,
    }
}

fn sigma(s: &Session) -> Value {
    s.document().canvases().get(&CANVAS).unwrap().nodes().get(&BLUR).unwrap().params.get("sigma").cloned().unwrap_or(Value::Bool(false))
}

#[test]
fn an_unconnected_action_waits_and_a_connected_one_settles_into_the_edit() {
    let mut s = session();
    assert!(!settle(&mut s), "nothing connected: nothing to run");
    connect(&mut s);
    assert!(settle(&mut s));
    let first = result(&s).expect("the blur has a result");
    assert!(s.assets().get(&first).is_some(), "its image is in the project");
    assert!(pending(&s, CANVAS, &BTreeMap::new()).unwrap().is_none(), "settled: nothing stale");
    // One undo removes the connection and its result together.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(result(&s).is_none());
    assert!(s.document().canvases().get(&CANVAS).unwrap().edges().is_empty());
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(result(&s), Some(first));
}

#[test]
fn a_parameter_change_recomputes_and_undo_restores_value_and_result_together() {
    let mut s = session();
    connect(&mut s);
    settle(&mut s);
    let first = result(&s).unwrap();
    let before = sigma(&s);
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(6.0).unwrap())}))
        .unwrap();
    assert!(settle(&mut s));
    let second = result(&s).unwrap();
    assert_ne!(first, second);
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!((sigma(&s), result(&s)), (before, Some(first)), "old value, old result");
    assert!(pending(&s, CANVAS, &BTreeMap::new()).unwrap().is_none(), "and nothing to recompute");
    // Results wait while there is something to redo; redo brings the newer result back.
    s.dispatch("edit.redo", &json!({})).unwrap();
    assert_eq!(result(&s), Some(second));
}

#[test]
fn results_for_an_older_revision_are_dropped() {
    let mut s = session();
    connect(&mut s);
    let job = pending(&s, CANVAS, &BTreeMap::new()).unwrap().unwrap();
    let outcome = compute(job, &Registry::builtins().unwrap()).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(3.0).unwrap())}))
        .unwrap();
    let revision = s.revision();
    assert!(!apply(&mut s, outcome).unwrap());
    assert_eq!(s.revision(), revision, "nothing recorded");
    assert!(settle(&mut s), "the next round records the current state");
}

#[test]
fn a_text_chain_settles_in_one_round() {
    let mut s = session();
    let (text, split, join) = (NodeId::from_bytes([20; 16]), NodeId::from_bytes([21; 16]), NodeId::from_bytes([22; 16]));
    for (id, kind) in [(text, "source.text"), (split, "text.split"), (join, "text.concat")] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 300}})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": text, "name": "text", "value": Value::Text("a,b,c".into())})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": join, "name": "separator", "value": Value::Text("+".into())})).unwrap();
    for (n, from, to, port) in [(30u8, text, split, "text"), (31, split, join, "texts")] {
        let edge = nodecraft_types::EdgeId::from_bytes([n; 16]);
        s.dispatch(
            "edge.connect",
            &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": from, "port": "out"}, "to": {"node": to, "port": port}, "order": 0}}),
        )
        .unwrap();
    }
    assert!(settle(&mut s));
    let joined = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&join).unwrap();
    let active = joined.history.iter().find(|g| Some(g.id) == joined.active).unwrap();
    assert_eq!(active.outputs, vec![Value::Text("a+b+c".into())]);
}

#[test]
fn a_reopened_project_records_new_results_at_revisions_it_used_before() {
    // The first session settles a result at some revision.
    let mut s = session();
    connect(&mut s);
    settle(&mut s);
    let first = result(&s).unwrap();
    // A second session opens that document: its revisions count from zero again.
    let mut again = Session::new(s.document().clone()).unwrap();
    let source = match s.document().canvases().get(&CANVAS).unwrap().nodes().get(&SOURCE).unwrap().params.get("asset") {
        Some(Value::Media(asset)) => asset.hash,
        _ => panic!(),
    };
    for hash in [source, first] {
        again.import_asset(s.assets().get(&hash).unwrap()).unwrap();
    }
    // One edit that records nothing, so the next rounds' bases include the first session's.
    again.dispatch("node.move", &json!({"canvas": CANVAS, "id": SOURCE, "pos": {"x": 5, "y": 5}})).unwrap();
    for name in ["sigma"] {
        for v in [3.0, 4.0, 5.0] {
            again
                .dispatch(
                    "node.param_set",
                    &json!({"canvas": CANVAS, "id": BLUR, "name": name, "value": Value::Number(nodecraft_types::Number::new(v).unwrap())}),
                )
                .unwrap();
            assert!(settle(&mut again), "recorded at revision {}", again.revision());
        }
    }
}

#[test]
fn a_failed_node_waits_for_its_inputs_to_change() {
    let mut s = session();
    let split = NodeId::from_bytes([40; 16]);
    let text = NodeId::from_bytes([41; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": text, "kind": "source.text", "pos": {"x": 0, "y": 600}})).unwrap();
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": split, "kind": "text.split", "pos": {"x": 300, "y": 600}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": split, "name": "separator", "value": Value::Text(String::new())})).unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([42; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": text, "port": "out"}, "to": {"node": split, "port": "text"}, "order": 0}}),
    )
    .unwrap();
    let kinds = Registry::builtins().unwrap();
    let outcome = compute(pending(&s, CANVAS, &BTreeMap::new()).unwrap().unwrap(), &kinds).unwrap();
    let failed: BTreeMap<_, _> = outcome.failed.iter().filter_map(|(id, _, hash)| hash.map(|h| (*id, h))).collect();
    assert!(failed.contains_key(&split));
    // An unrelated edit: still skipped.
    s.dispatch("node.move", &json!({"canvas": CANVAS, "id": SOURCE, "pos": {"x": 50, "y": 50}})).unwrap();
    assert!(pending(&s, CANVAS, &failed).unwrap().is_none());
    // Its own setting changes: tried again.
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": split, "name": "separator", "value": Value::Text(",".into())})).unwrap();
    assert!(pending(&s, CANVAS, &failed).unwrap().is_some());
}

#[test]
fn a_result_joining_a_drag_keeps_it_one_step() {
    let mut s = session();
    connect(&mut s);
    settle(&mut s);
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(9.0).unwrap())}))
        .unwrap();
    let pos = |s: &Session| s.document().canvases().get(&CANVAS).unwrap().nodes().get(&BLUR).unwrap().pos;
    let start = pos(&s);
    // A drag starts before the recompute lands; the result joins the drag's step.
    s.dispatch("node.move", &json!({"canvas": CANVAS, "id": BLUR, "pos": {"x": 500, "y": 10}, "gesture": "drag-1"})).unwrap();
    assert!(settle(&mut s));
    s.dispatch("node.move", &json!({"canvas": CANVAS, "id": BLUR, "pos": {"x": 600, "y": 20}, "gesture": "drag-1"})).unwrap();
    // One undo takes the whole drag back.
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(pos(&s), start);
    assert_eq!(sigma(&s), Value::Number(nodecraft_types::Number::new(9.0).unwrap()), "the parameter edit is the next step");
}

#[test]
fn an_action_that_cannot_run_does_not_hold_back_one_that_can() {
    let mut s = session();
    let other = NodeId::from_bytes([13; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": other, "kind": "image.blur", "pos": {"x": 400, "y": 300}})).unwrap();
    let edge = nodecraft_types::EdgeId::from_bytes([14; 16]);
    s.dispatch(
        "edge.connect",
        &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": SOURCE, "port": "out"}, "to": {"node": other, "port": "image"}, "order": 0}}),
    )
    .unwrap();
    assert!(settle(&mut s), "the connected blur runs while the unconnected one waits");
    let nodes = s.document().canvases().get(&CANVAS).unwrap().nodes();
    assert!(nodes.get(&other).unwrap().active.is_some());
    assert!(nodes.get(&BLUR).unwrap().active.is_none());
}

#[test]
fn a_settled_canvas_stays_settled_through_layout_only() {
    let mut s = session();
    connect(&mut s);
    settle(&mut s);
    let settled = Settled::now(&s, CANVAS).expect("nothing to redo");
    assert!(settled.holds(&s, CANVAS));
    s.dispatch("node.move", &json!({"canvas": CANVAS, "id": BLUR, "pos": {"x": 900, "y": 40}})).unwrap();
    assert!(settled.holds(&s, CANVAS), "moving a node changes nothing a run reads");
    assert!(!settled.holds(&s, CanvasId::from_bytes([9; 16])), "another canvas");
    s.dispatch("edit.undo", &json!({})).unwrap();
    assert!(!settled.holds(&s, CANVAS), "results wait while there is something to redo");
    assert!(Settled::now(&s, CANVAS).is_none());
    s.dispatch("edit.redo", &json!({})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": BLUR, "name": "sigma", "value": Value::Number(nodecraft_types::Number::new(6.0).unwrap())}))
        .unwrap();
    assert!(!settled.holds(&s, CANVAS), "a parameter change needs a new look");
}

#[test]
fn an_action_behind_a_batch_follows_its_stale_source_a_round_later() {
    let mut s = session();
    let (text, split, batch, replace) =
        (NodeId::from_bytes([50; 16]), NodeId::from_bytes([51; 16]), NodeId::from_bytes([52; 16]), NodeId::from_bytes([53; 16]));
    for (id, kind) in [(text, "source.text"), (split, "text.split"), (batch, nodecraft_nodes::batch::ID), (replace, "text.replace")] {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 900}})).unwrap();
    }
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": text, "name": "text", "value": Value::Text("a,b".into())})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": replace, "name": "find", "value": Value::Text("a".into())})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": replace, "name": "replacement", "value": Value::Text("x".into())})).unwrap();
    for (n, from, out, to, port) in [(54u8, text, "out", split, "text"), (55, split, "out", batch, "in"), (56, batch, "item", replace, "text")] {
        let edge = nodecraft_types::EdgeId::from_bytes([n; 16]);
        s.dispatch(
            "edge.connect",
            &json!({"canvas": CANVAS, "edge": {"id": edge, "from": {"node": from, "port": out}, "to": {"node": to, "port": port}, "order": 0}}),
        )
        .unwrap();
    }
    let outputs = |s: &Session| {
        let node = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&replace).unwrap();
        node.history.iter().find(|g| Some(g.id) == node.active).map(|g| (g.outputs.clone(), g.items.len()))
    };
    let mut rounds = 0;
    while settle(&mut s) {
        rounds += 1;
        assert!(rounds < 8, "settles");
    }
    let first = outputs(&s).expect("the replace ran over the batch");
    // The split goes stale with a result; the batch only collects existing results, so the
    // replace waits for the split's new one rather than holding it back.
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": text, "name": "text", "value": Value::Text("aa,c".into())})).unwrap();
    let mut rounds = 0;
    while settle(&mut s) {
        rounds += 1;
        assert!(rounds < 8, "settles");
    }
    assert!(rounds >= 1);
    assert_ne!(outputs(&s), Some(first), "the replace followed the new text");
}

mod techniques {
    //! Technique nodes refresh live when their version is local work only (#532).
    use super::*;

    const BRIEF: NodeId = NodeId::from_bytes([40; 16]);
    const INNER: NodeId = NodeId::from_bytes([41; 16]);
    const VERSION: CanvasId = CanvasId::from_bytes([42; 16]);
    const PLACED: NodeId = NodeId::from_bytes([43; 16]);
    const FEED: NodeId = NodeId::from_bytes([44; 16]);

    fn create(s: &mut Session, id: NodeId, kind: &str, x: f32) {
        s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": x, "y": 600}})).unwrap();
    }

    fn text(s: &mut Session, id: NodeId, value: &str) {
        s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": id, "name": "text", "value": Value::Text(value.into())})).unwrap();
    }

    fn wire(s: &mut Session, n: u8, from: NodeId, to: NodeId, port: &str) {
        let edge =
            json!({"id": nodecraft_types::EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": to, "port": port}, "order": 0});
        s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
    }

    /// Brief → `kind` (reading `port`, with `params`) published as a technique and placed.
    /// Brief → a text model, as a version put straight into a project that knows the builtin
    /// catalog's models (publishing would want a provider route), placed and fed.
    fn placed_with_model() -> Session {
        use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Technique, TechniquePort};
        let catalog = crate::generation_catalog::GenerationCatalog::builtin().unwrap();
        let kinds = catalog.registry().unwrap();
        let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "P").unwrap();
        Change::AddCanvas { canvas: Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
        let brief =
            kinds.get("source.text").unwrap().instantiate_instance(BRIEF, Default::default(), &[("text".into(), Value::Text("a cat".into()))].into()).unwrap();
        let model = [("model".into(), Value::Text("anthropic.claude-opus-5-5".into()))].into();
        let writer = kinds.get(nodecraft_nodes::generation::TEXT_KIND).unwrap().instantiate_instance(INNER, Default::default(), &model).unwrap();
        let port = |node: &nodecraft_graph::Node| node.outputs.first().map(|p| (p.name.clone(), p.port_type)).unwrap();
        let ((brief_out, brief_type), (writer_out, writer_type)) = (port(&brief), port(&writer));
        let edge = Edge {
            id: nodecraft_types::EdgeId::from_bytes([45; 16]),
            from: Endpoint { node: BRIEF, port: brief_out.clone() },
            to: Endpoint { node: INNER, port: "prompt".into() },
            order: 0,
        };
        let technique = Technique {
            id: nodecraft_types::TechniqueId::from_bytes([47; 16]),
            version: 1,
            name: "Write".into(),
            description: String::new(),
            category: String::new(),
            tags: Vec::new(),
            inputs: vec![TechniquePort { name: "Brief".into(), description: String::new(), node: BRIEF, port: brief_out, port_type: brief_type }],
            outputs: vec![TechniquePort { name: "Answer".into(), description: String::new(), node: INNER, port: writer_out, port_type: writer_type }],
            params: Vec::new(),
            source: None,
            retired: false,
        };
        let version = Canvas::technique(VERSION, "Write v1", technique, vec![brief, writer], vec![edge]).unwrap();
        Change::AddCanvas { canvas: version }.apply(&mut project).unwrap();
        let mut resources = crate::commands::Resources::default();
        resources.insert(catalog.clone());
        resources.insert(catalog.run_catalog());
        let mut s = Session::with_registries(project, crate::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
        place(&mut s);
        s
    }

    /// Places [`VERSION`] as [`PLACED`], fed by its own text.
    fn place(s: &mut Session) {
        s.dispatch("technique.place", &json!({"canvas": CANVAS, "version": VERSION, "id": PLACED, "pos": {"x": 600.0, "y": 600.0}})).unwrap();
        create(s, FEED, "source.text", 0.0);
        text(s, FEED, "a dog");
        wire(s, 48, FEED, PLACED, "brief");
    }

    fn placed(kind: &str, port: &str, params: &[(&str, Value)]) -> Session {
        let mut s = session();
        create(&mut s, BRIEF, "source.text", 0.0);
        text(&mut s, BRIEF, "a cat");
        create(&mut s, INNER, kind, 300.0);
        for (name, value) in params {
            s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": INNER, "name": name, "value": value})).unwrap();
        }
        wire(&mut s, 45, BRIEF, INNER, port);
        let publish = json!({"canvas": CANVAS, "published": VERSION, "seed": NodeId::from_bytes([46; 16]), "technique": nodecraft_types::TechniqueId::from_bytes([47; 16]),
            "name": "Swap", "inputs": [{"node": BRIEF, "name": "Brief"}], "outputs": [{"node": INNER, "name": "Swapped"}]});
        s.dispatch("technique.publish", &publish).unwrap();
        place(&mut s);
        s
    }

    fn settle_all(s: &mut Session) {
        for _ in 0..8 {
            if !settle(s) {
                return;
            }
        }
        panic!("live recompute never settled");
    }

    fn output(s: &Session, id: NodeId) -> Option<Value> {
        let node = s.document().canvases().get(&CANVAS)?.nodes().get(&id)?;
        node.history.iter().find(|g| Some(g.id) == node.active)?.outputs.first().cloned()
    }

    #[test]
    fn an_action_only_technique_node_updates_live() {
        let mut s = placed("text.replace", "text", &[("find", Value::Text("a ".into())), ("replacement", Value::Text("one ".into()))]);
        settle_all(&mut s);
        assert_eq!(output(&s, PLACED), Some(Value::Text("one dog".into())));
        // Feeding it something else refreshes it, with no run.
        text(&mut s, FEED, "a cow");
        settle_all(&mut s);
        assert_eq!(output(&s, PLACED), Some(Value::Text("one cow".into())));
        let placed = s.document().canvases().get(&CANVAS).unwrap().nodes().get(&PLACED).unwrap();
        assert_eq!(placed.history.len(), 2, "one result per refresh");
        // Only the instance records: the version's nodes keep no results.
        let version = s.document().canvases().get(&VERSION).unwrap();
        assert!(version.nodes().values().all(|n| n.history.is_empty()), "inlined nodes record nothing");
        assert!(pending(&s, CANVAS, &BTreeMap::new()).unwrap().is_none(), "settled");
    }

    #[test]
    fn a_technique_node_with_a_paid_step_stays_out_of_live_updates() {
        let mut s = placed_with_model();
        settle_all(&mut s);
        assert_eq!(output(&s, PLACED), None, "a model call waits for an explicit run");
        text(&mut s, FEED, "a cow");
        settle_all(&mut s);
        assert_eq!(output(&s, PLACED), None);
    }
}
