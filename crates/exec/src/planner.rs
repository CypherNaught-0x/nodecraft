//! The run planner: from the nodes a user asked to run to a [`RunPlan`] of steps in dependency
//! order, or the list of inputs that keep it from running yet.
//!
//! Planning is pure: it reads the canvas, the node kinds and the stale set, and never touches
//! providers, files or the clock. Each [`Step`] is one node to run. Its inputs come either from
//! values that already exist (an upstream node's active result, or a source node's content) or
//! from another step of the same run; a step whose inputs are all known carries its
//! [`JobSpec`]s now, the others are planned by the driver once their upstream steps finish.
//!
//! Routers (#72), techniques (#94) and batches (#92) are not node kinds yet; when they land they
//! are resolved here, before inputs are gathered.

use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, InputValue, JobSpec, ModeId, PlanCtx, Registry};
use nodecraft_types::{Error, ErrorKind, FieldId, NodeId, PortType, Value};
use std::collections::{BTreeMap, BTreeSet};

/// Which nodes a run covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// Exactly the requested nodes; every upstream node outside the run must already have a
    /// result ("can't run yet" otherwise).
    Selected,
    /// The requested nodes plus every ancestor that is stale or has no result yet.
    WithUpstream,
    /// Every node in the field; upstream nodes outside it must already have results.
    Field(FieldId),
    /// Every stale node on the canvas, with the ancestors it needs (as [`RunMode::WithUpstream`]).
    AllStale,
}

/// Where one value for an input comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum InputSource {
    /// Already known: an upstream node's active result or a source node's content.
    Value(Value),
    /// The output `port` of the step for `node`, produced earlier in this run. When `list` is
    /// set (a list output) and the input is a list too, its items are spliced in, not nested.
    Step { node: NodeId, port: String, list: bool },
}

/// One connected input port of a step. List inputs (`many`) have one source per noodle, in
/// input order; single inputs have exactly one.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedInput {
    pub port: String,
    pub port_type: PortType,
    pub from: Vec<InputSource>,
}

/// One node to run.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub node: NodeId,
    pub kind: String,
    pub mode: ModeId,
    /// Steps of this run that must finish first.
    pub after: Vec<NodeId>,
    pub inputs: Vec<PlannedInput>,
    /// The node's jobs when every input is already known; `None` until its upstream steps
    /// have produced their outputs.
    pub jobs: Option<Vec<JobSpec>>,
}

/// A plan that can run: steps in dependency order. Its price is `cost::quote` (#76), the one
/// place runs are priced.
#[derive(Clone, Debug, PartialEq)]
pub struct RunPlan {
    pub steps: Vec<Step>,
    /// Dependencies between steps, `(before, after)`.
    pub edges: Vec<(NodeId, NodeId)>,
}

/// Static target-selection failures; never contains provider or native hook text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetReason {
    MissingSelection,
    Mismatch,
}
impl TargetReason {
    pub fn message(self) -> &'static str {
        match self {
            Self::MissingSelection => "no provider endpoint is selected for this node",
            Self::Mismatch => "generated work does not match the selected provider endpoint",
        }
    }
}

/// Why a node can't run yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reason {
    /// `upstream` has no result and isn't part of this run.
    NoResult,
    /// The node's kind isn't installed.
    UnknownKind,
    /// The kind has no mode for the connected input types.
    UnsupportedInputs,
    /// The kind rejected the node (e.g. a required parameter is empty).
    Invalid(String),
    /// Checked endpoint selection failed.
    ProviderTarget(TargetReason),
}

/// A node that blocks the run, with the input and upstream node to highlight where they apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocker {
    pub node: NodeId,
    pub port: Option<String>,
    pub upstream: Option<NodeId>,
    pub reason: Reason,
}

impl Blocker {
    /// A message for the "can't run yet" notice.
    pub fn message(&self, canvas: &Canvas) -> String {
        let name = |id: NodeId| {
            canvas.nodes().get(&id).map_or_else(|| "a missing node".to_owned(), |n| if n.title.is_empty() { n.kind.clone() } else { n.title.clone() })
        };
        let node = name(self.node);
        match (&self.reason, &self.port, self.upstream) {
            (Reason::NoResult, Some(port), Some(up)) => format!("Can't run {node} yet: its {port} input needs a result from {}.", name(up)),
            (Reason::NoResult, ..) => format!("Can't run {node} yet: an input has no result."),
            (Reason::UnknownKind, _, Some(up)) if up != self.node => format!("Can't run {node}: the node kind of {} isn't available.", name(up)),
            (Reason::UnknownKind, ..) => format!("Can't run {node}: its node kind isn't available."),
            (Reason::UnsupportedInputs, ..) => format!("Can't run {node}: it doesn't accept this combination of inputs."),
            (Reason::ProviderTarget(reason), ..) => format!("Can't run {node}: {}.", reason.message()),
            (Reason::Invalid(message), _, Some(up)) if up != self.node => format!("Can't run {node}: {} has a problem: {message}", name(up)),
            (Reason::Invalid(message), ..) => format!("Can't run {node}: {message}"),
        }
    }
}

/// The outcome of planning a valid request.
#[derive(Clone, Debug, PartialEq)]
pub enum Planned {
    Ready(RunPlan),
    Blocked(Vec<Blocker>),
}

/// Most items one list input is planned with (the document's own list limit).
pub const MAX_LIST_ITEMS: usize = nodecraft_graph::MAX_VALUE_ITEMS;
/// Most bytes of known input values one step is planned with. A hostile project can wire one
/// large value into a list thousands of times; this keeps planning bounded.
pub const MAX_STEP_INPUT_BYTES: usize = 64 * 1024 * 1024;
/// Most bytes of known input values across a whole plan, however many steps share them.
pub const MAX_PLAN_INPUT_BYTES: usize = 256 * 1024 * 1024;
/// Most blockers reported for one request; the first ones are what the user acts on.
pub const MAX_BLOCKERS: usize = 256;

/// Plan a run. `requested` is used by [`RunMode::Selected`] and [`RunMode::WithUpstream`] and
/// ignored otherwise. `stale` holds the nodes whose results no longer match their inputs (#75);
/// it must already include everything downstream of a change, because a fresh ancestor stops
/// the upstream walk.
///
/// Errors are for malformed requests (unknown node ids, nothing requested, a cyclic canvas); a
/// well-formed request that can't run yet is [`Planned::Blocked`]. A field or "all stale" run
/// with nothing in it is an empty plan.
///
/// Kinds' [`NodeKind::mode`](nodecraft_nodes::NodeKind::mode) is asked with the node's own
/// connected input port types, in `node.inputs` order (list ports with `many: true`).
pub fn plan(canvas: &Canvas, kinds: &Registry, mode: &RunMode, requested: &BTreeSet<NodeId>, stale: &BTreeSet<NodeId>) -> Result<Planned, Error> {
    plan_with_targets(canvas, kinds, mode, requested, stale, &nodecraft_types::EMPTY_TARGET_SELECTION)
}

/// Plan with the immutable endpoint view supplied before any kind hook.
pub fn plan_with_targets(
    canvas: &Canvas,
    kinds: &Registry,
    mode: &RunMode,
    requested: &BTreeSet<NodeId>,
    stale: &BTreeSet<NodeId>,
    targets: &nodecraft_types::TargetSelection,
) -> Result<Planned, Error> {
    let graph = Graph::new(canvas, kinds, targets)?;
    let run: BTreeSet<NodeId> = match mode {
        RunMode::Selected | RunMode::WithUpstream => {
            if let Some(missing) = requested.iter().find(|id| !canvas.nodes().contains_key(id)) {
                return Err(Error::new(ErrorKind::NotFound, format!("node {missing} is not on this canvas")));
            }
            if requested.is_empty() {
                return Err(Error::new(ErrorKind::InvalidInput, "there is nothing to run"));
            }
            if *mode == RunMode::Selected { requested.clone() } else { graph.with_upstream(requested, stale) }
        }
        RunMode::Field(field) => canvas.nodes().values().filter(|n| n.field == Some(*field)).map(|n| n.id).collect(),
        RunMode::AllStale => graph.with_upstream(&stale.iter().copied().filter(|id| canvas.nodes().contains_key(id)).collect(), stale),
    };
    // Routers never run: they are resolved into their consumers' inputs.
    let requested_only_routers =
        matches!(mode, RunMode::Selected | RunMode::WithUpstream) && !run.is_empty() && run.iter().all(|id| crate::routing::is_router(canvas, *id));
    let run: BTreeSet<NodeId> = run.into_iter().filter(|id| !crate::routing::is_router(canvas, *id)).collect();
    if requested_only_routers {
        return Err(Error::new(ErrorKind::InvalidInput, "routers pass inputs through and have nothing to run"));
    }
    let order = graph.order(&run)?;
    let mut steps = Vec::with_capacity(order.len());
    let mut edges = BTreeSet::new();
    let mut blockers: Vec<Blocker> = Vec::new();
    let mut cache = BTreeMap::new();
    let mut plan_bytes = 0usize;
    for id in order {
        if blockers.len() >= MAX_BLOCKERS {
            break;
        }
        match graph.step(id, &run, &mut cache, &mut plan_bytes) {
            Ok(step) => {
                edges.extend(step.after.iter().map(|before| (*before, id)));
                steps.push(step);
            }
            Err(found) => {
                blockers.extend(found);
                blockers.truncate(MAX_BLOCKERS);
                // Every later step would repeat the same plan-wide budget blocker.
                if plan_bytes > MAX_PLAN_INPUT_BYTES {
                    break;
                }
            }
        }
    }
    if !blockers.is_empty() {
        return Ok(Planned::Blocked(blockers));
    }
    Ok(Planned::Ready(RunPlan { steps, edges: edges.into_iter().collect() }))
}

/// Values already resolved for an upstream `(node, output port)`, with whether that output is a
/// list and the value's size (measured once, so the budget is checked before any copy).
type Cache = BTreeMap<(NodeId, String), Result<(Value, bool, usize), Reason>>;

/// Edges indexed by target, once per plan.
struct Graph<'a> {
    canvas: &'a Canvas,
    kinds: &'a Registry,
    targets: &'a nodecraft_types::TargetSelection,
    /// Inputs of each node: (port, upstream node, upstream port, order).
    into: BTreeMap<NodeId, Vec<(&'a str, NodeId, &'a str, u32)>>,
    routing: crate::routing::Incoming<'a>,
}

impl<'a> Graph<'a> {
    fn new(canvas: &'a Canvas, kinds: &'a Registry, targets: &'a nodecraft_types::TargetSelection) -> Result<Self, Error> {
        // Routers are resolved away: consumers read the routers' own sources.
        let mut routing = crate::routing::incoming(canvas, kinds)?;
        let into = std::mem::take(&mut routing.feeds)
            .into_iter()
            .map(|(target, feeds)| (target, feeds.into_iter().map(|(port, position, from, from_port)| (port, from, from_port, position)).collect()))
            .collect();
        Ok(Self { canvas, kinds, targets, into, routing })
    }

    fn upstream(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.into.get(&id).into_iter().flatten().map(|(_, from, _, _)| *from)
    }

    fn is_source(&self, node: &Node) -> bool {
        self.kinds.get(&node.kind).is_ok_and(|kind| kind.descriptor().category == Category::Source)
    }

    /// The requested nodes plus ancestors that need a run, walking through ancestors that have
    /// a fresh result no further (their result is used as is). Source nodes are resolved inline
    /// and never become steps unless requested.
    fn with_upstream(&self, requested: &BTreeSet<NodeId>, stale: &BTreeSet<NodeId>) -> BTreeSet<NodeId> {
        let mut run = requested.clone();
        let mut stack: Vec<NodeId> = requested.iter().copied().collect();
        while let Some(id) = stack.pop() {
            for up in self.upstream(id) {
                let Some(node) = self.canvas.nodes().get(&up) else { continue };
                let needs_run = !self.is_source(node) && (stale.contains(&up) || usable_outputs(node).is_none());
                if needs_run && run.insert(up) {
                    stack.push(up);
                }
            }
        }
        run
    }

    /// The run's nodes in dependency order (Kahn's algorithm, smallest id first for a
    /// deterministic plan).
    fn order(&self, run: &BTreeSet<NodeId>) -> Result<Vec<NodeId>, Error> {
        let mut waiting: BTreeMap<NodeId, usize> = run.iter().map(|id| (*id, 0)).collect();
        let mut next: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
        for id in run {
            for up in self.upstream(*id).filter(|up| run.contains(up)) {
                if next.entry(up).or_default().insert(*id)
                    && let Some(count) = waiting.get_mut(id)
                {
                    *count = count.saturating_add(1);
                }
            }
        }
        let mut ready: BTreeSet<NodeId> = waiting.iter().filter(|(_, n)| **n == 0).map(|(id, _)| *id).collect();
        let mut order = Vec::with_capacity(run.len());
        while let Some(id) = ready.pop_first() {
            order.push(id);
            for after in next.get(&id).into_iter().flatten() {
                if let Some(count) = waiting.get_mut(after) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        ready.insert(*after);
                    }
                }
            }
        }
        if order.len() != run.len() {
            return Err(Error::new(ErrorKind::InvalidInput, "the nodes to run form a loop"));
        }
        Ok(order)
    }

    /// Plan one node. `plan_bytes` is the running total of known input bytes across the plan.
    fn step(&self, id: NodeId, run: &BTreeSet<NodeId>, cache: &mut Cache, plan_bytes: &mut usize) -> Result<Step, Vec<Blocker>> {
        let blocker = |port: Option<&str>, upstream: Option<NodeId>, reason: Reason| Blocker { node: id, port: port.map(str::to_owned), upstream, reason };
        let node = self.canvas.nodes().get(&id).ok_or_else(|| vec![blocker(None, None, Reason::UnknownKind)])?;
        let kind = self.kinds.get(&node.kind).map_err(|_| vec![blocker(None, None, Reason::UnknownKind)])?;
        let mut blockers = Vec::new();
        // Required parameters and port drift are checked for every step now, so a downstream
        // problem never surfaces after an upstream paid step has run.
        if let Err(error) = kind.validate_instance(node, true) {
            blockers.push(blocker(None, None, Reason::Invalid(error.message)));
        }
        let mut inputs = Vec::new();
        let mut after = BTreeSet::new();
        let mut bytes = 0usize;
        // One blocker per (port, upstream), however many noodles repeat it.
        let mut reported = BTreeSet::new();
        for port in &node.inputs {
            if let Some(router) = self.routing.invalid.get(&(id, port.name.as_str())) {
                let reason = self.routing.invalid_routers.get(router).copied().unwrap_or("invalid upstream router");
                blockers.push(blocker(Some(&port.name), Some(*router), Reason::Invalid(reason.into())));
                if blockers.len() >= MAX_BLOCKERS {
                    return Err(blockers);
                }
                continue;
            }
            let feeds: Vec<_> = self.into.get(&id).into_iter().flatten().filter(|(p, ..)| *p == port.name).collect();
            if feeds.is_empty() {
                // Wired, but only to routers with nothing behind them: don't silently run as if
                // the input were unconnected (another mode could drop it).
                if self.routing.wired.contains(&(id, port.name.as_str())) {
                    blockers.push(blocker(Some(&port.name), None, Reason::Invalid(format!("its {} input is fed by a router with no inputs", port.name))));
                }
                continue;
            }
            // A list can't reach a single input directly (the graph refuses it); through a
            // router it could.
            let list_into_single = |list: bool| list && !port.port_type.many;
            let mut from = Vec::with_capacity(feeds.len().min(MAX_LIST_ITEMS));
            for (_, up, up_port, _) in feeds {
                let upstream = self.canvas.nodes().get(up);
                if run.contains(up) && !upstream.is_some_and(|u| self.is_source(u)) {
                    after.insert(*up);
                    let list = upstream.and_then(|u| u.outputs.iter().find(|p| p.name == *up_port)).is_some_and(|p| p.port_type.many);
                    if list_into_single(list) {
                        blockers.push(blocker(
                            Some(&port.name),
                            Some(*up),
                            Reason::Invalid(format!("its {} input takes one value but a list reaches it through a router", port.name)),
                        ));
                        break;
                    }
                    from.push(InputSource::Step { node: *up, port: (*up_port).to_owned(), list });
                } else {
                    let resolved = cache.entry((*up, (*up_port).to_owned())).or_insert_with(|| {
                        self.resolve(*up, up_port).map(|(value, list)| {
                            let size = value_bytes(&value);
                            (value, list, size)
                        })
                    });
                    match resolved {
                        Ok((value, list, size)) => {
                            let (list, size) = (*list, *size);
                            if list_into_single(list) {
                                blockers.push(blocker(
                                    Some(&port.name),
                                    Some(*up),
                                    Reason::Invalid(format!("its {} input takes one value but a list reaches it through a router", port.name)),
                                ));
                                break;
                            }
                            bytes = bytes.saturating_add(size);
                            *plan_bytes = plan_bytes.saturating_add(size);
                            if bytes > MAX_STEP_INPUT_BYTES || *plan_bytes > MAX_PLAN_INPUT_BYTES {
                                let why = if bytes > MAX_STEP_INPUT_BYTES {
                                    "its inputs are too large to run in one step"
                                } else {
                                    "the inputs of this run are too large to plan at once"
                                };
                                blockers.push(blocker(Some(&port.name), None, Reason::Invalid(why.into())));
                                return Err(blockers);
                            }
                            // Within budget: only now copy. A list output into a list input
                            // contributes its items, not a nested list.
                            match value {
                                Value::List(items) if list && port.port_type.many => from.extend(items.iter().cloned().map(InputSource::Value)),
                                value => from.push(InputSource::Value(value.clone())),
                            }
                        }
                        Err(reason) => {
                            if reported.insert((port.name.as_str(), *up)) {
                                blockers.push(blocker(Some(&port.name), Some(*up), reason.clone()));
                            }
                        }
                    }
                }
                // A single input takes one value; through a router it could receive several.
                if !port.port_type.many && from.len() > 1 {
                    blockers.push(blocker(
                        Some(&port.name),
                        None,
                        Reason::Invalid(format!("its {} input takes one value but receives several through a router", port.name)),
                    ));
                    break;
                }
                // The same item cap for values and for outputs of other steps in this run.
                if from.len() > MAX_LIST_ITEMS {
                    blockers.push(blocker(Some(&port.name), None, Reason::Invalid(format!("its {} input has more than {MAX_LIST_ITEMS} items", port.name))));
                    break;
                }
            }
            inputs.push(PlannedInput { port: port.name.clone(), port_type: port.port_type, from });
        }
        let connected: Vec<PortType> = inputs.iter().map(|input| input.port_type).collect();
        let mode = match kind.instance_mode(node, &connected) {
            Ok(Some(mode)) => Some(mode),
            Ok(None) => {
                blockers.push(blocker(None, None, Reason::UnsupportedInputs));
                None
            }
            Err(error) => {
                blockers.push(blocker(None, None, Reason::Invalid(error.message)));
                None
            }
        };
        let (Some(mode), true) = (mode, blockers.is_empty()) else {
            return Err(blockers);
        };
        let jobs = match known_inputs(&inputs) {
            Some(values) => {
                let jobs = kind
                    .plan(&PlanCtx { targets: self.targets, node, inputs: &values })
                    .map_err(|error| vec![blocker(None, None, Reason::Invalid(error.message))])?;
                crate::generation_contract::validate_targets(node, &jobs, self.targets)
                    .map_err(|reason| vec![blocker(None, None, Reason::ProviderTarget(reason))])?;
                crate::generation_contract::validate_outputs(node, &jobs).map_err(|reason| vec![blocker(None, None, Reason::Invalid(reason.into()))])?;
                Some(jobs)
            }
            None => None,
        };
        Ok(Step { node: id, kind: node.kind.clone(), mode, after: after.into_iter().collect(), inputs, jobs })
    }

    /// The value an upstream node provides on `port` without running: a source's content, or
    /// the active result of anything else (also when its kind isn't installed).
    fn resolve(&self, id: NodeId, port: &str) -> Result<(Value, bool), Reason> {
        let node = self.canvas.nodes().get(&id).ok_or(Reason::NoResult)?;
        let index = node.outputs.iter().position(|p| p.name == port).ok_or(Reason::NoResult)?;
        let list = node.outputs.get(index).is_some_and(|p| p.port_type.many);
        if let Ok(kind) = self.kinds.get(&node.kind)
            && kind.descriptor().category == Category::Source
        {
            let jobs = kind.plan(&PlanCtx { targets: self.targets, node, inputs: &[] }).map_err(|error| Reason::Invalid(error.message))?;
            return jobs
                .into_iter()
                .find_map(|job| match job {
                    JobSpec::Source { output, value, .. } if output == port => Some((value, list)),
                    JobSpec::Source { .. } | JobSpec::Generate { .. } | JobSpec::Action { .. } => None,
                })
                .ok_or(Reason::NoResult);
        }
        let outputs = usable_outputs(node).ok_or(Reason::NoResult)?;
        outputs.get(index).cloned().map(|value| (value, list)).ok_or(Reason::NoResult)
    }
}

/// The outputs of a node's active generation, if it has one.
fn active_outputs(node: &Node) -> Option<&[Value]> {
    let active = node.active?;
    node.history.iter().find(|g| g.id == active).map(|g| g.outputs.as_slice())
}

/// The active outputs when they can be used: results are positional, so one recorded for a
/// different port layout is not. The upstream walk and resolution share this, so a node whose
/// result can't be used is re-run rather than waited on.
fn usable_outputs(node: &Node) -> Option<&[Value]> {
    active_outputs(node).filter(|outputs| outputs.len() == node.outputs.len())
}

/// Approximate in-memory size of a value, iteratively (lists can nest): every value, container
/// or not, costs its own storage, plus its text or media metadata.
pub(crate) fn value_bytes(value: &Value) -> usize {
    let mut total = 0usize;
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        total = total.saturating_add(std::mem::size_of::<Value>()).saturating_add(match value {
            Value::Text(text) => text.len(),
            Value::List(items) => {
                stack.extend(items.iter());
                0
            }
            Value::Media(asset) => asset.mime.as_str().len().saturating_add(64),
            _ => 0,
        });
    }
    total
}

/// Input values for [`NodeKind::plan`](nodecraft_nodes::NodeKind::plan) when nothing comes from
/// a step: one per connected port, a [`Value::List`] for list inputs.
fn known_inputs(inputs: &[PlannedInput]) -> Option<Vec<InputValue>> {
    inputs
        .iter()
        .map(|input| {
            let mut values = input.from.iter().map(|source| match source {
                InputSource::Value(value) => Some(value.clone()),
                InputSource::Step { .. } => None,
            });
            let value = if input.port_type.many { Value::List(values.collect::<Option<Vec<_>>>()?) } else { values.next()?? };
            Some(InputValue { port: input.port.clone(), port_type: input.port_type, value })
        })
        .collect()
}

#[cfg(test)]
mod tests;
