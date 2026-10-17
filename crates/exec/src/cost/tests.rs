use super::*;
use crate::planner::{Planned, RunMode, plan};
use nodecraft_graph::{Change, Edge, Endpoint, Port, Position, Project};
use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry};
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
    assert!(check(&full, &none, &Spent::default()).unwrap().is_empty());
    // Per call: 0.12 is over a 0.10 cap.
    let per_call = Budgets { per_call: Some(usd(100_000)), ..Budgets::default() };
    assert_eq!(
        check(&priced, &per_call, &Spent::default()).unwrap(),
        vec![Block::Over { scope: Scope::PerCall, node: Some(id(2)), limit: usd(100_000), would_be: usd(120_000) }]
    );
    // Session: 0.45 already spent plus 0.12 is over 0.50; 0.30 spent is fine.
    let session = Budgets { session: Some(usd(500_000)), ..Budgets::default() };
    let spent = |micros| Spent { session: [(Currency::USD, usd(micros))].into_iter().collect(), project: BTreeMap::new() };
    let blocks = check(&priced, &session, &spent(450_000)).unwrap();
    assert_eq!(blocks, vec![Block::Over { scope: Scope::Session, node: None, limit: usd(500_000), would_be: usd(570_000) }]);
    assert_eq!(blocks.first().unwrap().message(), "This would exceed the session budget: 0.57 USD of 0.5 USD.");
    assert!(check(&priced, &session, &spent(300_000)).unwrap().is_empty());
    // Exactly at the cap is allowed.
    assert!(check(&priced, &session, &spent(380_000)).unwrap().is_empty());
    // A run that costs nothing isn't blocked even when the budget is already used up.
    let free = Quote { steps: priced.steps.iter().filter(|s| s.node == id(1)).cloned().collect(), totals: BTreeMap::new() };
    assert!(check(&free, &session, &spent(600_000)).unwrap().is_empty());
    // Session and project caps together, each against its own spend.
    let both = Budgets { session: Some(usd(1_000_000)), project: Some(usd(2_000_000)), per_call: None };
    let spent_both = Spent { session: [(Currency::USD, usd(100_000))].into_iter().collect(), project: [(Currency::USD, usd(1_950_000))].into_iter().collect() };
    assert_eq!(
        check(&priced, &both, &spent_both).unwrap(),
        vec![Block::Over { scope: Scope::Project, node: None, limit: usd(2_000_000), would_be: usd(2_070_000) }]
    );
    // Unknown prices block any capped run.
    assert!(check(&full, &session, &Spent::default()).unwrap().iter().any(|b| matches!(b, Block::Unknown { scope: Scope::Session, .. })));
    // A EUR cap can't be compared with a USD price.
    let euro = Budgets { project: Some(Money::new(Currency::EUR, 10_000_000)), ..Budgets::default() };
    assert!(matches!(check(&priced, &euro, &Spent::default()).unwrap().as_slice(), [Block::Currency { scope: Scope::Project, .. }]));
}

#[test]
fn the_usage_log_counts_each_charge_once_and_exports_safe_csv() {
    let mut log = UsageLog::default();
    let entry = |key: &str, micros: u64, session: &str, model: &str| UsageEntry {
        at_ms: 1_700_000_000_000,
        session: session.into(),
        node: id(2),
        provider: "test".into(),
        model: model.into(),
        cost: usd(micros),
        idempotency_key: key.into(),
    };
    assert!(log.record(entry("nodecraft:run:1", 40_000, "now", "test.img")).unwrap());
    // A retry under the same key is the same charge.
    assert!(!log.record(entry("nodecraft:run:1", 40_000, "now", "test.img")).unwrap());
    assert!(log.record(entry("nodecraft:old:1", 1_500_000, "earlier", "=HYPERLINK(\"evil\")")).unwrap());
    // An empty key can't tell retries from new charges.
    assert!(log.record(entry("", 1, "now", "x")).is_err());
    let spent = log.spent("now").unwrap();
    assert_eq!(spent.session, [(Currency::USD, usd(40_000))].into_iter().collect());
    assert_eq!(spent.project, [(Currency::USD, usd(1_540_000))].into_iter().collect());
    let csv = log.to_csv();
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines.first(), Some(&"time_ms,session,node,provider,model,amount,currency,idempotency_key"));
    assert!(lines.get(1).unwrap().contains(",test,test.img,0.04,USD,nodecraft:run:1"));
    // Formula-looking text is neutralised and quoted.
    assert!(lines.get(2).unwrap().contains(",\"'=HYPERLINK(\"\"evil\"\")\",1.5,USD,"));
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
    assert!(check(&quote, &Budgets { per_call: Some(usd(50_000)), ..Budgets::default() }, &Spent::default()).unwrap().is_empty());
}

#[test]
fn eviction_never_lowers_what_was_spent() {
    let mut log = UsageLog::default();
    for n in 0..MAX_USAGE_ENTRIES + 10 {
        let session = if n < 10 { "earlier" } else { "now" };
        log.record(UsageEntry {
            at_ms: 0,
            session: session.into(),
            node: id(2),
            provider: "t".into(),
            model: "m".into(),
            cost: usd(1),
            idempotency_key: format!("k{n}"),
        })
        .unwrap();
    }
    assert_eq!(log.entries().count(), MAX_USAGE_ENTRIES);
    let spent = log.spent("earlier").unwrap();
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
    // A EUR model under USD caps: per-call and session both refuse to compare.
    let (project, plan) = generators(&[(2, "test.gen", &[("model", "test.eur")]), (3, "test.gen", &[("model", "test.img")])]);
    let mixed = super::quote(&plan, project.canvases().get(&C).unwrap(), &kinds(), &catalog()).unwrap();
    assert_eq!(mixed.totals.len(), 2);
    let caps = Budgets { per_call: Some(usd(1_000_000)), session: Some(usd(10_000_000)), project: None };
    let blocks = check(&mixed, &caps, &Spent::default()).unwrap();
    assert!(blocks.iter().any(|b| matches!(b, Block::Currency { scope: Scope::PerCall, .. })));
    assert!(blocks.iter().any(|b| matches!(b, Block::Currency { scope: Scope::Session, .. })));
}

#[test]
fn the_same_key_from_another_provider_is_another_charge() {
    let mut log = UsageLog::default();
    let entry = |provider: &str| UsageEntry {
        at_ms: 0,
        session: "now".into(),
        node: id(2),
        provider: provider.into(),
        model: "m".into(),
        cost: usd(10),
        idempotency_key: "nodecraft:run:1".into(),
    };
    assert!(log.record(entry("fal")).unwrap());
    assert!(log.record(entry("replicate")).unwrap());
    assert!(!log.record(entry("fal")).unwrap());
    assert_eq!(log.spent("now").unwrap().session.get(&Currency::USD), Some(&usd(20)));
}
