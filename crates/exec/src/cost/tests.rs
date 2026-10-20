use super::*;
use crate::planner::{Planned, RunMode, plan};
use nodecraft_graph::{Change, Edge, Endpoint, Port, Position, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
use nodecraft_types::Value;
use nodecraft_types::{CanvasId, EdgeId, Modality, ParamControl, ParamSchema, ParamSpec, PortType, ProjectId, Unsigned};
use std::collections::{BTreeMap, BTreeSet};

const C: CanvasId = CanvasId::from_bytes([1; 16]);

const CATALOG: &str = r#"
[[model]]
id = "test.img"
provider = "test"
provider_model = "img-v1"
label = "Test Image"
description = "Priced per image."
latency_s = 10
quality = 4
pricing = { type = "per_unit", unit = "image", price = "0.04 USD", count = "num_images" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "num_images"
label = "Images"
control = { type = "unsigned", min = "1", max = "4" }
default = { type = "unsigned", value = "1" }

[[model]]
id = "test.chat"
provider = "test"
provider_model = "chat-v1"
label = "Test Chat"
description = "Priced by tokens."
latency_s = 2
quality = 3
pricing = { type = "per_unit", unit = "thousand_tokens", price = "0.002 USD" }
[[model.modes]]
id = "chat"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "test.free"
provider = "test"
provider_model = "free-v1"
label = "Free"
description = "Local and free."
latency_s = 5
quality = 2
pricing = { type = "free" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "test.eur"
provider = "test"
provider_model = "eur-v1"
label = "Euro"
description = "Priced in euros."
latency_s = 5
quality = 3
pricing = { type = "per_call", price = "0.05 EUR" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "test.rewrite"
provider = "test"
provider_model = "rewrite-v1"
label = "Rewrite"
description = "A prompt rewriter priced per call."
latency_s = 1
quality = 3
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "chat"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A generative stand-in with a model picker and an image count.
struct Gen(NodeDescriptor);
impl NodeKind for Gen {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        (!inputs.is_empty()).then(|| ModeId::new("gen")).and_then(Result::ok)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value: Value::Text("generated".into()) }])
    }
}

fn kinds() -> Registry {
    let mut kinds = Registry::builtins().unwrap();
    let spec =
        |name: &str, control| ParamSpec { name: name.into(), label: name.into(), description: None, control, required: false, default: None, visible_if: None };
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.gen".into(),
            label: "Gen".into(),
            category: Category::Generation,
            inputs: vec![Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            params: ParamSchema {
                params: vec![
                    spec("model", ParamControl::Model),
                    spec("num_images", ParamControl::Unsigned { min: Some(Unsigned(1)), max: Some(Unsigned(4)), step: Unsigned(1) }),
                ],
            },
            quick_add: None,
        })))
        .unwrap();
    // A node with two model pickers: a prompt rewriter and an image model.
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.two".into(),
            label: "Two".into(),
            category: Category::Generation,
            inputs: vec![Port { name: "in".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Text, many: false } }],
            params: ParamSchema { params: vec![spec("rewriter", ParamControl::Model), spec("model", ParamControl::Model)] },
            quick_add: None,
        })))
        .unwrap();
    kinds
}
fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

/// Text 1 feeds generators 2, 3, 4, 5 with different model settings.
fn setup() -> (Project, RunPlan) {
    let kinds = kinds();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Cost").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut values = BTreeMap::new();
    values.insert("text".to_owned(), Value::Text("a fox".into()));
    let text = kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &values).unwrap();
    Change::AddNode { canvas: C, node: text }.apply(&mut project).unwrap();
    let models: [(u8, Option<&str>, Option<u64>); 4] =
        [(2, Some("test.img"), Some(3)), (3, None, None), (4, Some("gone.model"), None), (5, Some("test.chat"), None)];
    for (n, model, count) in models {
        let mut params = BTreeMap::new();
        if let Some(model) = model {
            params.insert("model".to_owned(), Value::Text(model.into()));
        }
        if let Some(count) = count {
            params.insert("num_images".to_owned(), Value::Unsigned(Unsigned(count)));
        }
        let mut node = kinds.get("test.gen").unwrap().descriptor().instantiate(id(n), &params).unwrap();
        node.pos = Position { x: f32::from(n) * 400.0, y: 0.0 };
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([n + 100; 16]),
            from: Endpoint { node: id(1), port: "out".into() },
            to: Endpoint { node: id(n), port: "in".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let canvas = project.canvases().get(&C).unwrap();
    let requested: BTreeSet<NodeId> = [1, 2, 3, 4, 5].into_iter().map(id).collect();
    let Planned::Ready(plan) = plan(canvas, &kinds, &RunMode::Selected, &requested, &BTreeSet::new()).unwrap() else { panic!("blocked") };
    (project, plan)
}
fn usd(micros: u64) -> Money {
    Money::new(Currency::USD, micros)
}
fn catalog() -> Catalog {
    Catalog::from_toml(CATALOG).unwrap()
}

#[test]
fn quotes_price_steps_from_their_models_and_say_why_others_are_unknown() {
    let (project, plan) = setup();
    let quote = quote(&plan, project.canvases().get(&C).unwrap(), &kinds(), &catalog()).unwrap();
    let price = |n: u8| quote.steps.iter().find(|s| s.node == id(n)).unwrap().total();
    assert_eq!(price(1), Ok(None)); // the text source is free
    assert_eq!(price(2), Ok(Some(usd(120_000)))); // 3 images × 0.04
    assert_eq!(price(3), Err(Unpriced::NoModel));
    assert_eq!(price(4), Err(Unpriced::UnknownModel("gone.model".into())));
    assert_eq!(price(5), Err(Unpriced::ByUsage("test.chat".into())));
    assert_eq!(quote.totals, [(Currency::USD, usd(120_000))].into_iter().collect());
    assert!(!quote.is_complete());
    assert_eq!(quote.unpriced().count(), 3);
}

#[test]
fn budgets_block_overruns_unknown_prices_and_other_currencies() {
    let (project, plan) = setup();
    let canvas = project.canvases().get(&C).unwrap();
    let full = quote(&plan, canvas, &kinds(), &catalog()).unwrap();
    // Only the priced steps, for the arithmetic cases.
    let priced = Quote { steps: full.steps.iter().filter(|s| s.per_job.is_ok()).cloned().collect(), totals: full.totals.clone() };
    assert!(priced.is_complete());
    let none = Budgets::default();
    assert!(check(&full, &none, &Spent::default()).unwrap().blocks.is_empty());
    // Per call: 0.12 is over a 0.10 cap.
    let per_call = Budgets { per_call: Some(usd(100_000)), ..Budgets::default() };
    assert_eq!(
        check(&priced, &per_call, &Spent::default()).unwrap().blocks,
        vec![Block::Over { scope: Scope::PerCall, node: Some(id(2)), limit: usd(100_000), would_be: usd(120_000) }]
    );
    // Session: 0.45 already spent plus 0.12 is over 0.50; 0.30 spent is fine.
    let session = Budgets { session: Some(usd(500_000)), ..Budgets::default() };
    let spent = |micros| Spent { session: [(Currency::USD, usd(micros))].into_iter().collect(), project: BTreeMap::new() };
    let blocks = check(&priced, &session, &spent(450_000)).unwrap().blocks;
    assert_eq!(blocks, vec![Block::Over { scope: Scope::Session, node: None, limit: usd(500_000), would_be: usd(570_000) }]);
    assert_eq!(blocks.first().unwrap().message(), "This would exceed the session budget: 0.57 USD of 0.5 USD.");
    assert!(check(&priced, &session, &spent(300_000)).unwrap().blocks.is_empty());
    // Exactly at the cap is allowed.
    assert!(check(&priced, &session, &spent(380_000)).unwrap().blocks.is_empty());
    // A run that costs nothing isn't blocked even when the budget is already used up.
    let free = Quote { steps: priced.steps.iter().filter(|s| s.node == id(1)).cloned().collect(), totals: BTreeMap::new() };
    assert!(check(&free, &session, &spent(600_000)).unwrap().blocks.is_empty());
    // Session and project caps together, each against its own spend.
    let both = Budgets { session: Some(usd(1_000_000)), project: Some(usd(2_000_000)), per_call: None };
    let spent_both = Spent { session: [(Currency::USD, usd(100_000))].into_iter().collect(), project: [(Currency::USD, usd(1_950_000))].into_iter().collect() };
    assert_eq!(
        check(&priced, &both, &spent_both).unwrap().blocks,
        vec![Block::Over { scope: Scope::Project, node: None, limit: usd(2_000_000), would_be: usd(2_070_000) }]
    );
    // Unknown prices block any capped run.
    assert!(check(&full, &session, &Spent::default()).unwrap().blocks.iter().any(|b| matches!(b, Block::Unknown { scope: Scope::Session, .. })));
    // A EUR cap can't be compared with a USD price.
    let euro = Budgets { project: Some(Money::new(Currency::EUR, 10_000_000)), ..Budgets::default() };
    assert!(matches!(check(&priced, &euro, &Spent::default()).unwrap().blocks.as_slice(), [Block::Currency { scope: Scope::Project, .. }]));
}

#[test]
fn the_usage_log_counts_each_charge_once_and_exports_safe_csv() {
    let mut log = UsageLog::default();
    let entry = |key: &str, micros: u64, session: &str, model: &str| UsageEntry {
        instance: None,
        at_ms: 1_700_000_000_000,
        session: session.into(),
        node: id(2),
        provider: "test".into(),
        model: model.into(),
        cost: usd(micros),
        idempotency_key: key.into(),
        status: Charge::Actual,
    };
    assert!(log.record(entry("nodecraft:run:1", 40_000, "now", "test.img")).unwrap());
    // A retry under the same key is the same charge.
    assert!(!log.record(entry("nodecraft:run:1", 40_000, "now", "test.img")).unwrap());
    assert!(log.record(entry("nodecraft:old:1", 1_500_000, "earlier", "=HYPERLINK(\"evil\")")).unwrap());
    // An empty key can't tell retries from new charges.
    assert!(log.record(entry("", 1, "now", "x")).is_err());
    let spent = log.spent("now");
    assert_eq!(spent.session, [(Currency::USD, usd(40_000))].into_iter().collect());
    assert_eq!(spent.project, [(Currency::USD, usd(1_540_000))].into_iter().collect());
    let csv = log.to_csv();
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines.first(), Some(&"time_ms,session,node,provider,model,amount,currency,status,idempotency_key"));
    assert!(lines.get(1).unwrap().contains(",test,test.img,0.04,USD,actual,nodecraft:run:1"));
    // Formula-looking text is neutralised and quoted.
    assert!(lines.get(2).unwrap().contains(",\"'=HYPERLINK(\"\"evil\"\")\",1.5,USD,actual,"));
    assert_eq!(log.entries().count(), 2);
}

#[test]
fn money_formats_without_float_noise() {
    assert_eq!(format_money(usd(0)), "0 USD");
    assert_eq!(format_money(usd(120_000)), "0.12 USD");
    assert_eq!(format_money(usd(1)), "0.000001 USD");
    assert_eq!(format_money(usd(u64::MAX)), "18446744073709.551615 USD");
}

#[test]
fn a_step_costs_its_per_job_price_times_its_jobs() {
    let step = StepQuote { node: id(2), models: vec!["test.img".into()], per_job: Ok(Some(usd(40_000))), jobs: 3, jobs_known: true };
    assert_eq!(step.total(), Ok(Some(usd(120_000))));
    let overflow = StepQuote { jobs: u64::MAX, ..step.clone() };
    assert!(matches!(overflow.total(), Err(Unpriced::Invalid(_))));
    // The per-call cap applies per job, so three 0.04 jobs pass a 0.05 cap.
    let quote = Quote { steps: vec![step], totals: [(Currency::USD, usd(120_000))].into_iter().collect() };
    assert!(check(&quote, &Budgets { per_call: Some(usd(50_000)), ..Budgets::default() }, &Spent::default()).unwrap().blocks.is_empty());
}

#[test]
fn eviction_never_lowers_what_was_spent() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 10 {
        let session = if n < 10 { "earlier" } else { "now" };
        log.record(UsageEntry {
            instance: None,
            at_ms: 0,
            session: session.into(),
            node: id(2),
            provider: "t".into(),
            model: "m".into(),
            cost: usd(1),
            idempotency_key: format!("k{n}"),
            status: Charge::Actual,
        })
        .unwrap();
    }
    assert_eq!(log.entries().count(), MAX_USAGE_ENTRIES);
    let spent = log.spent("earlier");
    assert_eq!(spent.project.get(&Currency::USD), Some(&usd(u64::try_from(MAX_USAGE_ENTRIES + 10).unwrap())));
    assert_eq!(spent.session.get(&Currency::USD), Some(&usd(10)));
}

#[test]
fn csv_cells_neutralise_formulas_and_quote_separators() {
    for (text, expected) in [
        ("plain", "plain"),
        ("a,b", "\"a,b\""),
        ("two\nlines", "\"two\nlines\""),
        ("+1", "'+1"),
        ("-1", "'-1"),
        ("@cmd", "'@cmd"),
        ("  =1+1", "'  =1+1"),
        ("\u{FF1D}1", "'\u{FF1D}1"),
        ("\tx", "'\tx"),
    ] {
        assert_eq!(cell(text), expected, "{text:?}");
    }
}

/// One text source feeding generator nodes built from `(n, kind, params)`; returns the project and
/// a plan of the generators alone (so their jobs are known: the source resolves inline).
/// A generator to build: node number, kind, and text parameters.
type GenSpec<'a> = (u8, &'a str, &'a [(&'a str, &'a str)]);

fn generators(nodes: &[GenSpec<'_>]) -> (Project, RunPlan) {
    let kinds = kinds();
    let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Cost").unwrap();
    Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut values = BTreeMap::new();
    values.insert("text".to_owned(), Value::Text("a fox".into()));
    Change::AddNode { canvas: C, node: kinds.get("source.text").unwrap().descriptor().instantiate(id(1), &values).unwrap() }.apply(&mut project).unwrap();
    for (n, kind, params) in nodes {
        let params: BTreeMap<String, Value> = params.iter().map(|(k, v)| ((*k).to_owned(), Value::Text((*v).into()))).collect();
        let node = kinds.get(kind).unwrap().descriptor().instantiate(id(*n), &params).unwrap();
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        let edge = Edge {
            id: EdgeId::from_bytes([n + 100; 16]),
            from: Endpoint { node: id(1), port: "out".into() },
            to: Endpoint { node: id(*n), port: "in".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    }
    let requested: BTreeSet<NodeId> = nodes.iter().map(|(n, ..)| id(*n)).collect();
    let canvas = project.canvases().get(&C).unwrap();
    let Planned::Ready(plan) = plan(canvas, &kinds, &RunMode::Selected, &requested, &BTreeSet::new()).unwrap() else { panic!("blocked") };
    (project, plan)
}

#[test]
fn every_model_on_a_node_is_priced_and_free_models_cost_nothing() {
    let (project, plan) = generators(&[(2, "test.two", &[("rewriter", "test.rewrite"), ("model", "test.img")]), (3, "test.gen", &[("model", "test.free")])]);
    let quote = quote(&plan, project.canvases().get(&C).unwrap(), &kinds(), &catalog()).unwrap();
    let step = |n: u8| quote.steps.iter().find(|s| s.node == id(n)).unwrap().clone();
    // 0.01 rewrite + 0.04 image, one job each, known.
    assert_eq!(step(2).total(), Ok(Some(usd(50_000))));
    assert_eq!(step(2).models, vec!["test.rewrite".to_owned(), "test.img".to_owned()]);
    assert!(step(2).jobs_known);
    assert_eq!(step(3).total(), Ok(None));
    assert!(quote.is_complete());
    assert_eq!(quote.totals, [(Currency::USD, usd(50_000))].into_iter().collect());
    // Models in two currencies on one node can't be summed: unpriced, not silently dropped.
    let (project, plan) = generators(&[(2, "test.two", &[("rewriter", "test.rewrite"), ("model", "test.eur")])]);
    let mixed = super::quote(&plan, project.canvases().get(&C).unwrap(), &kinds(), &catalog()).unwrap();
    assert!(matches!(mixed.steps.first().unwrap().total(), Err(Unpriced::Invalid(_))));
}

#[test]
fn deferred_paid_steps_are_estimates_and_currency_mismatches_block() {
    // Generator 3 is fed by generator 2 in the same run, so its jobs are planned later.
    let (mut project, _) = generators(&[(2, "test.two", &[("rewriter", "test.rewrite"), ("model", "test.rewrite")])]);
    let node = kinds()
        .get("test.gen")
        .unwrap()
        .descriptor()
        .instantiate(id(3), &[("model".to_owned(), Value::Text("test.img".into()))].into_iter().collect())
        .unwrap();
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    let edge = Edge {
        id: EdgeId::from_bytes([200; 16]),
        from: Endpoint { node: id(2), port: "out".into() },
        to: Endpoint { node: id(3), port: "in".into() },
        order: 0,
    };
    Change::Connect { canvas: C, edge }.apply(&mut project).unwrap();
    let canvas = project.canvases().get(&C).unwrap();
    let Planned::Ready(chain) = plan(canvas, &kinds(), &RunMode::Selected, &[id(2), id(3)].into_iter().collect(), &BTreeSet::new()).unwrap() else { panic!() };
    let quote = quote(&chain, canvas, &kinds(), &catalog()).unwrap();
    assert_eq!(quote.estimated().collect::<Vec<_>>(), vec![id(3)]);
    assert!(quote.unpriced().next().is_none());
    assert!(!quote.is_complete());
    // Under a session cap the guess passes only provisionally: the real jobs are checked as they
    // start. Without aggregate caps there is nothing to be provisional about.
    let capped = check(&quote, &Budgets { session: Some(usd(10_000_000)), ..Budgets::default() }, &Spent::default()).unwrap();
    assert!(capped.allowed() && !capped.is_final());
    assert_eq!(capped.provisional, vec![id(3)]);
    let uncapped = check(&quote, &Budgets { per_call: Some(usd(10_000_000)), ..Budgets::default() }, &Spent::default()).unwrap();
    assert!(uncapped.is_final());
    // A EUR model under USD caps: per-call and session both refuse to compare.
    let (project, plan) = generators(&[(2, "test.gen", &[("model", "test.eur")]), (3, "test.gen", &[("model", "test.img")])]);
    let mixed = super::quote(&plan, project.canvases().get(&C).unwrap(), &kinds(), &catalog()).unwrap();
    assert_eq!(mixed.totals.len(), 2);
    let caps = Budgets { per_call: Some(usd(1_000_000)), session: Some(usd(10_000_000)), project: None };
    let blocks = check(&mixed, &caps, &Spent::default()).unwrap().blocks;
    assert!(blocks.iter().any(|b| matches!(b, Block::Currency { scope: Scope::PerCall, .. })));
    assert!(blocks.iter().any(|b| matches!(b, Block::Currency { scope: Scope::Session, .. })));
}

#[test]
fn the_same_key_from_another_provider_is_another_charge() {
    let mut log = UsageLog::default();
    let entry = |provider: &str| UsageEntry {
        instance: None,
        at_ms: 0,
        session: "now".into(),
        node: id(2),
        provider: provider.into(),
        model: "m".into(),
        cost: usd(10),
        idempotency_key: "nodecraft:run:1".into(),
        status: Charge::Actual,
    };
    assert!(log.record(entry("fal")).unwrap());
    assert!(log.record(entry("replicate")).unwrap());
    assert!(!log.record(entry("fal")).unwrap());
    assert_eq!(log.spent("now").session.get(&Currency::USD), Some(&usd(20)));
}

fn charge(key: &str, micros: u64, status: Charge) -> UsageEntry {
    UsageEntry {
        instance: None,

        at_ms: 1,
        session: "now".into(),
        node: id(2),
        provider: "fal".into(),
        model: "m".into(),
        cost: usd(micros),
        idempotency_key: key.into(),
        status,
    }
}

#[test]
fn a_generation_kind_without_a_model_is_never_free() {
    let mut kinds = kinds();
    kinds
        .register(Box::new(Gen(NodeDescriptor {
            id: "test.opaque".into(),
            label: "Opaque".into(),
            category: Category::Generation,
            inputs: vec![],
            outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            params: ParamSchema { params: vec![] },
            quick_add: None,
        })))
        .unwrap();
    let node = kinds.get("test.opaque").unwrap().descriptor().instantiate(id(9), &BTreeMap::new()).unwrap();
    let (_, price) = price_step(&node, &kinds, &catalog());
    assert!(matches!(price, Err(Unpriced::Invalid(m)) if m.contains("no model")));
    // Sources (and other non-generation kinds) without a model stay free.
    let text = kinds.get("source.text").unwrap().descriptor().instantiate(id(8), &BTreeMap::new()).unwrap();
    assert_eq!(price_step(&text, &kinds, &catalog()).1, Ok(None));
}

#[test]
fn estimates_settle_to_the_actual_cost() {
    let mut log = UsageLog::default();
    assert!(log.record(charge("k1", 40_000, Charge::Estimate)).unwrap());
    assert!(log.record(charge("k2", 40_000, Charge::Estimate)).unwrap());
    assert_eq!(log.spent("now").session.get(&Currency::USD), Some(&usd(80_000)));
    // The provider reported less for one, and the other's outcome is unknown.
    log.settle("fal", "k1", usd(25_000), Charge::Actual).unwrap();
    log.settle("fal", "k2", usd(40_000), Charge::Unresolved).unwrap();
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(65_000)));
    // Unresolved can still settle; actual is final.
    log.settle("fal", "k2", usd(0), Charge::Actual).unwrap();
    assert_eq!(log.spent("now").session.get(&Currency::USD), Some(&usd(25_000)));
    assert_eq!(log.settle("fal", "k1", usd(1), Charge::Actual).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(log.settle("fal", "nope", usd(1), Charge::Actual).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(log.settle("replicate", "k1", usd(1), Charge::Actual).unwrap_err().kind, ErrorKind::NotFound, "keys are per provider");
    // A recorded retry doesn't replace the estimate: settle does.
    assert!(log.record(charge("k3", 10, Charge::Estimate)).unwrap());
    assert!(!log.record(charge("k3", 99, Charge::Actual)).unwrap());
    assert_eq!(log.entries().find(|e| e.idempotency_key == "k3").map(|e| (e.cost, e.status)), Some((usd(10), Charge::Estimate)));
    assert!(log.to_csv().contains(",USD,estimate,k3"));
}

#[test]
fn settling_works_after_older_entries_leave_the_list() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 3 {
        log.record(charge(&format!("k{n}"), 1, Charge::Estimate)).unwrap();
    }
    let last = format!("k{}", MAX_USAGE_ENTRIES + 2);
    log.settle("fal", &last, usd(5), Charge::Actual).unwrap();
    assert_eq!(log.entries().last().map(|e| e.cost), Some(usd(5)));
    // k0 left the list while open: it is pending, so it still settles (#228), once.
    assert_eq!(log.charge("fal", "k0").map(|e| e.status), Some(Charge::Estimate));
    log.settle("fal", "k0", usd(5), Charge::Actual).unwrap();
    assert_eq!(log.settle("fal", "k0", usd(5), Charge::Actual).unwrap_err().kind, ErrorKind::NotFound, "settled and retired");
    assert!(!log.record(charge("k0", 1, Charge::Actual)).unwrap(), "a retry is still the same charge");
    // An unsettled pending estimate can be released.
    log.release("fal", "k1").unwrap();
    assert!(log.charge("fal", "k1").is_none());
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(u64::try_from(MAX_USAGE_ENTRIES + 3).unwrap() + 4 + 4 - 1)));
    // Pending charges are saved and checked on load.
    let json = serde_json::to_string(&log).unwrap();
    let back: UsageLog = serde_json::from_str(&json).unwrap();
    assert_eq!(back, log);
    assert_eq!(back.charge("fal", "k2").map(|e| e.status), Some(Charge::Estimate));
}

#[test]
fn hostile_costs_are_refused_and_change_nothing() {
    let mut log = UsageLog::default();
    log.record(charge("k1", 10, Charge::Actual)).unwrap();
    let before = log.clone();
    assert!(log.record(charge("k2", MAX_CHARGE_MICROS + 1, Charge::Actual)).is_err());
    assert!(log.record(UsageEntry { provider: "p".repeat(10_000), ..charge("k3", 1, Charge::Actual) }).is_err());
    log.record(charge("k4", 5, Charge::Estimate)).unwrap();
    assert!(log.settle("fal", "k4", usd(u64::MAX), Charge::Actual).is_err());
    assert_eq!(log.entries().count(), before.entries().count() + 1);
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(15)), "spent() never fails and holds");
    // Many maximal charges can't overflow the totals into an error later on.
    let mut big = UsageLog::default();
    for n in 0..1000 {
        big.record(charge(&format!("b{n}"), MAX_CHARGE_MICROS, Charge::Actual)).unwrap();
    }
    assert_eq!(big.spent("now").project.get(&Currency::USD).map(|m| m.micros), Some(1000 * MAX_CHARGE_MICROS));
}

#[test]
fn saved_logs_keep_their_totals_and_are_checked_on_load() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 5 {
        log.record(charge(&format!("k{n}"), 2, Charge::Actual)).unwrap();
    }
    log.record(UsageEntry { session: "later".into(), ..charge("e", 7, Charge::Estimate) }).unwrap();
    let json = serde_json::to_string(&log).unwrap();
    let back: UsageLog = serde_json::from_str(&json).unwrap();
    assert!(back == log, "round trip keeps entries and totals");
    assert_eq!(back.spent("now").project, log.spent("now").project, "evicted spend survives a save: caps don't loosen");
    let mut back = back;
    back.settle("fal", "e", usd(3), Charge::Actual).unwrap();

    let file = |entries: serde_json::Value, project: serde_json::Value| {
        serde_json::json!({"entries": entries, "project": project, "sessions": {"now": project}}).to_string()
    };
    let entry = serde_json::to_value(charge("x", 50, Charge::Actual)).unwrap();
    let usd_total = |micros: u64| serde_json::json!({"USD": usd(micros)});
    // Totals lower than the listed charges are refused, as are duplicate keys and bad costs.
    assert!(serde_json::from_str::<UsageLog>(&file(serde_json::json!([entry]), usd_total(10))).is_err());
    assert!(serde_json::from_str::<UsageLog>(&file(serde_json::json!([entry, entry]), usd_total(100))).is_err());
    let huge = serde_json::to_value(charge("h", MAX_CHARGE_MICROS + 1, Charge::Actual)).unwrap();
    assert!(serde_json::from_str::<UsageLog>(&file(serde_json::json!([huge]), usd_total(u64::MAX))).is_err());
    assert!(serde_json::from_str::<UsageLog>(r#"{"entries": [], "project": {}, "extra": 1}"#).is_err());
    // A session's totals must cover its charges too.
    let ok = serde_json::json!({"entries": [entry], "project": usd_total(50), "sessions": {"now": usd_total(50)}}).to_string();
    assert!(serde_json::from_str::<UsageLog>(&ok).is_ok());
    let low = serde_json::json!({"entries": [entry], "project": usd_total(50), "sessions": {"now": usd_total(1)}}).to_string();
    assert!(serde_json::from_str::<UsageLog>(&low).is_err());
    // Old entries without a status are actual charges.
    let mut legacy = entry.clone();
    legacy.as_object_mut().unwrap().remove("status");
    let loaded: UsageLog = serde_json::from_str(&file(serde_json::json!([legacy]), usd_total(50))).unwrap();
    assert_eq!(loaded.entries().next().map(|e| e.status), Some(Charge::Actual));
}

#[test]
fn review_regressions_for_219() {
    // A saved file can't claim totals that block later charges or sit outside the project.
    let load = |v: serde_json::Value| serde_json::from_str::<UsageLog>(&v.to_string());
    let max = Money::new(Currency::USD, u64::MAX);
    assert!(load(serde_json::json!({"entries": [], "project": {"USD": max}, "sessions": {"s": {"USD": max}}})).is_err());
    let eur = Money::new(Currency::EUR, 5);
    assert!(load(serde_json::json!({"entries": [], "project": {}, "sessions": {"other": {"EUR": eur}}})).is_err());
    assert!(load(serde_json::json!({"entries": [], "project": {"EUR": eur}, "sessions": {"other": {"EUR": eur}}})).is_ok());
    let entry = serde_json::to_value(charge("x", 50, Charge::Actual)).unwrap();
    let mut extra = entry.clone();
    extra.as_object_mut().unwrap().insert("surprise".into(), serde_json::json!(1));
    assert!(load(serde_json::json!({"entries": [extra], "project": {"USD": usd(50)}, "sessions": {"now": {"USD": usd(50)}}})).is_err());

    // Settling can't change currency or go back to an estimate.
    let mut log = UsageLog::default();
    log.record(charge("k", 40_000, Charge::Estimate)).unwrap();
    assert_eq!(log.settle("fal", "k", Money::new(Currency::EUR, 1), Charge::Actual).unwrap_err().kind, ErrorKind::CurrencyMismatch);
    assert!(log.settle("fal", "k", usd(1), Charge::Estimate).is_err());
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(40_000)));

    // A certainly unbilled job is released, and its key can be reserved again.
    log.record(charge("after", 7, Charge::Estimate)).unwrap();
    log.release("fal", "k").unwrap();
    assert_eq!(log.spent("now").project.get(&Currency::USD), Some(&usd(7)));
    assert!(log.record(charge("k", 40_000, Charge::Estimate)).unwrap(), "the key is free again");
    log.settle("fal", "after", usd(6), Charge::Actual).unwrap();
    log.settle("fal", "k", usd(30_000), Charge::Actual).unwrap();
    assert_eq!(log.spent("now").session.get(&Currency::USD), Some(&usd(30_006)));
    assert_eq!(log.release("fal", "k").unwrap_err().kind, ErrorKind::Conflict, "actual charges are final");
    assert_eq!(log.release("fal", "nope").unwrap_err().kind, ErrorKind::NotFound);
    log.record(charge("lost", 9, Charge::Estimate)).unwrap();
    log.settle("fal", "lost", usd(9), Charge::Unresolved).unwrap();
    assert_eq!(log.release("fal", "lost").unwrap_err().kind, ErrorKind::Conflict, "unresolved may have been billed");
    // Still saves and loads after a release.
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert!(back == log);
}

#[test]
fn charges_are_found_after_releases_and_eviction() {
    let mut log = UsageLog::default();
    for n in 0..5 {
        log.record(charge(&format!("k{n}"), 1, Charge::Estimate)).unwrap();
    }
    log.release("fal", "k1").unwrap();
    assert!(log.charge("fal", "k1").is_none());
    assert_eq!(log.charge("fal", "k3").map(|e| e.idempotency_key.as_str()), Some("k3"), "renumbered after the release");
    assert!(log.charge("other", "k3").is_none());
    let mut full = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 2 {
        full.record(charge(&format!("k{n}"), 1, Charge::Actual)).unwrap();
    }
    assert!(full.charge("fal", "k0").is_none(), "evicted");
    let last = format!("k{}", MAX_USAGE_ENTRIES + 1);
    assert_eq!(full.charge("fal", &last).map(|e| e.idempotency_key.clone()), Some(last));
}

fn entry(session: &str, key: &str, cost: u64) -> UsageEntry {
    UsageEntry {
        instance: None,

        at_ms: 0,
        session: session.into(),
        node: id(2),
        provider: "t".into(),
        model: "m".into(),
        cost: usd(cost),
        idempotency_key: key.into(),
        status: Charge::Actual,
    }
}

#[test]
fn a_known_empty_job_list_quotes_nothing() {
    let (project, mut plan) = setup();
    let step = plan.steps.iter_mut().find(|s| s.node == id(2)).unwrap();
    step.jobs = Some(Vec::new());
    let quote = quote(&plan, project.canvases().get(&C).unwrap(), &kinds(), &catalog()).unwrap();
    let step = quote.steps.iter().find(|s| s.node == id(2)).unwrap();
    assert_eq!((step.jobs, step.jobs_known), (0, true));
    assert_eq!(step.total(), Ok(Some(usd(0))));
}

#[test]
fn a_retry_of_a_charge_that_left_the_list_is_not_charged_again() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 5 {
        assert!(log.record(entry("s", &format!("k{n}"), 1)).unwrap());
    }
    let before = log.spent("s");
    // k0..k4 left the list, but a late retry of one is still the same charge.
    assert!(!log.record(entry("s", "k0", 1)).unwrap());
    assert_eq!(log.spent("s"), before);
    // The retired identities survive saving and loading.
    let json = serde_json::to_string(&log).unwrap();
    let mut back: UsageLog = serde_json::from_str(&json).unwrap();
    assert!(!back.record(entry("s", "k3", 1)).unwrap());
    assert_eq!(back, log);
    // A file can't list a charge it also says retired, or garble an identity.
    let mut file: UsageLogFile = serde_json::from_str(&json).unwrap();
    file.retired.push("zz".into());
    assert!(UsageLog::try_from(file).is_err());
}

#[test]
fn listed_text_and_sessions_are_bounded_without_losing_spend() {
    let mut log = UsageLog::default();
    // Long keys: the text bound evicts before the entry count does.
    let long = "k".repeat(500);
    let per_entry = 1 + 1 + 1 + 500 + 8;
    let fit = MAX_LISTED_TEXT / per_entry;
    for n in 0..fit + 10 {
        log.record(entry("s", &format!("{n:08}{long}"), 1)).unwrap();
    }
    assert!(log.entries().count() <= fit);
    assert_eq!(log.spent("s").project.get(&Currency::USD), Some(&usd(u64::try_from(fit + 10).unwrap())));
    // Many short sessions: old ones fold into the earlier-sessions total; the project total and
    // the current session's stay exact, and the log still reloads.
    let mut log = UsageLog::default();
    for n in 0..MAX_SESSIONS + 50 {
        log.record(entry(&format!("session-{n:05}"), &format!("k{n}"), 2)).unwrap();
    }
    let json = serde_json::to_string(&log).unwrap();
    let file: UsageLogFile = serde_json::from_str(&json).unwrap();
    assert!(file.sessions.len() <= MAX_SESSIONS);
    let last = format!("session-{:05}", MAX_SESSIONS + 49);
    assert_eq!(log.spent(&last).session.get(&Currency::USD), Some(&usd(2)));
    assert_eq!(log.spent(&last).project.get(&Currency::USD), Some(&usd(2 * u64::try_from(MAX_SESSIONS + 50).unwrap())));
    let back: UsageLog = serde_json::from_str(&json).unwrap();
    assert_eq!(back, log);
    // The earlier-sessions name is reserved.
    assert!(log.record(entry(EARLIER_SESSIONS, "x", 1)).is_err());
}

#[test]
fn csv_streams_the_same_rows() {
    let mut log = UsageLog::default();
    log.record(entry("s", "=k1", 5)).unwrap();
    let mut streamed = Vec::new();
    log.write_csv(&mut streamed).unwrap();
    assert_eq!(String::from_utf8(streamed).unwrap(), log.to_csv());
    assert!(log.to_csv().contains("'=k1"));
}

#[test]
fn a_provisional_admission_is_caught_when_the_step_expands_past_the_cap() {
    let deferred = StepQuote { node: id(3), models: vec!["test.img".into()], per_job: Ok(Some(usd(40_000))), jobs: 1, jobs_known: false };
    let quote = Quote { steps: vec![deferred.clone()], totals: [(Currency::USD, usd(40_000))].into_iter().collect() };
    let caps = Budgets { session: Some(usd(100_000)), ..Budgets::default() };
    let admission = check(&quote, &caps, &Spent::default()).unwrap();
    assert!(admission.allowed() && !admission.is_final(), "one guessed job fits, provisionally");
    // The step's jobs turn out to be three: 0.12 is past the 0.10 cap, so checking the real jobs
    // (as job-time enforcement does) refuses it.
    let expanded = StepQuote { jobs: 3, jobs_known: true, ..deferred };
    let quote = Quote { steps: vec![expanded], totals: [(Currency::USD, usd(120_000))].into_iter().collect() };
    let admission = check(&quote, &caps, &Spent::default()).unwrap();
    assert!(matches!(admission.blocks.as_slice(), [Block::Over { scope: Scope::Session, .. }]));
}

fn estimate(session: &str, key: &str, cost: u64) -> UsageEntry {
    UsageEntry { status: Charge::Estimate, ..entry(session, key, cost) }
}

#[test]
fn a_folded_session_with_listed_charges_settles_reloads_and_charges_again() {
    let mut log = UsageLog::default();
    // "a" (sorting first) holds an open estimate and an actual charge, then goes quiet.
    log.record(estimate("a", "a-open", 10)).unwrap();
    log.record(estimate("a", "a-free", 4)).unwrap();
    log.record(entry("a", "a-done", 5)).unwrap();
    for n in 0..MAX_SESSIONS {
        log.record(entry(&format!("s{n:05}"), &format!("k{n}"), 1)).unwrap();
    }
    // a holds open charges, so it isn't folded: their settle and release come out of its own total.
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(19)), "a kept its total");
    log.settle("t", "a-open", usd(7), Charge::Actual).unwrap();
    log.release("t", "a-free").unwrap();
    log.record(entry("a", "a-new", 3)).unwrap();
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(15)));
    let project = 7 + 5 + 3 + u64::try_from(MAX_SESSIONS).unwrap();
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(project)));
    // And the log the app saved reloads.
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(back, log);
    // With nothing open any more, a quiet a folds like any other session.
    for n in 0..MAX_SESSIONS {
        log.record(entry(&format!("t{n:05}"), &format!("j{n}"), 1)).unwrap();
    }
    assert!(log.spent("a").session.is_empty(), "a was folded");
    assert_eq!(log.spent("a").project.get(&Currency::USD), Some(&usd(project + u64::try_from(MAX_SESSIONS).unwrap())));
}

#[test]
fn folding_picks_the_least_recently_charged_session() {
    let mut log = UsageLog::default();
    // "a" sorts first but keeps charging; the oldest quiet session folds instead.
    for n in 0..MAX_SESSIONS - 1 {
        log.record(entry(&format!("s{n:05}"), &format!("k{n}"), 1)).unwrap();
        if n % 100 == 0 {
            log.record(entry("a", &format!("a{n}"), 1)).unwrap();
        }
    }
    log.record(entry("new", "new", 1)).unwrap();
    assert!(log.spent("a").session.get(&Currency::USD).is_some_and(|m| m.micros > 0), "a is active and keeps its total");
    assert!(log.spent("s00000").session.is_empty(), "the least recently charged session folded");
}

#[test]
fn retired_identities_are_validated_released_keys_are_free_and_old_ones_are_forgotten() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 1 {
        log.record(entry("s", &format!("k{n}"), 1)).unwrap();
    }
    let json = serde_json::to_string(&log).unwrap();
    let file: UsageLogFile = serde_json::from_str(&json).unwrap();
    let retired = file.retired.first().cloned().unwrap();
    // A duplicate identity, an uppercase one, and one that is also listed are refused.
    let mut dup = file.clone();
    dup.retired.push(retired.clone());
    assert!(UsageLog::try_from(dup).is_err());
    let mut upper = file.clone();
    upper.retired = vec![retired.to_uppercase()];
    assert!(UsageLog::try_from(upper).is_err());
    let mut listed = file.clone();
    listed.retired.push(super::hex(&super::retired_id("t", &format!("k{}", MAX_USAGE_ENTRIES))));
    assert!(UsageLog::try_from(listed).is_err());
    // A released estimate isn't retired: its key can be charged again.
    log.record(estimate("s", "maybe", 2)).unwrap();
    log.release("t", "maybe").unwrap();
    assert!(log.record(entry("s", "maybe", 2)).unwrap());
    // Past MAX_RETIRED_KEYS, the oldest identities are forgotten (the documented boundary).
    let mut big = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + MAX_RETIRED_KEYS + 1 {
        big.record(entry("s", &format!("k{n}"), 1)).unwrap();
    }
    assert!(big.record(entry("s", "k0", 1)).unwrap(), "forgotten: counts again");
    assert!(!big.record(entry("s", "k5", 1)).unwrap(), "still remembered");
}

#[test]
fn a_step_that_runs_no_jobs_blocks_nothing() {
    let unpriced = StepQuote { node: id(4), models: vec![], per_job: Err(Unpriced::NoModel), jobs: 0, jobs_known: true };
    let quote = Quote { steps: vec![unpriced], totals: BTreeMap::new() };
    assert!(quote.is_complete());
    let caps = Budgets { per_call: Some(usd(1)), session: Some(usd(1)), project: Some(usd(1)) };
    assert!(check(&quote, &caps, &Spent::default()).unwrap().is_final());
}

#[test]
fn settling_respects_the_total_ceiling() {
    let mut log = UsageLog::default();
    // Fill the project close to its ceiling with maximal charges, then try to settle above it.
    let full = MAX_TOTAL_MICROS / MAX_CHARGE_MICROS;
    for n in 0..full - 1 {
        log.record(UsageEntry { cost: usd(MAX_CHARGE_MICROS), ..entry("s", &format!("big{n}"), 0) }).unwrap();
    }
    log.record(UsageEntry { status: Charge::Estimate, ..entry("s", "last", 1) }).unwrap();
    let before = log.clone();
    assert!(log.settle("t", "last", usd(MAX_CHARGE_MICROS), Charge::Actual).is_ok());
    let mut over = before.clone();
    over.record(UsageEntry { status: Charge::Estimate, ..entry("s", "more", 1) }).unwrap();
    let snapshot = over.clone();
    assert_eq!(over.settle("t", "more", usd(MAX_CHARGE_MICROS), Charge::Actual).unwrap_err().kind, ErrorKind::Overflow);
    assert_eq!(over, snapshot, "nothing changed");
}

#[test]
fn a_reused_session_with_an_open_charge_keeps_its_own_total() {
    let mut log = UsageLog::default();
    let in_session = |session: &str, key: &str, micros: u64, status: Charge| UsageEntry { session: session.into(), ..charge(key, micros, status) };
    log.record(in_session("a", "old", 5_000_000, Charge::Estimate)).unwrap();
    // Enough other sessions to force folding; "a" has an open charge, so it isn't folded.
    for n in 0..MAX_SESSIONS + 1 {
        log.record(in_session(&format!("s{n}"), &format!("k{n}"), 1, Charge::Actual)).unwrap();
    }
    log.record(in_session("a", "new", 5_000_000, Charge::Estimate)).unwrap();
    log.release("fal", "old").unwrap();
    assert_eq!(log.spent("a").session.get(&Currency::USD), Some(&usd(5_000_000)), "the new estimate still counts");
    assert!(log.session_clock.len() <= log.sessions.len(), "the clock forgets folded sessions");
    // It still saves and loads.
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(back, log);
}

#[test]
fn many_open_sessions_settle_reload_and_fold_back_under_the_limit() {
    let mut log = UsageLog::default();
    let extra = 50;
    for n in 0..MAX_SESSIONS + extra {
        log.record(UsageEntry { session: format!("o{n:05}"), ..charge(&format!("k{n}"), 1, Charge::Estimate) }).unwrap();
    }
    assert!(log.sessions.len() > MAX_SESSIONS, "open sessions are kept past the limit");
    for n in 0..MAX_SESSIONS + extra {
        log.settle("fal", &format!("k{n}"), usd(1), Charge::Actual).unwrap();
    }
    // A log saved with more sessions than the limit (their charges now settled) reloads.
    let back: UsageLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(back, log);
    // The next new session folds the surplus at once.
    let mut log = back;
    log.record(UsageEntry { session: "next".into(), ..charge("next", 1, Charge::Actual) }).unwrap();
    assert!(log.sessions.len() <= MAX_SESSIONS);
    let total = u64::try_from(MAX_SESSIONS + extra + 1).unwrap();
    assert_eq!(log.spent("next").project.get(&Currency::USD), Some(&usd(total)), "folding never changes the project total");
}

#[test]
fn folding_keeps_the_session_count_at_the_limit() {
    let mut log = UsageLog::default();
    for n in 0..MAX_SESSIONS {
        log.record(UsageEntry { session: format!("s{n}"), ..charge(&format!("k{n}"), 1, Charge::Actual) }).unwrap();
    }
    assert_eq!(log.sessions.len(), MAX_SESSIONS);
    log.record(UsageEntry { session: "new".into(), ..charge("kn", 1, Charge::Actual) }).unwrap();
    // The earlier-sessions total takes a slot of its own, so the first fold makes room for it
    // and for the new session; later folds take one each.
    assert_eq!(log.sessions.len(), MAX_SESSIONS);
    assert!(!log.sessions.contains_key("s0") && log.sessions.contains_key("s2"), "the least recent folded first");
    log.record(UsageEntry { session: "newer".into(), ..charge("kn2", 1, Charge::Actual) }).unwrap();
    assert_eq!(log.sessions.len(), MAX_SESSIONS);
    assert!(log.sessions.contains_key("s3"));
}

#[test]
fn saved_pending_charges_are_checked_on_load() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 1 {
        log.record(estimate("s", &format!("k{n}"), 1)).unwrap();
    }
    assert_eq!(log.pending().map(|e| e.idempotency_key.as_str()).collect::<Vec<_>>(), vec!["k0"]);
    let file: UsageLogFile = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
    assert_eq!(file.pending.len(), 1);
    // The same key twice within pending: refused.
    // (At no cost, so only the duplicate guard can refuse it, not the totals check.)
    let mut twice = file.clone();
    twice.pending.push(UsageEntry { cost: usd(0), ..twice.pending[0].clone() });
    assert!(UsageLog::try_from(twice).is_err());
    assert!(UsageLog::try_from(file.clone()).is_ok());
    // Settled, listed too, retired too, or too many: refused.
    let mut settled = file.clone();
    settled.pending[0].status = Charge::Actual;
    assert!(UsageLog::try_from(settled).is_err());
    let mut listed = file.clone();
    listed.pending[0].idempotency_key = file.entries[0].idempotency_key.clone();
    assert!(UsageLog::try_from(listed).is_err());
    let mut retired = file.clone();
    retired.retired.push(super::hex(&super::retired_id("t", &file.pending[0].idempotency_key)));
    assert!(UsageLog::try_from(retired).is_err());
    let mut many = file;
    many.pending = (0..=MAX_PENDING).map(|n| estimate("s", &format!("p{n}"), 0)).collect();
    assert!(UsageLog::try_from(many).is_err());
}

#[test]
fn once_pending_is_full_an_open_charge_leaving_the_list_stays_charged_as_it_is() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + MAX_PENDING + 1 {
        log.record(estimate("s", &format!("k{n}"), 1)).unwrap();
    }
    let total = u64::try_from(MAX_USAGE_ENTRIES + MAX_PENDING + 1).unwrap();
    // k0..k9999 are pending; the last one out found pending full: identity only.
    let last_out = format!("k{MAX_PENDING}");
    assert!(log.charge("t", "k0").is_some());
    assert!(log.charge("t", &last_out).is_none());
    assert_eq!(log.settle("t", &last_out, usd(0), Charge::Actual).unwrap_err().kind, ErrorKind::NotFound);
    assert!(!log.record(estimate("s", &last_out, 1)).unwrap(), "still the same charge");
    assert_eq!(log.spent("s").project.get(&Currency::USD), Some(&usd(total)), "it still counts");
}

mod strict257;

mod legacy255;
mod reconcile325;

#[test]
fn csv_row_admission_never_writes_rejected_rows_and_preserves_writer_errors() {
    let mut log = UsageLog::default();
    log.record(entry("s", "k1", 1)).unwrap();
    log.record(entry("s", "k2", 2)).unwrap();
    let mut expected = Vec::new();
    log.write_csv_rows(&mut expected, 0, 1).unwrap();
    let mut actual = Vec::new();
    let mut calls = 0;
    assert_eq!(
        log.write_csv_rows_while(&mut actual, 0, 2, |row| {
            assert!(row.ends_with(b"\r\n"));
            calls += 1;
            calls == 1
        })
        .unwrap(),
        1
    );
    assert_eq!(calls, 2);
    assert_eq!(actual, expected);
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::PermissionDenied.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert_eq!(log.write_csv_rows(&mut Broken, 0, 2).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(log.write_csv_rows_while(&mut Broken, 0, 2, |_| true).unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
}

// Exercise the public quote path for one detached node, rather than the removed
// private raw-selection helper.
fn price_step(node: &Node, kinds: &Registry, catalog: &Catalog) -> (Vec<String>, Result<Option<Money>, Unpriced>) {
    let mut project = Project::new(ProjectId::from_bytes([9; 16]), "Quote").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    Change::AddNode { canvas: C, node: node.clone() }.apply(&mut project).unwrap();
    let plan = RunPlan {
        steps: vec![crate::planner::Step {
            node: node.id,
            kind: node.kind.clone(),
            mode: ModeId::new("quote").unwrap(),
            after: vec![],
            inputs: vec![],
            jobs: None,
            fanout: None,
        }],
        edges: vec![],
        inner: Default::default(),
    };
    let quote = quote(&plan, project.canvases().get(&C).unwrap(), kinds, catalog).unwrap();
    let step = quote.steps.into_iter().next().unwrap();
    (step.models, step.per_job)
}
