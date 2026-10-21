//! `quote`: what running a project would cost, priced by the engine's `run.quote` exactly as the
//! canvas prices it. Nothing is run or charged.
//!
//! Like the app, the CLI routes each card a run reaches to its provider's adapter first; building
//! an adapter reads its key from the keychain, so only the adapters the run needs are built. A
//! provider that can't run (no key, no server) is named with what to do about it.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use nodecraft_engine::Session;
use nodecraft_engine::provider_setup::{ProviderSetup, Readiness};
use nodecraft_engine::run_cmds::BudgetState;
use nodecraft_exec::cost::{Budgets, Spent};
use nodecraft_graph::Canvas;
use nodecraft_providers::Provider;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::{Value, json};

use crate::inspect::{has_result, model_of};

/// Builds one provider's adapter (the CLI's reads the keychain; tests pass their own).
pub type Maker<'a> = dyn FnMut(&str) -> Result<Arc<dyn Provider>, Readiness> + 'a;
/// The adapter each card runs on, and the technique node each inlined card runs for.
type Adapters = (BTreeMap<NodeId, String>, BTreeMap<NodeId, NodeId>);

/// The canvas `wanted` names, or the project's one node canvas.
pub fn canvas_of(session: &Session, wanted: Option<&str>) -> Result<CanvasId, Error> {
    let canvases = session.document().canvases();
    if let Some(wanted) = wanted {
        let id: CanvasId = wanted.parse().map_err(|_| Error::new(ErrorKind::InvalidInput, "`--canvas` must be a canvas id (see `inspect`)"))?;
        let canvas = canvases.get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "the project has no canvas with that id"))?;
        if !canvas.kind().is_canvas() {
            return Err(Error::new(ErrorKind::InvalidInput, "that canvas isn't a node canvas; nothing on it runs"));
        }
        return Ok(id);
    }
    let mut graphs = canvases.values().filter(|c| c.kind().is_canvas());
    match (graphs.next(), graphs.next()) {
        (Some(only), None) => Ok(only.id()),
        (None, _) => Err(Error::new(ErrorKind::NotFound, "the project has no node canvas")),
        (Some(_), Some(_)) => Err(Error::new(ErrorKind::InvalidInput, "the project has several canvases; pick one with `--canvas <id>` (see `inspect`)")),
    }
}

/// `nodes` and the upstream cards a run of them also runs: those stale or without a result. The
/// graph may have cycles; the seen-set ends the walk.
fn with_upstream(canvas: &Canvas, nodes: &[NodeId], stale: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
    let mut seen: BTreeSet<NodeId> = nodes.iter().copied().collect();
    let mut inputs: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        inputs.entry(edge.to.node).or_default().push(edge.from.node);
    }
    let mut next = nodes.to_vec();
    while let Some(node) = next.pop() {
        for from in inputs.get(&node).into_iter().flatten() {
            let runs = canvas.nodes().get(from).is_some_and(|up| stale.contains(from) || !has_result(up));
            if runs && seen.insert(*from) {
                next.push(*from);
            }
        }
    }
    seen
}

/// The adapter each card with a catalog model runs on (cards a technique inlines included), and
/// the technique node each inlined card runs for.
fn adapters_of(session: &Session, canvas: &Canvas) -> Result<Adapters, Error> {
    let catalog = session.generation_catalog()?.run_catalog();
    let expanded = nodecraft_exec::technique::expand(session.document().canvases(), canvas, session.kinds())
        .unwrap_or_else(|_| nodecraft_exec::technique::Expanded::plain(canvas));
    let adapters = expanded
        .canvas
        .nodes()
        .iter()
        .filter_map(|(id, node)| catalog.catalog().get(model_of(node)?).map(|descriptor| (*id, descriptor.provider.clone())))
        .collect();
    Ok((adapters, expanded.inner))
}

/// The cards a quote is for: `nodes`, or every stale or never-run card when it is empty.
pub fn targets(session: &Session, canvas: CanvasId, nodes: &[NodeId]) -> Result<Vec<NodeId>, Error> {
    let graph = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such canvas"))?;
    for node in nodes {
        if !graph.nodes().contains_key(node) {
            return Err(Error::new(ErrorKind::NotFound, format!("the canvas has no node {node}")));
        }
    }
    if !nodes.is_empty() {
        return Ok(nodes.to_vec());
    }
    let stale = nodecraft_exec::staleness::stale(graph, session.kinds())?;
    Ok(graph.nodes().values().filter(|n| stale.contains(&n.id) || !has_result(n)).map(|n| n.id).collect())
}

/// Builds the adapters a run of `targets` and their upstream needs with `maker`, routes the
/// canvas's cards to them and installs empty budgets: what `run.quote` reads.
pub fn prepare(session: &mut Session, canvas: CanvasId, targets: &[NodeId], maker: &mut Maker<'_>) -> Result<(), Error> {
    let graph = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such canvas"))?;
    let stale = nodecraft_exec::staleness::stale(graph, session.kinds())?;
    let mut reached = with_upstream(graph, targets, &stale);
    let (adapters, inner) = adapters_of(session, graph)?;
    reached.extend(inner.iter().filter(|(_, owner)| reached.contains(owner)).map(|(id, _)| *id).collect::<Vec<_>>());
    let needed: BTreeSet<&str> = reached.iter().filter_map(|node| adapters.get(node).map(String::as_str)).collect();
    let mut setup = ProviderSetup::default();
    let unready: Vec<String> = needed.into_iter().filter_map(|adapter| setup.ensure(adapter, &mut *maker).message(adapter)).collect();
    if !unready.is_empty() {
        return Err(Error::new(ErrorKind::Unsupported, unready.join(" ")));
    }
    let ready = adapters.iter().filter(|(_, a)| setup.readiness(a) == Some(&Readiness::Ready)).map(|(node, a)| (*node, a.as_str()));
    let routing = setup.routing(ready)?;
    session.set_provider_routing(routing)?;
    // No budgets are set from the command line, and nothing was spent in this process.
    session.swap_resource(Some(BudgetState { budgets: Budgets::default(), spent: Spent::default() }));
    Ok(())
}

/// The `run.quote` result for a run of `targets` with their upstream.
pub fn quote(session: &mut Session, canvas: CanvasId, targets: &[NodeId]) -> Result<Value, Error> {
    // Nothing to run still gets a (empty) quote: `all_stale` on a canvas with nothing stale.
    let params =
        if targets.is_empty() { json!({"canvas": canvas, "mode": "all_stale"}) } else { json!({"canvas": canvas, "mode": "with_upstream", "nodes": targets}) };
    session.dispatch("run.quote", &params)
}

/// A quote in words. Node titles come from `session`'s canvas.
pub fn quote_text(session: &Session, canvas: CanvasId, quote: &Value) -> String {
    let graph = session.document().canvases().get(&canvas);
    let name = |node: &Value| -> String {
        let id = node.as_str().and_then(|s| s.parse::<NodeId>().ok());
        let found = id.and_then(|id| graph.and_then(|g| g.nodes().get(&id)));
        match found {
            Some(node) if !node.title.is_empty() => format!("{} ({})", node.title, node.kind),
            Some(node) => node.kind.clone(),
            None => node.as_str().unwrap_or_default().to_owned(),
        }
    };
    let strings = |key: &str| quote[key].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
    let mut out = String::new();
    let steps = quote["steps"].as_array().map_or(&[][..], Vec::as_slice);
    let total_steps = quote["steps_total"].as_u64().unwrap_or(u64::try_from(steps.len()).unwrap_or(u64::MAX));
    out.push_str(&match total_steps {
        0 => "Nothing to run.\n".to_owned(),
        1 => "1 step:\n".to_owned(),
        n => format!("{n} steps:\n"),
    });
    for step in steps {
        let price = match (step["price"]["text"].as_str(), step["unpriced"].as_str()) {
            (Some(text), _) => text.to_owned(),
            (None, Some(why)) => format!("priced during the run: {why}"),
            (None, None) => "no charge".to_owned(),
        };
        out.push_str(&format!("  {}  {price}\n", name(&step["node"])));
    }
    if u64::try_from(steps.len()).is_ok_and(|n| n < total_steps) {
        out.push_str(&format!("  … and {} more\n", total_steps.saturating_sub(u64::try_from(steps.len()).unwrap_or(0))));
    }
    let totals: Vec<String> = quote["totals"].as_object().into_iter().flatten().filter_map(|(_, t)| t["text"].as_str().map(str::to_owned)).collect();
    let complete = quote["complete"] == true;
    let total = match (totals.is_empty(), complete) {
        (true, true) => "No charge".to_owned(),
        (true, false) => "Unknown price".to_owned(),
        (false, true) => totals.join(" + "),
        (false, false) => format!("{} so far; some steps are priced during the run", totals.join(" + ")),
    };
    out.push_str(&format!("Total: {total}\n"));
    for reason in strings("blocked").into_iter().chain(strings("blocks")) {
        out.push_str(&format!("Blocked: {reason}\n"));
    }
    if quote["allowed"] != true && quote["blocked"].as_array().is_none_or(Vec::is_empty) && quote["blocks"].as_array().is_none_or(Vec::is_empty) {
        out.push_str("Blocked: the run isn't allowed\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Change, Project};
    use nodecraft_types::{ProjectId, Value as V};

    const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
    const NODE: NodeId = NodeId::from_bytes([3; 16]);

    fn project(canvases: u8, model: &str) -> Project {
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Quote").unwrap();
        for n in 0..canvases {
            Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([n.wrapping_add(1); 16]), "Canvas") }.apply(&mut project).unwrap();
        }
        // Instantiated by its kind, as the canvas adds it, so its parameters are complete.
        let blank = Session::new(project.clone()).unwrap();
        let kinds = blank.kinds();
        let mut values: BTreeMap<String, V> = [("prompt".into(), V::Text("a fox".into()))].into();
        if !model.is_empty() {
            values.insert("model".into(), V::Text(model.into()));
        }
        let node = kinds.get(nodecraft_nodes::generation::KIND).unwrap().instantiate_instance(NODE, Default::default(), &values).unwrap();
        Change::AddNode { canvas: CANVAS, node }.apply(&mut project).unwrap();
        project
    }

    fn first_model(adapter: &str) -> String {
        let session = Session::new(project(1, "")).unwrap();
        let catalog = session.generation_catalog().unwrap().run_catalog();
        catalog.catalog().models().find(|m| m.provider == adapter).unwrap().id.clone()
    }

    #[test]
    fn a_provider_without_a_key_is_named_and_nothing_else_is_built() {
        let mut session = Session::new(project(1, &first_model("openai-images"))).unwrap();
        let mut asked = Vec::new();
        let targets = targets(&session, CANVAS, &[]).unwrap();
        let error = prepare(&mut session, CANVAS, &targets, &mut |adapter| {
            asked.push(adapter.to_owned());
            Err(Readiness::NeedsKey { account: "openai".into() })
        })
        .err()
        .unwrap();
        assert_eq!(asked, vec!["openai-images".to_owned()], "only the adapter the run needs is built");
        assert!(error.message.contains("needs an API key"), "{}", error.message);
    }

    #[test]
    fn a_card_without_a_catalog_model_builds_no_provider_and_still_quotes() {
        let mut session = Session::new(project(1, "")).unwrap();
        let mut asked = 0;
        prepare(&mut session, CANVAS, &[NODE], &mut |_| {
            asked += 1;
            Err(Readiness::NotSetUp("unused".into()))
        })
        .unwrap();
        assert_eq!(asked, 0);
        let quote = quote(&mut session, CANVAS, &[NODE]).unwrap();
        let text = quote_text(&session, CANVAS, &quote);
        assert!(text.contains("Total:"), "{text}");
    }

    #[test]
    fn an_unknown_node_is_refused() {
        let session = Session::new(project(1, "")).unwrap();
        let error = targets(&session, CANVAS, &[NodeId::from_bytes([7; 16])]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
    }

    /// The mock provider stands in for a ready adapter: the catalog prices the card.
    fn ready(adapter: &str) -> Result<Arc<dyn Provider>, Readiness> {
        use nodecraft_testkit::mock::{MockModel, MockOutput, MockProvider};
        Ok(Arc::new(MockProvider::new(adapter, [(first_model(adapter), MockModel::new(MockOutput::Image))])))
    }

    #[test]
    fn a_ready_provider_prices_the_run() {
        // Seedream's first model has one price per call.
        let mut session = Session::new(project(1, &first_model("seedream"))).unwrap();
        let targets = targets(&session, CANVAS, &[]).unwrap();
        assert_eq!(targets, vec![NODE], "a never-run card is quoted by default");
        prepare(&mut session, CANVAS, &targets, &mut ready).unwrap();
        let quote = quote(&mut session, CANVAS, &targets).unwrap();
        assert_eq!(quote["steps"].as_array().map(Vec::len), Some(1), "{quote}");
        assert!(quote["totals"]["USD"]["micros"].as_u64().is_some_and(|m| m > 0), "{quote}");
        let text = quote_text(&session, CANVAS, &quote);
        assert!(text.contains("1 step:") && !text.contains("Total: No charge") && !text.contains("Unknown"), "{text}");
    }

    #[test]
    fn by_default_a_card_with_a_current_result_needs_no_key() {
        let mut project = project(1, &first_model("openai-images"));
        let generation = nodecraft_types::GenerationId::from_bytes([5; 16]);
        // No input hash: a result recorded that way is never stale.
        let made = nodecraft_graph::Generation::new(generation, Vec::new());
        Change::AddGeneration { canvas: CANVAS, id: NODE, index: 0, generation: made }.apply(&mut project).unwrap();
        Change::SetActiveGeneration { canvas: CANVAS, id: NODE, generation: Some(generation) }.apply(&mut project).unwrap();
        let mut session = Session::new(project).unwrap();
        let targets = targets(&session, CANVAS, &[]).unwrap();
        assert!(targets.is_empty(), "{targets:?}");
        let mut asked = 0;
        prepare(&mut session, CANVAS, &targets, &mut |_| {
            asked += 1;
            Err(Readiness::NeedsKey { account: "openai".into() })
        })
        .unwrap();
        assert_eq!(asked, 0, "no adapter is built for a card that won't run");
        let quote = quote(&mut session, CANVAS, &targets).unwrap();
        let text = quote_text(&session, CANVAS, &quote);
        assert!(text.starts_with("Nothing to run."), "{text}");
    }

    #[test]
    fn several_canvases_need_a_choice() {
        let one = Session::new(project(1, "")).unwrap();
        assert_eq!(canvas_of(&one, None).unwrap(), CANVAS);
        assert!(canvas_of(&one, Some("not-an-id")).is_err());
        let two = Session::new(project(2, "")).unwrap();
        assert!(canvas_of(&two, None).unwrap_err().message.contains("--canvas"));
        let second = CanvasId::from_bytes([2; 16]);
        assert_eq!(canvas_of(&two, Some(&second.to_string())).unwrap(), second);
    }
}
