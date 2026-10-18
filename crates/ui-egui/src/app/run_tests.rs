//! Stale badges and refreshes (#211), driven through real pointer events.
use super::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx};
use nodecraft_types::{Modality, PortType};

const C: CanvasId = CanvasId::from_bytes([1; 16]);

/// Text in, text out: the input upper-cased, as a local job.
struct Shout(NodeDescriptor);
impl NodeKind for Shout {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("shout").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let text = match ctx.inputs.first().map(|i| &i.value) {
            Some(Value::Text(text)) => text.to_uppercase(),
            _ => String::new(),
        };
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text(text) }])
    }
}

const CATALOG: &str = r#"
[[model]]
id = "test.img"
provider = "test"
provider_model = "img-v1"
label = "Test Image"
description = "Priced per image."
latency_s = 10
quality = 4
pricing = { type = "per_unit", unit = "image", price = "0.04 USD", default_count = 1 }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

/// Text 1 → `kind` 2, with node 2's params.
fn harness(kind: &str, category: Category, params: Vec<ParamSpec>, values: &[(&str, Value)]) -> Harness<'static, NodeCraftApp> {
    let text = PortType { modality: Modality::Text, many: false };
    let descriptor = NodeDescriptor {
        id: kind.into(),
        label: "Shout".into(),
        category,
        inputs: vec![nodecraft_graph::Port { name: "in".into(), port_type: text }],
        outputs: vec![nodecraft_graph::Port { name: "out".into(), port_type: text }],
        params: ParamSchema { params },
        quick_add: None,
    };
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(Shout(descriptor))).unwrap();
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Refresh").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut document).unwrap();
    let mut source =
        kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".to_owned(), Value::Text("fox".into()))].into_iter().collect()).unwrap();
    source.pos = nodecraft_graph::Position { x: -700.0, y: -60.0 };
    let mut shout = kinds.get(kind).unwrap().descriptor().instantiate(id(2), &values.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()).unwrap();
    shout.pos = nodecraft_graph::Position { x: -300.0, y: -60.0 };
    let edge = nodecraft_graph::Edge {
        id: EdgeId::from_bytes([9; 16]),
        from: Endpoint { node: id(1), port: "out".into() },
        to: Endpoint { node: id(2), port: "in".into() },
        order: 0,
    };
    nodecraft_graph::Changeset::new(vec![
        Change::AddNode { canvas: C, node: source },
        Change::AddNode { canvas: C, node: shout },
        Change::Connect { canvas: C, edge },
    ])
    .apply(&mut document)
    .unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(Vec2::new(1000.0, 700.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, C).unwrap())
}

/// Node 2's active result text, if any.
fn result(h: &Harness<'static, NodeCraftApp>) -> Option<String> {
    let node = h.state().session.document().canvases().get(&C)?.nodes().get(&id(2))?.clone();
    let active = node.active?;
    match node.history.iter().find(|g| g.id == active)?.outputs.first()? {
        Value::Text(text) => Some(text.clone()),
        _ => None,
    }
}

fn set_text(h: &mut Harness<'static, NodeCraftApp>, text: &str) {
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": C, "id": id(1), "name": "text", "value": {"type": "text", "value": text}})).unwrap();
    h.run();
}

#[test]
fn an_upstream_edit_shows_a_stale_badge_and_refreshing_clears_it() {
    let mut h = harness("test.shout", Category::Action, vec![], &[]);
    h.run();
    // A first run (free, so no confirmation) gives node 2 a result.
    h.state_mut().refresh(id(2));
    h.run();
    assert_eq!(result(&h).as_deref(), Some("FOX"), "{:?}", h.state().notice);
    assert!(h.query_by_label("Stale result: refresh").is_none());
    // Editing the prompt makes it stale: the badge appears.
    set_text(&mut h, "owl");
    assert!(h.query_by_label("Stale result: refresh").is_some());
    // Cards are drawn in a world-space layer: find the badge on screen from the painted card.
    let at = {
        let app = h.state();
        let t = app.theme.tokens();
        let card = *app.painted.get(&id(2)).unwrap();
        let world = card.right_top() + Vec2::new(-t.badge_width / 2.0, -t.badge_height / 2.0 - t.label_gap);
        app.view.to_screen(app.viewport.min, world)
    };
    h.hover_at(at);
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert_eq!(
        result(&h).as_deref(),
        Some("OWL"),
        "notice: {:?} selected: {:?} at {at:?} painted {:?}",
        h.state().notice,
        h.state().selection.contains(id(2)),
        h.state().painted.get(&id(2))
    );
    assert!(h.query_by_label("Stale result: refresh").is_none(), "refreshed");
    assert!(h.state().notice.is_none(), "{:?}", h.state().notice);
    // Each run is one undo step.
    h.state_mut().session.dispatch("edit.undo", &json!(null)).unwrap();
    h.run();
    assert_eq!(result(&h).as_deref(), Some("FOX"));
}

#[test]
fn a_paid_refresh_asks_first_and_cancel_runs_nothing() {
    let model = ParamSpec {
        name: "model".into(),
        label: "Model".into(),
        description: None,
        control: ParamControl::Model,
        required: false,
        default: None,
        visible_if: None,
    };
    let mut h = harness("test.paid", Category::Generation, vec![model], &[("model", Value::Text("test.img".into()))]);
    h.state_mut().set_run_catalog(nodecraft_catalog::Catalog::from_toml(CATALOG).unwrap()).unwrap();
    h.run();
    h.state_mut().refresh(id(2));
    h.run();
    assert!(h.state().run.confirm.as_ref().is_some_and(|c| c.price == "0.04 USD"), "{:?}", h.state().notice);
    assert!(h.query_by_label_contains("This refresh costs 0.04 USD").is_some());
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.run();
    assert!(h.state().run.confirm.is_none());
    assert_eq!(result(&h), None, "cancelled: nothing ran");
    h.state_mut().refresh(id(2));
    h.run();
    h.get_by_role_and_label(Role::Button, "Run").click();
    h.run();
    assert_eq!(result(&h).as_deref(), Some("FOX"), "{:?}", h.state().notice);
}

#[test]
fn an_edit_while_confirming_closes_the_confirmation() {
    let model = ParamSpec {
        name: "model".into(),
        label: "Model".into(),
        description: None,
        control: ParamControl::Model,
        required: false,
        default: None,
        visible_if: None,
    };
    let mut h = harness("test.paid", Category::Generation, vec![model], &[("model", Value::Text("test.img".into()))]);
    h.state_mut().set_run_catalog(nodecraft_catalog::Catalog::from_toml(CATALOG).unwrap()).unwrap();
    h.run();
    h.state_mut().refresh(id(2));
    h.run();
    assert!(h.state().run.confirm.is_some());
    // The prompt changes while the confirmation is open: its price no longer holds.
    set_text(&mut h, "owl");
    assert!(h.state().run.confirm.is_none());
    assert!(h.query_by_label("Run").is_none());
    assert_eq!(h.state().notice.as_deref(), Some("The canvas changed; refresh again."));
    assert_eq!(result(&h), None);
}

#[test]
fn the_host_refuses_a_run_whose_token_no_longer_matches() {
    let mut h = harness("test.shout", Category::Action, vec![], &[]);
    h.run();
    let request = json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]});
    let quote = h.state_mut().session.dispatch("run.quote", &request).unwrap();
    // The run was started at this quote, then the document changed before the host ran it.
    set_text(&mut h, "owl");
    let started = json!({"run": request, "token": quote["token"], "accept": {"amounts": {}, "partial": false}});
    let error = h.state_mut().execute(&started).unwrap_err();
    assert!(error.message.contains("changed before the run started"), "{}", error.message);
    assert_eq!(result(&h), None);
}

#[test]
fn a_run_started_from_anywhere_is_run_by_the_app() {
    // As the control channel or an agent would: quote, then start, without the badge.
    let mut h = harness("test.shout", Category::Action, vec![], &[]);
    h.run();
    let request = json!({"canvas": C, "mode": "with_upstream", "nodes": [id(2)]});
    let quote = h.state_mut().session.dispatch("run.quote", &request).unwrap();
    let mut start = request.clone();
    start["accept"] = json!({"token": quote["token"], "amounts": {}, "partial": false});
    assert_eq!(h.state_mut().session.dispatch("run.start", &start).unwrap(), json!({"accepted": true}));
    h.run();
    assert_eq!(result(&h).as_deref(), Some("FOX"), "{:?}", h.state().notice);
    // And the next run isn't blocked by a leftover request.
    set_text(&mut h, "owl");
    h.state_mut().refresh(id(2));
    h.run();
    assert_eq!(result(&h).as_deref(), Some("OWL"), "{:?}", h.state().notice);
}

/// Text in, text out, through a model call.
struct Calls(NodeDescriptor);
impl NodeKind for Calls {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("calls").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let outputs = vec![nodecraft_nodes::GenerateOutput { port: "out".into(), modality: Modality::Text }];
        Ok(vec![JobSpec::Generate { node: ctx.node.id, outputs, provider: "fal".into(), model: "test.img".into(), params: Default::default(), inputs: vec![] }])
    }
}

#[test]
fn a_refresh_that_needs_a_provider_says_so_instead_of_asking_for_money() {
    let text = PortType { modality: Modality::Text, many: false };
    let model = ParamSpec {
        name: "model".into(),
        label: "Model".into(),
        description: None,
        control: ParamControl::Model,
        required: false,
        default: None,
        visible_if: None,
    };
    let descriptor = NodeDescriptor {
        id: "test.calls".into(),
        label: "Calls".into(),
        category: Category::Generation,
        inputs: vec![nodecraft_graph::Port { name: "in".into(), port_type: text }],
        outputs: vec![nodecraft_graph::Port { name: "out".into(), port_type: text }],
        params: ParamSchema { params: vec![model] },
        quick_add: None,
    };
    let mut kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.register(Box::new(Calls(descriptor))).unwrap();
    let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Calls").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut document).unwrap();
    let source =
        kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &[("text".to_owned(), Value::Text("fox".into()))].into_iter().collect()).unwrap();
    let calls = kinds
        .get("test.calls")
        .unwrap()
        .descriptor()
        .instantiate(id(2), &[("model".to_owned(), Value::Text("test.img".into()))].into_iter().collect())
        .unwrap();
    let edge = nodecraft_graph::Edge {
        id: EdgeId::from_bytes([9; 16]),
        from: Endpoint { node: id(1), port: "out".into() },
        to: Endpoint { node: id(2), port: "in".into() },
        order: 0,
    };
    nodecraft_graph::Changeset::new(vec![
        Change::AddNode { canvas: C, node: source },
        Change::AddNode { canvas: C, node: calls },
        Change::Connect { canvas: C, edge },
    ])
    .apply(&mut document)
    .unwrap();
    let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.set_run_catalog(nodecraft_catalog::Catalog::from_toml(CATALOG).unwrap()).unwrap();
    app.refresh(id(2));
    assert!(app.run.confirm.is_none(), "no confirmation for a run that can't happen");
    assert_eq!(app.notice.as_deref(), Some("This refresh calls a model, and no fal provider is set up yet."));
}
