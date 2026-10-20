//! Run planning consults the option lists the host has fetched (#162). A catalog model whose
//! parameter draws from a run-time list (`fixture.checkpoints` and, scoped by it, a LoRA list) is
//! quoted and started through the real commands: a value off a *known* list blocks the run before
//! a job is admitted, priced or charged, while an unfetched list, a disabled parameter and a scope
//! with no value still send the value on for the provider to judge. Mock provider only, no live
//! calls. The edit, bulk-edit and describe halves of the same contract are in `option_sources162`.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_routing::ProviderRouting,
    run_cmds::{BudgetState, PreparedRun, RunCatalog},
};
use nodecraft_exec::{cost::Budgets, run::RunOptions};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_nodes::{JobSpec, generation::TEXT_KIND};
use nodecraft_providers::mock::MockProvider;
use nodecraft_types::{
    CanvasId, Choice, Condition, Contract, NodeId, OptionKey, OptionLists, OptionSource, ParamControl, ParamOwner, ParamRule, ParamSpec, ProjectId, Value,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

const C: CanvasId = CanvasId::from_bytes([162; 16]);
const NODE: NodeId = NodeId::from_bytes([163; 16]);
const LOCAL: NodeId = NodeId::from_bytes([164; 16]);
const DOWNSTREAM: NodeId = NodeId::from_bytes([167; 16]);
/// A title no report may repeat: blocked reports are fixed UUID/code/text records.
const TITLE: &str = "Stale Lighthouse Draft";
const OFF_LIST: &str = "removed.safetensors";
const MODEL: &str = "fixture/listed";
const CHECKPOINTS: &str = "fixture.checkpoints";
const LORAS: &str = "fixture.loras";
const PRICE: u64 = 10_000;

/// `mode` enables `checkpoint`; `checkpoint` must come from a fetched list and `lora` from the
/// list its value selects. A paid mock model, so a run that is admitted has a price to lose.
fn model() -> nodecraft_catalog::ModelDescriptor {
    let catalog = Catalog::from_toml(&format!(
        r#"[[model]]
id = "{MODEL}"
provider = "mock"
provider_model = "mock-text"
label = "Fixture listed"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = {{ type = "per_call", price = "0.01 USD" }}
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}]
"#
    ))
    .expect("the fixture model parses");
    let mut model = catalog.get(MODEL).expect("the fixture model").clone();
    let text = |name: &str, label: &str| ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Text { multiline: false, max_bytes: 128 },
        required: false,
        default: None,
        visible_if: None,
    };
    model.params.params.insert(
        0,
        ParamSpec {
            name: "mode".into(),
            label: "Mode".into(),
            description: None,
            control: ParamControl::Choice { options: ["plain", "fine"].iter().map(|v| Choice { value: (*v).into(), label: (*v).into() }).collect() },
            required: false,
            default: None,
            visible_if: None,
        },
    );
    model.params.params.insert(1, text("checkpoint", "Checkpoint"));
    model.params.params.push(text("lora", "LoRA"));
    // `checkpoint` is editable only while `mode` is fine, and draws on a list of its own; `lora`
    // draws on the list `checkpoint` selects.
    let mut contract = Contract::from_schema(MODEL, ParamOwner::Model, model.params.clone());
    let fine = Condition::Equals { param: "mode".into(), value: Value::Text("fine".into()) };
    contract.rules.push(ParamRule {
        options: Some(OptionSource { id: CHECKPOINTS.into(), scope: None }),
        enabled_when: Some(fine),
        ..ParamRule::implied("checkpoint")
    });
    contract.rules.push(ParamRule { options: Some(OptionSource { id: LORAS.into(), scope: Some("checkpoint".into()) }), ..ParamRule::implied("lora") });
    model.parameter_contract = Some(contract);
    model
}

/// What the host has fetched. `scoped` lists are narrowed by a checkpoint value; the same source
/// id also has an unscoped list, which a scoped parameter is never checked against.
fn lists(checkpoints: &[&str], scoped: &[(&str, &[&str])]) -> OptionLists {
    let mut lists = OptionLists::default();
    let choices = |values: &[&str]| values.iter().map(|v| Choice { value: (*v).into(), label: (*v).into() }).collect();
    lists.set(OptionKey { owner: MODEL, source: CHECKPOINTS, scope: None }, choices(checkpoints)).expect("a bounded fixture list");
    for (scope, values) in scoped {
        lists.set(OptionKey { owner: MODEL, source: LORAS, scope: Some(scope) }, choices(values)).expect("a bounded fixture list");
    }
    lists.set(OptionKey { owner: MODEL, source: LORAS, scope: None }, choices(&["unscoped.safetensors"])).expect("a bounded fixture list");
    lists
}

struct Fixture {
    session: Session,
    /// What the host's run service was handed: one entry means a run was admitted.
    admitted: Arc<Mutex<Vec<PreparedRun>>>,
}

/// A session whose host has fetched nothing yet, so every value can be set while all lists are
/// unknown. A caller then installs a list with `swap_resource`, the way a refresh does.
fn fixture(values: &[(&str, &str)]) -> Fixture {
    let catalog = Catalog::from_models([model()]).expect("the fixture catalog");
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).expect("run catalog")).expect("generation catalog");
    let kinds = snapshot.registry().expect("kinds");
    let mut project = Project::new(ProjectId::from_bytes([165; 16]), "Option lists at planning").expect("project");
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).expect("canvas");
    let supplied = std::iter::once(("model".to_owned(), Value::Text(MODEL.into())))
        .chain(std::iter::once(("prompt".to_owned(), Value::Text("A lighthouse".into()))))
        .chain(values.iter().map(|(name, value)| ((*name).to_owned(), Value::Text((*value).to_owned()))))
        .collect();
    let mut node = kinds.get(TEXT_KIND).expect("the text kind").instantiate_instance(NODE, Default::default(), &supplied).expect("node");
    node.title = TITLE.into();
    Change::AddNode { canvas: C, node }.apply(&mut project).expect("node");
    // A second card wired downstream of the first, clean of its own, so a run that only asks for
    // it still has to plan the off-list one upstream.
    let downstream = kinds
        .get(TEXT_KIND)
        .expect("the text kind")
        .instantiate_instance(
            DOWNSTREAM,
            Default::default(),
            &[("model".into(), Value::Text(MODEL.into())), ("prompt".into(), Value::Text("A shoal".into()))].into(),
        )
        .expect("downstream node");
    Change::AddNode { canvas: C, node: downstream }.apply(&mut project).expect("downstream node");
    let edge = nodecraft_graph::Edge {
        id: nodecraft_types::EdgeId::from_bytes([168; 16]),
        from: nodecraft_graph::Endpoint { node: NODE, port: "out".into() },
        to: nodecraft_graph::Endpoint { node: DOWNSTREAM, port: "prompt".into() },
        order: 0,
    };
    Change::Connect { canvas: C, edge }.apply(&mut project).expect("edge");
    let local = kinds
        .get("source.text")
        .expect("the text source")
        .instantiate_instance(LOCAL, Default::default(), &[("text".into(), Value::Text("A lighthouse".into()))].into())
        .expect("source node");
    Change::AddNode { canvas: C, node: local }.apply(&mut project).expect("source node");
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    resources.insert(BudgetState { budgets: Budgets::default(), spent: Default::default() });
    resources.insert(OptionLists::default());
    let admitted: Arc<Mutex<Vec<PreparedRun>>> = Arc::default();
    let sink = admitted.clone();
    let mut services = Services::default();
    services
        .register_run_host(move |prepared| {
            sink.lock().expect("the capture lock").push(prepared);
            Ok(json!({"accepted": true}))
        })
        .expect("a run host");
    let mut session =
        Session::with_registries(project, Registry::builtins().expect("commands"), kinds).expect("session").with_resources(resources).with_services(services);
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(MockProvider::standard())).expect("the mock binds");
    for id in [NODE, DOWNSTREAM] {
        routing.select(id, "mock", binding.target()).expect("the mock is selected");
    }
    session.set_provider_routing(routing).expect("routing");
    Fixture { session, admitted }
}

impl Fixture {
    /// The host refreshes its lists: what was unknown becomes known, and vice versa.
    fn fetch(&mut self, checkpoints: &[&str], scoped: &[(&str, &[&str])]) {
        self.session.swap_resource(Some(lists(checkpoints, scoped)));
    }

    fn quote(&mut self, nodes: &[NodeId]) -> serde_json::Value {
        self.quote_mode("selected", nodes)
    }

    fn quote_mode(&mut self, mode: &str, nodes: &[NodeId]) -> serde_json::Value {
        let request = json!({"canvas": C, "mode": mode, "nodes": nodes});
        self.session.dispatch("run.quote", &request).expect("a quote")
    }

    /// The blocked report as one string, for leak assertions.
    fn blocked_text(quote: &serde_json::Value) -> String {
        quote["blocked"].as_array().expect("blocked").iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(" ")
    }

    /// Quote, accept and start; the error is what a blocked run reports instead.
    fn start(&mut self, nodes: &[NodeId]) -> Result<serde_json::Value, nodecraft_types::Error> {
        let quote = self.quote(nodes);
        let mut request = json!({"canvas": C, "mode": "selected", "nodes": nodes});
        request["accept"] = json!({"token": quote["token"], "partial": false, "unpriced_jobs": 0, "amounts": {"USD": PRICE}});
        self.session.dispatch("run.start", &request)
    }

    fn set(&mut self, name: &str, value: Option<&str>) -> Result<serde_json::Value, nodecraft_types::Error> {
        let value = value.map(|v| json!({"type": "text", "value": v})).unwrap_or(serde_json::Value::Null);
        self.session.dispatch("node.param_set", &json!({"canvas": C, "id": NODE, "name": name, "value": value}))
    }

    fn nothing_admitted(&self) {
        assert!(self.admitted.lock().expect("the capture lock").is_empty(), "a blocked run admits no job and charges nothing");
    }

    /// Nothing has been reserved against the budget: a refused run spends neither the session's
    /// nor the project's total.
    fn spent_is_empty(&mut self) {
        let state = self.session.swap_resource::<BudgetState>(None).expect("the budget state");
        assert!(state.spent.session.is_empty() && state.spent.project.is_empty(), "a refused run reserved nothing: {:?}", state.spent);
        self.session.swap_resource(Some(state));
    }

    /// A blocked report is a fixed UUID/code/static-text record: no value, parameter, model or
    /// title from the document or a contract leaks into it.
    fn assert_leaks_nothing(text: &str, where_: &str) {
        for secret in [OFF_LIST, "checkpoint", "lora", MODEL, TITLE] {
            assert!(!text.contains(secret), "{where_} leaked {secret:?}: {text}");
        }
    }

    /// The parameters the admitted job would send to the provider.
    fn sent_params(&mut self) -> Vec<String> {
        let admitted = self.admitted.lock().expect("the capture lock").pop();
        let prepared = admitted.expect("an admitted run");
        let (run, progress) = self
            .session
            .authorize_prepared_run(prepared, RunOptions { id: [166; 16], first_job: 1, limits: Default::default(), retry: Default::default() })
            .expect("authorize");
        let spec = progress.jobs.first().expect("one job");
        match run.work(spec.id).expect("the job's spec") {
            JobSpec::Generate { params, .. } => params.keys().cloned().collect(),
            other => panic!("expected a generate job, got {other:?}"),
        }
    }
}

#[test]
fn a_value_off_a_fetched_list_blocks_the_run_before_anything_is_admitted() {
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    // The checkpoint was deleted from the server: the next refresh no longer lists it.
    f.fetch(&["good.safetensors"], &[]);
    let quote = f.quote(&[NODE]);
    assert_eq!(quote["allowed"], false, "{quote}");
    assert!(quote["steps"].as_array().expect("steps").is_empty(), "no step is priced: {quote}");
    let blocked = Fixture::blocked_text(&quote);
    assert!(blocked.contains(&format!("node {NODE} [not_an_option]")), "{blocked}");
    Fixture::assert_leaks_nothing(&blocked, "run.quote's blocked report");
    f.nothing_admitted();
    f.spent_is_empty();
    // The node stays editable around the stale value, and is fixed by picking a listed one.
    f.set("checkpoint", Some("good.safetensors")).expect("a listed value is accepted");
    assert_eq!(f.quote(&[NODE])["allowed"], true, "an on-list value runs");
}

#[test]
fn a_run_quoted_while_the_list_was_unknown_is_refused_at_start_once_it_is_known() {
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    let quote = f.quote(&[NODE]);
    assert_eq!(quote["allowed"], true, "an unfetched list refuses nothing: {quote}");
    assert_eq!(quote["totals"]["USD"]["micros"], PRICE);
    // The host fetches the list between the quote and the start; the value it names is gone.
    f.fetch(&["good.safetensors"], &[]);
    let refused = f.start(&[NODE]).expect_err("the run must be refused");
    assert!(refused.message.contains("[not_an_option]"), "{}", refused.message);
    Fixture::assert_leaks_nothing(&refused.message, "run.start's error");
    f.nothing_admitted();
    f.spent_is_empty();
    // And the budget was not consumed by the attempt: the same run goes through at full price
    // once the value is on the list again.
    f.set("checkpoint", Some("good.safetensors")).expect("a listed value");
    assert_eq!(f.start(&[NODE]).expect("the fixed run starts"), json!({"accepted": true}));
    assert_eq!(f.admitted.lock().expect("the capture lock").len(), 1, "one run admitted, no earlier phantom");
}

#[test]
fn an_unfetched_list_sends_the_value_on_for_the_provider_to_judge() {
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    assert_eq!(f.start(&[NODE]).expect("a run with nothing fetched"), json!({"accepted": true}));
    let params = f.sent_params();
    assert!(params.contains(&"checkpoint".to_owned()), "the provider still sees the value: {params:?}");
}

#[test]
fn a_disabled_parameter_draws_from_no_list() {
    // `checkpoint` is only editable while `mode` is fine; its stale value stays on the node.
    let mut f = fixture(&[("mode", "plain"), ("checkpoint", OFF_LIST)]);
    f.fetch(&["good.safetensors"], &[]);
    assert_eq!(f.quote(&[NODE])["allowed"], true, "a disabled value is not checked against a list");
    assert_eq!(f.start(&[NODE]).expect("a run whose stale value is disabled"), json!({"accepted": true}));
    let params = f.sent_params();
    assert!(!params.contains(&"checkpoint".to_owned()), "and it is never submitted: {params:?}");
}

#[test]
fn a_scoped_list_is_the_one_its_scope_value_selects() {
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", "good.safetensors"), ("lora", "unscoped.safetensors")]);
    // `good.safetensors` has one fetched LoRA: the unscoped list of the same id is not its list.
    f.fetch(&["good.safetensors"], &[("good.safetensors", &["ink.safetensors"])]);
    let blocked = f.quote(&[NODE]);
    assert_eq!(blocked["allowed"], false, "{blocked}");
    let text = Fixture::blocked_text(&blocked);
    assert!(text.contains("[not_an_option]"), "{text}");
    Fixture::assert_leaks_nothing(&text, "the scoped blocked report");
    f.nothing_admitted();
    f.set("lora", Some("ink.safetensors")).expect("a LoRA on the scoped list");
    assert_eq!(f.quote(&[NODE])["allowed"], true, "the scope-selected list is the one that applies");
    // Without a checkpoint there is no scope value, so nothing is refused for its LoRA: the
    // unscoped list of that source id is not offered, and an unknown list blocks nothing.
    f.set("checkpoint", None).expect("a stale value can be cleared");
    f.set("lora", Some("anything.safetensors")).expect("no scope value, so no list to be off");
    assert_eq!(f.quote(&[NODE])["allowed"], true, "a scope with no value refuses nothing");
}

#[test]
fn a_local_job_is_unaffected_by_the_lists() {
    // A source node's work is local and free: nothing about it names a list, so a host carrying
    // fetched lists plans, quotes and admits it exactly as it did before.
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    f.fetch(&["good.safetensors"], &[]);
    let quote = f.quote(&[LOCAL]);
    assert_eq!(quote["allowed"], true, "a local job still runs: {quote}");
    assert_eq!(quote["steps_total"], 1, "the local step is planned as it always was: {quote}");
    assert_eq!(quote["totals"]["USD"]["micros"].as_u64().unwrap_or(0), 0, "a source job costs nothing: {}", quote["totals"]);
    assert_eq!(f.start(&[LOCAL]).expect("the local job starts"), json!({"accepted": true}));
}

#[test]
fn one_off_list_upstream_card_stops_a_run_that_only_asked_for_the_card_downstream_of_it() {
    // Only DOWNSTREAM is requested, with_upstream; it is clean itself, but the run has to plan
    // NODE to feed it, and NODE is off a known list. The whole run is refused, not the upstream
    // card quietly dropped.
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    f.fetch(&["good.safetensors"], &[]);
    let quote = f.quote_mode("with_upstream", &[DOWNSTREAM]);
    assert_eq!(quote["allowed"], false, "{quote}");
    assert!(quote["steps"].as_array().expect("steps").is_empty(), "no step is priced: {quote}");
    let blocked = Fixture::blocked_text(&quote);
    assert!(blocked.contains(&format!("node {NODE} [not_an_option]")), "the upstream node is named: {blocked}");
    Fixture::assert_leaks_nothing(&blocked, "the with_upstream blocked report");
    f.nothing_admitted();
    f.spent_is_empty();
    // Fix the upstream value and the same request runs both cards, at both their prices.
    f.set("checkpoint", Some("good.safetensors")).expect("a listed value");
    let fixed = f.quote_mode("with_upstream", &[DOWNSTREAM]);
    assert_eq!(fixed["allowed"], true, "{fixed}");
    assert_eq!(fixed["steps_total"], 2, "upstream and downstream both run: {fixed}");
    assert_eq!(fixed["totals"]["USD"]["micros"], PRICE * 2, "and both are priced: {fixed}");
}

#[test]
fn a_technique_node_checks_the_lists_for_the_nodes_it_inlines() {
    const VERSION: CanvasId = CanvasId::from_bytes([169; 16]);
    const PLACED: NodeId = NodeId::from_bytes([170; 16]);
    let mut f = fixture(&[("mode", "fine"), ("checkpoint", OFF_LIST)]);
    // Published while the list is unknown, so the version keeps the value as it was.
    let publish = json!({"canvas": C, "published": VERSION, "seed": NodeId::from_bytes([171; 16]), "technique": nodecraft_types::TechniqueId::from_bytes([172; 16]),
        "name": "Lighthouse", "inputs": [], "outputs": [{"node": NODE, "name": "Answer"}]});
    f.session.dispatch("technique.publish", &publish).expect("publish");
    f.session.dispatch("technique.place", &json!({"canvas": C, "version": VERSION, "id": PLACED, "pos": {"x": 400.0, "y": 0.0}})).expect("place");
    let canvas = f.session.document().canvases().get(&C).expect("the canvas");
    let expanded = nodecraft_exec::technique::expand(f.session.document().canvases(), canvas, f.session.kinds()).expect("expand");
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(MockProvider::standard())).expect("the mock binds");
    for id in expanded.inner.keys().copied().chain([NODE, DOWNSTREAM]) {
        routing.select(id, "mock", binding.target()).expect("the mock is selected");
    }
    f.session.set_provider_routing(routing).expect("routing");
    // Unknown list: the technique node plans, priced like the card it inlines.
    let quote = f.quote(&[PLACED]);
    assert_eq!(quote["allowed"], true, "{quote}");
    f.fetch(&["good.safetensors"], &[]);
    let quote = f.quote(&[PLACED]);
    assert_eq!(quote["allowed"], false, "{quote}");
    let blocked = Fixture::blocked_text(&quote);
    // Reported on the technique node, the one the person can see, with the same code.
    assert!(blocked.contains(&format!("node {PLACED} [not_an_option]")), "{blocked}");
    Fixture::assert_leaks_nothing(&blocked, "a technique node's blocked report");
    f.nothing_admitted();
}
