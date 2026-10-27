//! `run.changed` and `run.workflow` (#965): the whole canvas, quoted first, started only through
//! `run.start` with the quote's mode, token and totals.
use super::*;

fn steps(quote: &Value) -> Vec<Value> {
    quote["steps"].as_array().unwrap().iter().map(|step| step["node"].clone()).collect()
}

/// The fixture without its unpriced generator 3, so what is left can be accepted in full.
fn priced_session() -> (Session, Started) {
    let (mut s, started) = session(Budgets::default(), true);
    s.dispatch("node.remove", &json!({"canvas": C, "id": id(3)})).unwrap();
    (s, started)
}

#[test]
fn run_changed_quotes_without_starting_and_a_confirmed_run_starts_only_what_it_quoted() {
    let (mut s, started) = priced_session();
    let revision = s.revision();
    let quote = s.dispatch("run.changed", &json!({"canvas": C})).unwrap();
    assert_eq!(s.revision(), revision, "a quote changes nothing");
    assert!(started.lock().unwrap().is_empty(), "a quote starts nothing");
    assert_eq!(quote["mode"], "pending");
    assert_eq!(steps(&quote), vec![json!(id(2))], "generator 2 has no result; text 1 is a source");
    assert_eq!(quote["totals"]["USD"]["micros"], 120_000);
    // The same quote as run.quote in that mode, so its token starts it.
    assert_eq!(price(&s, &json!({"canvas": C, "mode": "pending"})).unwrap().token, quote["token"]);
    let accept = json!({"token": quote["token"], "amounts": {"USD": 120_000}});
    assert_eq!(s.dispatch("run.start", &json!({"canvas": C, "mode": "pending", "accept": accept})).unwrap(), json!({"run": "accepted"}));
    let requests = std::mem::take(&mut *started.lock().unwrap());
    assert_eq!(requests.len(), 1);
    assert!(matches!(requests[0].run.mode, ModeParam::Pending));
    assert_eq!(requests[0].quote().steps.iter().map(|step| step.node).collect::<Vec<_>>(), vec![id(2)]);
    // A token from another mode doesn't start this one.
    let selected = s.dispatch("run.quote", &request(&[2])).unwrap();
    let accept = json!({"token": selected["token"], "amounts": {"USD": 120_000}});
    assert!(s.dispatch("run.start", &json!({"canvas": C, "mode": "pending", "accept": accept})).is_err());
}

#[test]
fn run_workflow_quotes_every_generator_and_starts_with_its_own_mode() {
    let (mut s, started) = priced_session();
    let quote = s.dispatch("run.workflow", &json!({"canvas": C})).unwrap();
    assert_eq!(quote["mode"], "workflow");
    assert_eq!(steps(&quote), vec![json!(id(2))]);
    assert!(started.lock().unwrap().is_empty());
    let accept = json!({"token": quote["token"], "amounts": {"USD": 120_000}});
    s.dispatch("run.start", &json!({"canvas": C, "mode": "workflow", "accept": accept})).unwrap();
    let requests = std::mem::take(&mut *started.lock().unwrap());
    assert!(matches!(requests.first().map(|r| &r.run.mode), Some(ModeParam::Workflow)));
}

#[test]
fn nothing_to_run_is_an_empty_quote_and_bad_params_are_errors() {
    let (mut s, _) = priced_session();
    s.dispatch("node.remove", &json!({"canvas": C, "id": id(2)})).unwrap();
    for id in ["run.changed", "run.workflow"] {
        let quote = s.dispatch(id, &json!({"canvas": C})).unwrap();
        assert!(steps(&quote).is_empty(), "{id}: {quote}");
        assert_eq!(quote["reused"], json!([]));
        assert_eq!(quote["allowed"], true);
    }
    for bad in [json!({}), json!({"canvas": C, "mode": "selected"}), json!({"canvas": "not-a-canvas"}), json!({"canvas": CanvasId::from_bytes([9; 16])})] {
        for id in ["run.changed", "run.workflow"] {
            assert!(s.dispatch(id, &bad).is_err(), "{id} {bad}");
            assert!(s.enabled(id, &bad).is_err(), "{id} {bad}");
        }
    }
    assert!(s.enabled("run.changed", &json!({"canvas": C})).is_ok());
}

/// Text 1 → generator 2 (has a result) → free captioning action 5 (fresh result) → generator 6
/// (no result). With `stale_head`, 2's result no longer matches its inputs.
fn through_an_action(stale_head: bool) -> (Session, Started) {
    let mut kinds = kinds();
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.caption".into(),
            label: "Caption".into(),
            category: Category::Action,
            inputs: vec![Port { name: "in".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            params: ParamSchema::default(),
            quick_add: None,
        })))
        .unwrap();
    let mut p = Project::new(ProjectId::from_bytes([2; 16]), "Run").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut p).unwrap();
    let priced: nodecraft_types::ParamValues = [("model".to_owned(), V::Text("test.img".into()))].into_iter().collect();
    let source =
        kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".to_owned(), V::Text("a fox".into()))].into_iter().collect()).unwrap();
    let head = kinds.get("test.gen").unwrap().descriptor().instantiate(id(2), &priced).unwrap();
    let action = kinds.get("test.caption").unwrap().descriptor().instantiate(id(5), &Default::default()).unwrap();
    let tail = kinds.get("test.gen").unwrap().descriptor().instantiate(id(6), &priced).unwrap();
    let wire = |e: u8, from: u8, to: u8| Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([e; 16]),
            from: Endpoint { node: id(from), port: "out".into() },
            to: Endpoint { node: id(to), port: "in".into() },
            order: 0,
        },
    };
    nodecraft_graph::Changeset::new(vec![
        Change::AddNode { canvas: C, node: source },
        Change::AddNode { canvas: C, node: head },
        Change::AddNode { canvas: C, node: action },
        Change::AddNode { canvas: C, node: tail },
        wire(10, 1, 2),
        wire(11, 2, 5),
        wire(12, 5, 6),
    ])
    .apply(&mut p)
    .unwrap();
    // Results made from what is wired now, so they are fresh; then 2's is made stale on request.
    for (n, output) in [(2u8, "a fox, written"), (5, "a fox, written twice")] {
        let canvas = p.canvases().get(&C).unwrap();
        let hash = nodecraft_exec::staleness::input_hash(canvas, &kinds, id(n)).unwrap();
        let mut node = canvas.nodes().get(&id(n)).unwrap().clone();
        let generation = GenerationId::from_bytes([n; 16]);
        let stale = stale_head && n == 2;
        node.history.push(Generation {
            id: generation,
            outputs: vec![V::Text(output.into())],
            input_hash: Some(if stale { nodecraft_types::Blake3::from_bytes([0; 32]) } else { hash }),
            items: Vec::new(),
            settings: Vec::new(),
            source_versions: Vec::new(),
            incomplete: None,
        });
        node.active = Some(generation);
        Change::SetNode { canvas: C, node }.apply(&mut p).unwrap();
    }
    session_with(p, kinds, Budgets::default(), true)
}

#[test]
fn a_fresh_action_between_two_generators_runs_between_them_in_both_modes() {
    for (id_, stale_head) in [("run.workflow", false), ("run.changed", true)] {
        let (mut s, started) = through_an_action(stale_head);
        let stale = nodecraft_exec::staleness::stale(s.document().canvases().get(&C).unwrap(), s.kinds()).unwrap();
        // The workflow regenerates a fresh 2, so fresh 5 has to run between it and 6. A stale 2
        // makes 5 stale too (staleness reaches downstream), which a pending run covers already.
        assert_eq!(stale.contains(&id(5)), stale_head);
        let quote = s.dispatch(id_, &json!({"canvas": C})).unwrap();
        assert_eq!(steps(&quote), vec![json!(id(2)), json!(id(5)), json!(id(6))], "{id_}: {quote}");
        // 6's input is made during the run, so its price is only partly known now.
        let accept = json!({"token": quote["token"], "amounts": {"USD": 1_000_000}, "partial": true, "unpriced_jobs": 16});
        s.dispatch("run.start", &json!({"canvas": C, "mode": quote["mode"], "accept": accept})).unwrap();
        let requests = std::mem::take(&mut *started.lock().unwrap());
        let run = &requests[0];
        assert_eq!(run.quote().steps.iter().map(|step| step.node).collect::<Vec<_>>(), vec![id(2), id(5), id(6)]);
    }
}
