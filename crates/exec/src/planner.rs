//! The run planner: from the nodes a user asked to run to a [`RunPlan`] of steps in dependency
//! order, or the list of inputs that keep it from running yet.
//!
//! Planning is pure: it reads the canvas, the node kinds and the stale set, and never touches
//! providers, files or the clock. Each [`Step`] is one node to run. Its inputs come either from
//! values that already exist (an upstream node's active result, or a source node's content) or
//! from another step of the same run; a step whose inputs are all known carries its
//! [`JobSpec`]s now, the others are planned by the driver once their upstream steps finish.
//!
//! Routers (#72) and batches (#92) are resolved here, before inputs are gathered: a router into
//! its sources, a batch into its items. A node with a single input fed by a batch, or a list
//! input its contract marks as fanning out (a generation's prompt, #515), is planned once per
//! item ([`Fanout`], combined by [`crate::fanout`]). Technique nodes (#94) are inlined first
//! ([`crate::technique`]): their copied nodes run as steps of the run, before the technique
//! node's own step gives their results back.

use crate::technique::{Collect, Expanded};
use nodecraft_graph::{Canvas, ItemTag, Node, Port};
use nodecraft_nodes::{Category, InputValue, JobSpec, ModeId, PlanCtx, Registry, batch, mention, technique};
use nodecraft_types::{Error, ErrorKind, FieldId, Modality, NodeId, PortType, Value};
use std::collections::{BTreeMap, BTreeSet};

mod options;

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
    /// Value `index` of the output `port` of the fanned step for `node` (#92): one item's value.
    /// When `list` is set (one list per item, #519) and the input is a list too, that item's
    /// values are spliced in.
    Item { node: NodeId, port: String, index: usize, list: bool },
}

/// One connected input port of a step. List inputs (`many`) have one source per noodle, in
/// input order; single inputs have exactly one.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedInput {
    pub port: String,
    pub port_type: PortType,
    pub from: Vec<InputSource>,
    /// The upstream node behind each entry of `from` (past any router), in the same order.
    pub origins: Vec<NodeId>,
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
    /// have produced their outputs. A fanned step's jobs are its items' jobs, in item order.
    pub jobs: Option<Vec<JobSpec>>,
    /// Set when batches fan this step out (#92); `inputs` is then empty.
    pub fanout: Option<Fanout>,
    /// How many variations it makes (#81): 1, or more for a generation node asked for several.
    /// Its `jobs` are then one variation's jobs repeated this many times, in variation order.
    pub variations: usize,
}

/// A step that runs once per batch item combination (#92).
#[derive(Clone, Debug, PartialEq)]
pub struct Fanout {
    /// Which items each run is for; the result records them in this order.
    pub items: Vec<ItemTag>,
    /// Each item's inputs, in `items` order.
    pub cells: Vec<Vec<PlannedInput>>,
    /// When the step's jobs are known: how many of them belong to each item.
    pub jobs: Vec<usize>,
    /// Completing the node's partial result (#480): the items it finished have no jobs here.
    pub resume: Option<crate::partial::Resume>,
}

impl Fanout {
    /// How many of its items run: all of them, or only those its resume plans.
    pub fn running(&self) -> usize {
        self.resume.as_ref().map_or(self.items.len(), |resume| resume.missing.len())
    }
}

/// A plan that can run: steps in dependency order. Its price is `cost::quote` (#76), the one
/// place runs are priced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunPlan {
    pub steps: Vec<Step>,
    /// Dependencies between steps, `(before, after)`.
    pub edges: Vec<(NodeId, NodeId)>,
    /// Steps for nodes inlined from techniques (#94), each with the technique node on the real
    /// canvas it runs for. Their results are never recorded; a failure fails that node.
    pub inner: BTreeMap<NodeId, NodeId>,
}

impl RunPlan {
    /// Fanned steps planned during the run whose node has a partial result it may complete
    /// (#518): when what feeds them is made again unchanged, only their missing items run, so the
    /// quote, which counts every item, is an upper bound for them.
    pub fn resumable<'a>(&'a self, canvas: &'a Canvas) -> impl Iterator<Item = NodeId> + 'a {
        self.steps
            .iter()
            .filter(|step| step.jobs.is_none() && !self.inner.contains_key(&step.node) && !self.inner.values().any(|owner| *owner == step.node))
            .filter(move |step| {
                step.fanout.as_ref().is_some_and(|fanout| fanout.resume.is_none() && crate::partial::completable(canvas, step.node, &fanout.items).is_some())
            })
            .map(|step| step.node)
    }
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
    /// Validation from the planner or a registered built-in implementation; public-safe.
    PublicInvalid(String),
    /// A parameter's value is not on the run-time list the host fetched for it (#162). Carries
    /// nothing on purpose: the blocked report a CLI, agent or the shell's notice is built from is
    /// fixed-shape records that never copy a contract, kind or model string. `node.describe` and
    /// `node.options` name the parameter, per node, when a caller needs which one.
    NotAnOption,
    /// Checked endpoint selection failed.
    ProviderTarget(TargetReason),
}

impl Reason {
    /// Bounded public validation text. Arbitrary native callbacks never supply public text.
    pub fn validation_message(&self) -> &str {
        let Self::PublicInvalid(message) = self else {
            return "check node parameters, connections and installed kind";
        };
        let mut end = message.len().min(1024);
        while !message.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        message.get(..end).unwrap_or("")
    }
}

fn invalid_kind(kinds: &Registry, kind: &str, message: String) -> Reason {
    if kinds.has_public_errors(kind) { Reason::PublicInvalid(message) } else { Reason::Invalid(message) }
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
            // `plugin.` is nodecraft_plugin's PLUGIN_PREFIX; exec doesn't depend on that crate.
            (Reason::UnknownKind, ..) if canvas.nodes().get(&self.node).is_some_and(|n| n.kind.starts_with("plugin.")) => {
                if cfg!(target_arch = "wasm32") {
                    format!("Can't run {node}: plugins can't run in the browser yet.")
                } else {
                    format!("Can't run {node}: its plugin isn't installed, granted or enabled here.")
                }
            }
            (Reason::UnknownKind, ..) => format!("Can't run {node}: its node kind isn't available."),
            (Reason::UnsupportedInputs, ..) => format!("Can't run {node}: it doesn't accept this combination of inputs."),
            (Reason::ProviderTarget(reason), ..) => format!("Can't run {node}: {}.", reason.message()),
            (Reason::NotAnOption, ..) => {
                format!("Can't run {node}: a parameter is not one of the available options; refresh the list or pick another.")
            }
            (Reason::Invalid(_) | Reason::PublicInvalid(_), _, Some(up)) if up != self.node => {
                format!("Can't run {node}: {} has a problem: {}", name(up), self.reason.validation_message())
            }
            (Reason::Invalid(_) | Reason::PublicInvalid(_), ..) => format!("Can't run {node}: {}", self.reason.validation_message()),
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

/// Plan with the immutable endpoint view supplied before any kind hook, and no run-time option
/// lists: [`plan_with_options`] with an empty [`nodecraft_types::NoOptions`], so nothing is
/// refused for its options. Live local recompute and a technique's publish check plan through
/// this; a run that can cost money plans through [`plan_expanded`] with the host's lists.
pub fn plan_with_targets(
    canvas: &Canvas,
    kinds: &Registry,
    mode: &RunMode,
    requested: &BTreeSet<NodeId>,
    stale: &BTreeSet<NodeId>,
    targets: &nodecraft_types::TargetSelection,
) -> Result<Planned, Error> {
    plan_with_options(canvas, kinds, mode, requested, stale, targets, &nodecraft_types::NoOptions)
}

/// Plan with the endpoint view and the option lists the host has fetched (#162), on a canvas
/// with no technique nodes to inline: [`plan_expanded`] over the canvas as is.
pub fn plan_with_options(
    canvas: &Canvas,
    kinds: &Registry,
    mode: &RunMode,
    requested: &BTreeSet<NodeId>,
    stale: &BTreeSet<NodeId>,
    targets: &nodecraft_types::TargetSelection,
    options: &dyn nodecraft_types::OptionLookup,
) -> Result<Planned, Error> {
    plan_expanded(&Expanded::plain(canvas), kinds, mode, requested, stale, targets, options)
}

/// Plan on a canvas with its technique nodes inlined ([`crate::technique::expand`]). `stale`
/// holds nodes of the real canvas; inlined nodes have no results of their own, so they run.
///
/// `options` answers what the host knows of a parameter's run-time list; a value off a list it
/// *has* fetched blocks its node with [`Reason::NotAnOption`] here, before any job is admitted,
/// quoted or charged, and before an idempotency key exists. A list it has not fetched, could not
/// fetch, or whose scope parameter has no value refuses nothing: unknown is not empty, and the
/// provider stays the authority when the job is submitted; the check itself is the `options`
/// module. Inlined technique nodes carry their params, so they are checked the same way.
pub fn plan_expanded(
    expanded: &Expanded<'_>,
    kinds: &Registry,
    mode: &RunMode,
    requested: &BTreeSet<NodeId>,
    stale: &BTreeSet<NodeId>,
    targets: &nodecraft_types::TargetSelection,
    options: &dyn nodecraft_types::OptionLookup,
) -> Result<Planned, Error> {
    let canvas: &Canvas = &expanded.canvas;
    let graph = Graph::new(canvas, kinds, targets, &expanded.collects, &expanded.inner, options)?;
    let run: BTreeSet<NodeId> = match mode {
        RunMode::Selected | RunMode::WithUpstream => {
            if let Some(missing) = requested.iter().find(|id| !canvas.nodes().contains_key(id) || expanded.inner.contains_key(id)) {
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
    let run = graph.with_inlined(run, stale, &expanded.inner);
    // Routers and batches never run: they are resolved into their consumers' inputs.
    let resolved_away = |id: &NodeId| crate::routing::is_router(canvas, *id) || is_batch(canvas, *id);
    let requested_only_routers = matches!(mode, RunMode::Selected | RunMode::WithUpstream) && !run.is_empty() && run.iter().all(resolved_away);
    let run: BTreeSet<NodeId> = run.into_iter().filter(|id| !resolved_away(id)).collect();
    if requested_only_routers {
        return Err(Error::new(ErrorKind::InvalidInput, "routers and batches pass inputs through and have nothing to run"));
    }
    let order = graph.order(&run)?;
    let mut steps = Vec::with_capacity(order.len());
    let mut edges = BTreeSet::new();
    // A technique node that can't be inlined blocks only the runs it is in.
    let mut blockers: Vec<Blocker> = expanded.blockers.iter().filter(|b| run.contains(&b.node)).cloned().collect();
    let not_inlined: BTreeSet<NodeId> = expanded.blockers.iter().map(|b| b.node).collect();
    let mut state = State::default();
    for id in order {
        if blockers.len() >= MAX_BLOCKERS {
            break;
        }
        if not_inlined.contains(&id) {
            continue;
        }
        match graph.step(id, &run, &mut state) {
            Ok(step) => {
                edges.extend(step.after.iter().map(|before| (*before, id)));
                if let Some(fanout) = &step.fanout {
                    state.fanned.insert(id, fanout.items.clone());
                }
                steps.push(step);
            }
            Err(found) => {
                blockers.extend(found);
                blockers.truncate(MAX_BLOCKERS);
                // Every later step would repeat the same plan-wide budget blocker.
                if state.plan_bytes > MAX_PLAN_INPUT_BYTES {
                    break;
                }
            }
        }
    }
    if !blockers.is_empty() {
        return Ok(Planned::Blocked(blockers.into_iter().map(|blocker| owned(blocker, expanded)).collect()));
    }
    crate::partial::resume_partials(canvas, kinds, &mut steps);
    let inner = steps.iter().filter_map(|step| Some((step.node, *expanded.inner.get(&step.node)?))).collect();
    crate::partial::resume_techniques(canvas, kinds, &mut steps, &inner);
    Ok(Planned::Ready(RunPlan { steps, edges: edges.into_iter().collect(), inner }))
}

/// The `requested` nodes that would each plan [`Planned::Ready`] on their own with
/// [`RunMode::WithUpstream`] (as [`plan`] plans), every step of that plan passing `accept`.
///
/// One graph and one walk answer for all of them, where planning each alone rebuilds the graph
/// per node and costs `O(nodes²)` on every edit (#549). When every requested node needs a run
/// itself (it is stale or has no result, as live recompute asks), their runs together are the
/// union of their runs alone; a blocked or refused step fails everything after it in the run.
/// The answer is the same as planning each alone or stricter, never looser:
/// - a batch refuses to collect from a node in the run, so a node behind a batch whose stale
///   source another requested node reruns waits for that rerun (alone it would read the old
///   result; together the two could never plan at all);
/// - the steps share the plan-wide input budget, so near it fewer nodes pass;
/// - a requested node that doesn't need a run joins the others' runs, where alone they would read
///   its result, so they pass only if it does.
///
/// Routers and batches never pass: they have nothing to run.
pub fn ready_alone(
    canvas: &Canvas,
    kinds: &Registry,
    requested: &BTreeSet<NodeId>,
    stale: &BTreeSet<NodeId>,
    accept: &dyn Fn(&Step) -> bool,
) -> Result<BTreeSet<NodeId>, Error> {
    ready_alone_expanded(&Expanded::plain(canvas), kinds, requested, stale, accept)
}

/// [`ready_alone`] on a canvas with its technique nodes inlined (#532): a technique node passes
/// when its inlined nodes, which it comes after, pass too. One that couldn't be inlined never
/// passes. `requested` and `stale` hold nodes of the real canvas.
pub fn ready_alone_expanded(
    expanded: &Expanded<'_>,
    kinds: &Registry,
    requested: &BTreeSet<NodeId>,
    stale: &BTreeSet<NodeId>,
    accept: &dyn Fn(&Step) -> bool,
) -> Result<BTreeSet<NodeId>, Error> {
    let canvas: &Canvas = &expanded.canvas;
    let graph = Graph::new(canvas, kinds, &nodecraft_types::EMPTY_TARGET_SELECTION, &expanded.collects, &expanded.inner, &nodecraft_types::NoOptions)?;
    let requested: BTreeSet<NodeId> = requested.iter().copied().filter(|id| canvas.nodes().contains_key(id) && !expanded.inner.contains_key(id)).collect();
    let run = graph.with_inlined(graph.with_upstream(&requested, stale), stale, &expanded.inner);
    let run: BTreeSet<NodeId> = run.into_iter().filter(|id| !crate::routing::is_router(canvas, *id) && !is_batch(canvas, *id)).collect();
    let not_inlined: BTreeSet<NodeId> = expanded.blockers.iter().map(|b| b.node).collect();
    let mut state = State::default();
    let mut passed = BTreeSet::new();
    // Dependency order: a step's `after` has been decided by the time it is reached.
    for id in graph.order(&run)? {
        if state.plan_bytes > MAX_PLAN_INPUT_BYTES {
            break;
        }
        if not_inlined.contains(&id) {
            continue;
        }
        let Ok(step) = graph.step(id, &run, &mut state) else { continue };
        if let Some(fanout) = &step.fanout {
            state.fanned.insert(id, fanout.items.clone());
        }
        if accept(&step) && step.after.iter().all(|before| passed.contains(before)) {
            passed.insert(id);
        }
    }
    Ok(passed.intersection(&requested).copied().collect())
}

/// A blocker as the real canvas shows it: one inside a technique is its technique node's.
fn owned(blocker: Blocker, expanded: &Expanded<'_>) -> Blocker {
    if expanded.inner.contains_key(&blocker.node) {
        // An off-list value keeps its own reason, so the technique node reports the same code.
        if blocker.reason == Reason::NotAnOption {
            return Blocker { node: expanded.owner(blocker.node), port: None, upstream: None, reason: Reason::NotAnOption };
        }
        let message = blocker.message(&expanded.canvas);
        let reason = match &blocker.reason {
            Reason::PublicInvalid(_) => Reason::PublicInvalid(format!("inside its technique: {}", blocker.reason.validation_message())),
            _ => Reason::Invalid(format!("inside its technique: {message}")),
        };
        return Blocker { node: expanded.owner(blocker.node), port: None, upstream: None, reason };
    }
    match blocker.upstream {
        Some(up) if expanded.inner.contains_key(&up) => Blocker { upstream: None, ..blocker },
        _ => blocker,
    }
}

/// Values already resolved for an upstream `(node, output port)`, with whether that output is a
/// list, the value's size (measured once, so the budget is checked before any copy) and, for a
/// result fanned out by batches, its items.
type Cache = BTreeMap<(NodeId, String), Result<(Value, bool, usize, Vec<ItemTag>), Reason>>;

/// A batch resolved into its items: values, their tags and their total size.
type Items = Result<(Vec<Value>, Vec<ItemTag>, usize), Reason>;

/// What planning carries from step to step.
#[derive(Default)]
struct State {
    cache: Cache,
    batches: BTreeMap<NodeId, Items>,
    /// Steps of this run that fan out, with their items.
    fanned: BTreeMap<NodeId, Vec<ItemTag>>,
    /// Known input bytes across the plan.
    plan_bytes: usize,
}

/// Whether `id` is a batch.
pub fn is_batch(canvas: &Canvas, id: NodeId) -> bool {
    canvas.nodes().get(&id).is_some_and(|node| node.kind == batch::ID)
}

/// A single or fanning-out input fed by a batch, or by a fanned result: one value per item.
struct Fanned {
    /// Index into the step's inputs.
    input: usize,
    /// The wire's place among the input's sources, which each item takes in turn.
    slot: usize,
    tags: Vec<ItemTag>,
    from: FannedFrom,
}

enum FannedFrom {
    Values(Vec<Value>),
    /// One value per item of a fanned step; a `list` output gives each item's list (#533).
    Step {
        node: NodeId,
        port: String,
        list: bool,
    },
}

/// What a technique node's output takes from its inlined node: one source, or one per item.
enum Collected {
    Plain(Vec<InputSource>),
    Fanned(Vec<ItemTag>, FannedFrom),
}

/// Edges indexed by target, once per plan.
struct Graph<'a> {
    canvas: &'a Canvas,
    kinds: &'a Registry,
    targets: &'a nodecraft_types::TargetSelection,
    /// What the host has fetched of each parameter's run-time list (#162).
    options: &'a dyn nodecraft_types::OptionLookup,
    /// Inputs of each node: (port, upstream node, upstream port, order).
    into: BTreeMap<NodeId, Vec<(&'a str, NodeId, &'a str, u32)>>,
    routing: crate::routing::Incoming<'a>,
    /// How each batch pairs with others (#92); one whose setting is invalid is blocked when
    /// its items are resolved.
    pairing: BTreeMap<NodeId, batch::Pairing>,
    /// Where each inlined technique node's outputs come from (#94).
    collects: &'a BTreeMap<NodeId, Vec<Collect>>,
    /// Each inlined node's technique node on the real canvas.
    inner: &'a BTreeMap<NodeId, NodeId>,
}

impl<'a> Graph<'a> {
    fn new(
        canvas: &'a Canvas,
        kinds: &'a Registry,
        targets: &'a nodecraft_types::TargetSelection,
        collects: &'a BTreeMap<NodeId, Vec<Collect>>,
        inner: &'a BTreeMap<NodeId, NodeId>,
        options: &'a dyn nodecraft_types::OptionLookup,
    ) -> Result<Self, Error> {
        // Routers are resolved away: consumers read the routers' own sources.
        let mut routing = crate::routing::incoming(canvas, kinds)?;
        let into = std::mem::take(&mut routing.feeds)
            .into_iter()
            .map(|(target, feeds)| (target, feeds.into_iter().map(|(port, position, from, from_port)| (port, from, from_port, position)).collect()))
            .collect();
        let pairing = canvas.nodes().values().filter(|n| n.kind == batch::ID).filter_map(|n| Some((n.id, batch::pairing(n).ok()?))).collect();
        Ok(Self { canvas, kinds, targets, options, into, routing, pairing, collects, inner })
    }

    /// What `id` is made from: its inputs' nodes and, for a technique node, the inlined nodes
    /// its outputs come from.
    fn upstream(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let collects = self.collects.get(&id).into_iter().flatten().map(|collect| collect.node);
        self.into.get(&id).into_iter().flatten().map(|(_, from, _, _)| *from).chain(collects)
    }

    /// Whether `node`, upstream of the run, runs too rather than giving its result as is.
    fn needs_run(&self, node: &Node, stale: &BTreeSet<NodeId>) -> bool {
        // A batch collects existing results only (#92), so the walk stops there.
        !self.is_source(node) && node.kind != batch::ID && (stale.contains(&node.id) || usable_outputs(node).is_none())
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
                if self.needs_run(node, stale) && run.insert(up) {
                    stack.push(up);
                }
            }
        }
        run
    }

    /// `run` plus the inlined nodes its technique nodes need, whatever the mode: a technique
    /// node's step is only its copied nodes' results given back. The walk stays inside the
    /// techniques; what feeds them is planned as for any node.
    fn with_inlined(&self, mut run: BTreeSet<NodeId>, stale: &BTreeSet<NodeId>, inner: &BTreeMap<NodeId, NodeId>) -> BTreeSet<NodeId> {
        let mut stack: Vec<NodeId> = run.iter().copied().filter(|id| self.collects.contains_key(id)).collect();
        while let Some(id) = stack.pop() {
            for up in self.upstream(id) {
                let Some(node) = self.canvas.nodes().get(&up).filter(|_| inner.contains_key(&up)) else { continue };
                if self.needs_run(node, stale) && run.insert(up) {
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

    /// Plan one node. `state.plan_bytes` is the running total of known input bytes across the
    /// plan.
    fn step(&self, id: NodeId, run: &BTreeSet<NodeId>, state: &mut State) -> Result<Step, Vec<Blocker>> {
        let blocker = |port: Option<&str>, upstream: Option<NodeId>, reason: Reason| Blocker { node: id, port: port.map(str::to_owned), upstream, reason };
        let node = self.canvas.nodes().get(&id).ok_or_else(|| vec![blocker(None, None, Reason::UnknownKind)])?;
        let kind = self.kinds.get(&node.kind).map_err(|_| vec![blocker(None, None, Reason::UnknownKind)])?;
        let mut blockers = Vec::new();
        // Required parameters and port drift are checked for every step now, so a downstream
        // problem never surfaces after an upstream paid step has run.
        if let Err(error) = kind.validate_instance(node, true) {
            blockers.push(blocker(None, None, invalid_kind(self.kinds, &node.kind, error.message)));
        }
        if self.routing.ambiguous.contains(&id) {
            let reason = Reason::PublicInvalid("its prompt mentions a name more than one Element has; rename one of them".into());
            blockers.push(blocker(Some(crate::mentions::PROMPT), None, reason));
        }
        if let Some(reason) = self.routing.mention_problems.get(&id) {
            blockers.push(blocker(Some(nodecraft_nodes::generation::PROMPT), None, Reason::PublicInvalid(reason.clone())));
        }
        // List inputs that run once per item rather than collecting them (#515).
        let fans_out: BTreeSet<String> = match kind.instance_contract(node) {
            Ok(contract) => {
                // A value off a list the host has fetched blocks the run here, before any job of
                // this node is admitted, quoted or charged (#162). An unfetched list refuses
                // nothing, so an offline host still runs the node and lets the provider judge.
                if options::off_list(&contract, node, self.options) {
                    blockers.push(blocker(None, None, Reason::NotAnOption));
                }
                contract.inputs.into_iter().filter(|input| input.fans_out).map(|input| input.port).collect()
            }
            Err(error) => {
                blockers.push(blocker(None, None, invalid_kind(self.kinds, &node.kind, error.message)));
                BTreeSet::new()
            }
        };
        let mut inputs = Vec::new();
        let mut fanned: Vec<Fanned> = Vec::new();
        let mut after = BTreeSet::new();
        let mut bytes = 0usize;
        // Of which, values of fanned inputs (each item copies only its own).
        let mut fan_bytes = 0usize;
        // One blocker per (port, upstream), however many noodles repeat it.
        let mut reported = BTreeSet::new();
        // Cards the prompt `@mentions` feed a port the node doesn't have (#505).
        let mentions = self
            .into
            .get(&id)
            .into_iter()
            .flatten()
            .any(|(p, ..)| *p == mention::PORT)
            .then(|| Port { name: mention::PORT.into(), port_type: PortType { modality: Modality::Text, many: true } });
        for port in node.inputs.iter().chain(&mentions) {
            if let Some(router) = self.routing.invalid.get(&(id, port.name.as_str())) {
                let reason = self.routing.invalid_routers.get(router).copied().unwrap_or("invalid upstream router");
                blockers.push(blocker(Some(&port.name), Some(*router), Reason::PublicInvalid(reason.into())));
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
                    blockers.push(blocker(Some(&port.name), None, Reason::PublicInvalid(format!("its {} input is fed by a router with no inputs", port.name))));
                }
                continue;
            }
            // A list can't reach a single input directly (the graph refuses it); through a
            // router it could.
            let list_into_single = |list: bool| list && !port.port_type.many;
            let list_through_router = || Reason::PublicInvalid(format!("its {} input takes one value but a list reaches it through a router", port.name));
            // A list input that collects every item of a batch into one run.
            let collects = port.port_type.many && !fans_out.contains(&port.name);
            let mut from = Vec::with_capacity(feeds.len().min(MAX_LIST_ITEMS));
            let mut origins = Vec::with_capacity(from.capacity());
            // A fanned wire holds its place in `from` with a stand-in that each item replaces.
            let mut fan = |from: &mut Vec<InputSource>, tags: Vec<ItemTag>, source: FannedFrom| {
                fanned.push(Fanned { input: inputs.len(), slot: from.len(), tags, from: source });
                from.push(InputSource::Value(Value::List(Vec::new())));
            };
            for (_, up, up_port, _) in feeds {
                let upstream = self.canvas.nodes().get(up);
                let before = from.len();
                if upstream.is_some_and(|u| u.kind == batch::ID) {
                    let items = state.batches.entry(*up).or_insert_with(|| self.batch_items(*up, run));
                    match items {
                        Ok((values, tags, size)) => {
                            if !self.charge(&mut bytes, &mut state.plan_bytes, *size) {
                                blockers.push(blocker(Some(&port.name), None, Reason::PublicInvalid(over_budget(bytes).into())));
                                return Err(blockers);
                            }
                            if collects {
                                from.extend(values.iter().cloned().map(InputSource::Value));
                            } else {
                                fan_bytes = fan_bytes.saturating_add(*size);
                                fan(&mut from, tags.clone(), FannedFrom::Values(values.clone()));
                            }
                        }
                        Err(reason) => {
                            if reported.insert((port.name.as_str(), *up)) {
                                blockers.push(blocker(Some(&port.name), Some(*up), reason.clone()));
                            }
                        }
                    }
                } else if run.contains(up) && !upstream.is_some_and(|u| self.is_source(u)) {
                    after.insert(*up);
                    let list = upstream.and_then(|u| u.outputs.iter().find(|p| p.name == *up_port)).is_some_and(|p| p.port_type.many);
                    if list_into_single(list) {
                        blockers.push(blocker(Some(&port.name), Some(*up), list_through_router()));
                        break;
                    }
                    match state.fanned.get(up) {
                        // A list input collects every item's list, flattened in item order (#519).
                        Some(tags) if list && collects => {
                            from.extend((0..tags.len()).map(|index| InputSource::Item { node: *up, port: (*up_port).to_owned(), index, list: true }))
                        }
                        Some(_) if list => {
                            blockers.push(blocker(Some(&port.name), Some(*up), Reason::PublicInvalid(fanned_list(&port.name))));
                            break;
                        }
                        // A list input collects every item of a fanned step.
                        Some(_) if collects => from.push(InputSource::Step { node: *up, port: (*up_port).to_owned(), list: true }),
                        Some(tags) => fan(&mut from, tags.clone(), FannedFrom::Step { node: *up, port: (*up_port).to_owned(), list: false }),
                        None => from.push(InputSource::Step { node: *up, port: (*up_port).to_owned(), list }),
                    }
                } else {
                    let resolved = state.cache.entry((*up, (*up_port).to_owned())).or_insert_with(|| {
                        self.resolve(*up, up_port).map(|(value, list, items)| {
                            let size = value_bytes(&value);
                            (value, list, size, items)
                        })
                    });
                    match resolved {
                        Ok((value, list, size, items)) => {
                            let (list, size) = (*list, *size);
                            if list_into_single(list) {
                                blockers.push(blocker(Some(&port.name), Some(*up), list_through_router()));
                                break;
                            }
                            if !self.charge(&mut bytes, &mut state.plan_bytes, size) {
                                blockers.push(blocker(Some(&port.name), None, Reason::PublicInvalid(over_budget(bytes).into())));
                                return Err(blockers);
                            }
                            // Within budget: only now copy. A list output into a list input
                            // contributes its items, not a nested list; so does a fanned result.
                            match value {
                                // A list input collects every item's list, flattened (#519).
                                Value::List(lists) if list && !items.is_empty() && collects => {
                                    from.extend(flatten(lists).cloned().map(InputSource::Value));
                                }
                                Value::List(_) if list && !items.is_empty() => {
                                    blockers.push(blocker(Some(&port.name), Some(*up), Reason::PublicInvalid(fanned_list(&port.name))));
                                    break;
                                }
                                Value::List(values) if !items.is_empty() && !collects => {
                                    fan_bytes = fan_bytes.saturating_add(size);
                                    fan(&mut from, items.clone(), FannedFrom::Values(values.clone()));
                                }
                                Value::List(values) if (list || !items.is_empty()) && port.port_type.many => {
                                    from.extend(values.iter().cloned().map(InputSource::Value));
                                }
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
                // A mention puts one text where its token is (#505): a card giving several can't.
                let several = || {
                    upstream.is_some_and(|u| u.kind == batch::ID)
                        || state.fanned.contains_key(up)
                        || from.len().checked_sub(before) != Some(1)
                        // A fanned feed's stand-in is an empty list.
                        || matches!(from.last(), Some(InputSource::Step { list: true, .. } | InputSource::Item { .. } | InputSource::Value(Value::List(_))))
                };
                if port.name == mention::PORT && several() {
                    let title = crate::node_mentions::title_of(node, *up);
                    let reason = crate::node_mentions::reason(title, crate::node_mentions::Problem::Several);
                    blockers.push(blocker(Some(nodecraft_nodes::generation::PROMPT), Some(*up), Reason::PublicInvalid(reason)));
                    break;
                }
                origins.resize(from.len(), *up);
                // A single input takes one value; through a router it could receive several.
                if !port.port_type.many && from.len() > 1 {
                    blockers.push(blocker(
                        Some(&port.name),
                        None,
                        Reason::PublicInvalid(format!("its {} input takes one value but receives several through a router", port.name)),
                    ));
                    break;
                }
                // The same item cap for values and for outputs of other steps in this run.
                if from.len() > MAX_LIST_ITEMS {
                    blockers.push(blocker(
                        Some(&port.name),
                        None,
                        Reason::PublicInvalid(format!("its {} input has more than {MAX_LIST_ITEMS} items", port.name)),
                    ));
                    break;
                }
            }
            inputs.push(PlannedInput { port: port.name.clone(), port_type: port.port_type, from, origins });
        }
        let connected: Vec<PortType> = inputs.iter().filter(|input| input.port != mention::PORT).map(|input| input.port_type).collect();
        let mode = match kind.instance_mode(node, &connected) {
            Ok(Some(mode)) => Some(mode),
            Ok(None) => {
                blockers.push(blocker(None, None, Reason::UnsupportedInputs));
                None
            }
            Err(error) => {
                blockers.push(blocker(None, None, invalid_kind(self.kinds, &node.kind, error.message)));
                None
            }
        };
        let (Some(mode), true) = (mode, blockers.is_empty()) else {
            return Err(blockers);
        };
        // A technique node takes each output from its inlined node; one fanned by a batch fans
        // the technique node too, one run of its version per item (#533).
        if let Some(collects) = self.collects.get(&id) {
            // Its own inputs only feed the inlined nodes: a batch on them fans the technique
            // node through what it collects, or not at all when the version collects the items.
            // Those inputs, holding only stand-ins for the items, are left out of its step.
            let fed: BTreeSet<usize> = fanned.drain(..).map(|fan| fan.input).collect();
            inputs = std::mem::take(&mut inputs).into_iter().enumerate().filter(|(index, _)| !fed.contains(index)).map(|(_, input)| input).collect();
            for collect in collects {
                let (collected, port_type) =
                    self.collect(collect, run, state, &mut bytes, &mut after).map_err(|(upstream, reason)| vec![blocker(None, Some(upstream), reason)])?;
                // A batch inside the version stays unsupported (#93); only one on the canvas
                // fans a technique node.
                if let Collected::Fanned(tags, _) = &collected
                    && tags.iter().flatten().any(|key| self.inner.contains_key(&key.batch))
                {
                    return Err(vec![blocker(None, Some(collect.node), Reason::PublicInvalid("a batch inside its technique can't fan it out yet".into()))]);
                }
                let from = match collected {
                    Collected::Plain(from) => from,
                    Collected::Fanned(tags, source) => {
                        if let FannedFrom::Values(values) = &source {
                            fan_bytes = fan_bytes.saturating_add(values.iter().map(value_bytes).fold(0usize, usize::saturating_add));
                        }
                        fanned.push(Fanned { input: inputs.len(), slot: 0, tags, from: source });
                        vec![InputSource::Value(Value::List(Vec::new()))]
                    }
                };
                let origins = vec![collect.node; from.len()];
                inputs.push(PlannedInput { port: format!("{}{}", technique::OUTPUT_PREFIX, collect.output), port_type, from, origins });
            }
        }
        let plan_jobs = |inputs: &[PlannedInput]| -> Result<Option<Vec<JobSpec>>, Vec<Blocker>> {
            let Some(values) = known_inputs(inputs) else { return Ok(None) };
            let jobs = kind
                .plan(&PlanCtx { targets: self.targets, node, inputs: &values })
                .map_err(|error| vec![blocker(None, None, invalid_kind(self.kinds, &node.kind, error.message))])?;
            crate::generation_contract::validate_targets(node, &jobs, self.targets)
                .map_err(|reason| vec![blocker(None, None, Reason::ProviderTarget(reason))])?;
            crate::generation_contract::validate_outputs(node, &jobs).map_err(|reason| vec![blocker(None, None, Reason::PublicInvalid(reason.into()))])?;
            Ok(Some(jobs))
        };
        let after = after.into_iter().collect();
        // Variations (#81) are for generation nodes on the canvas: a step inlined from a
        // technique makes its technique node's one result.
        let variations = match kind.descriptor().category {
            Category::Generation if !self.inner.contains_key(&id) => crate::variations::requested(node),
            _ => 1,
        };
        if fanned.is_empty() {
            // Each variation copies the step's inputs: charge the extra copies before making them.
            state.plan_bytes = state.plan_bytes.saturating_add(bytes.saturating_mul(variations.saturating_sub(1)));
            if state.plan_bytes > MAX_PLAN_INPUT_BYTES {
                return Err(vec![blocker(None, None, Reason::Invalid("the inputs of this run are too large to plan at once".into()))]);
            }
            let jobs = plan_jobs(&inputs)?.map(|jobs| crate::variations::repeat(jobs, variations));
            // A single result's own job count is the run's to refuse (`MAX_STEP_JOBS`), as without variations.
            if variations > 1 && jobs.as_ref().is_some_and(|jobs| jobs.len() > crate::run::MAX_STEP_JOBS) {
                let reason = format!("its variations would run more than {} jobs", crate::run::MAX_STEP_JOBS);
                return Err(vec![blocker(None, None, Reason::Invalid(reason))]);
            }
            return Ok(Step { node: id, kind: node.kind.clone(), mode, after, inputs, jobs, fanout: None, variations });
        }
        if variations > 1 {
            let reason = "it makes one result per batch item: set its variations to 1 to run it over a list";
            return Err(vec![blocker(None, None, Reason::Invalid(reason.into()))]);
        }
        let invalid = |message: String| vec![blocker(None, None, Reason::PublicInvalid(message))];
        let relations: Vec<&[ItemTag]> = fanned.iter().map(|f| f.tags.as_slice()).collect();
        let combined = crate::fanout::combine(&relations, &self.pairing).map_err(invalid)?;
        if combined.is_empty() {
            return Err(invalid("its batch has no items".into()));
        }
        // Every item copies the inputs they share and its own items' values. The first copy of
        // everything is already charged; charge the rest before copying.
        let shared = bytes.saturating_sub(fan_bytes);
        let mut copies = shared.saturating_mul(combined.len().saturating_sub(1));
        for cell in &combined {
            for (fan, row) in fanned.iter().zip(&cell.rows) {
                if let FannedFrom::Values(values) = &fan.from {
                    copies = copies.saturating_add(values.get(*row).map_or(0, value_bytes));
                }
            }
        }
        state.plan_bytes = state.plan_bytes.saturating_add(copies.saturating_sub(fan_bytes));
        if state.plan_bytes > MAX_PLAN_INPUT_BYTES {
            return Err(invalid("the inputs of this run are too large to plan at once".into()));
        }
        let mut items = Vec::with_capacity(combined.len());
        let mut cells = Vec::with_capacity(combined.len());
        for cell in combined {
            let mut cell_inputs = inputs.clone();
            for (fan, row) in fanned.iter().zip(&cell.rows) {
                let source = match &fan.from {
                    FannedFrom::Values(values) => InputSource::Value(values.get(*row).cloned().ok_or_else(|| invalid("an item is missing".into()))?),
                    FannedFrom::Step { node, port, list } => InputSource::Item { node: *node, port: port.clone(), index: *row, list: *list },
                };
                if let Some(slot) = cell_inputs.get_mut(fan.input).and_then(|input| input.from.get_mut(fan.slot)) {
                    *slot = source;
                }
            }
            items.push(cell.tag);
            cells.push(cell_inputs);
        }
        let mut jobs = Some(Vec::new());
        let mut counts = Vec::with_capacity(cells.len());
        for cell in &cells {
            match (plan_jobs(cell)?, &mut jobs) {
                (Some(planned), Some(all)) => {
                    counts.push(planned.len());
                    all.extend(planned);
                    if all.len() > crate::run::MAX_STEP_JOBS {
                        return Err(invalid(format!("it would run more than {} jobs", crate::run::MAX_STEP_JOBS)));
                    }
                }
                _ => {
                    jobs = None;
                    counts.clear();
                    break;
                }
            }
        }
        Ok(Step {
            node: id,
            kind: node.kind.clone(),
            mode,
            after,
            inputs: Vec::new(),
            jobs,
            fanout: Some(Fanout { items, cells, jobs: counts, resume: None }),
            variations: 1,
        })
    }

    /// Where one output of a technique node comes from: the inlined node's step in this run, or
    /// its result as is; per item when a batch fans that node out. `Err` names the node.
    fn collect(
        &self,
        collect: &Collect,
        run: &BTreeSet<NodeId>,
        state: &mut State,
        bytes: &mut usize,
        after: &mut BTreeSet<NodeId>,
    ) -> Result<(Collected, PortType), (NodeId, Reason)> {
        let inner = self.canvas.nodes().get(&collect.node).ok_or((collect.node, Reason::NoResult))?;
        let port_type = inner.outputs.iter().find(|p| p.name == collect.port).map(|p| p.port_type).ok_or((collect.node, Reason::NoResult))?;
        if run.contains(&collect.node) && !self.is_source(inner) {
            after.insert(collect.node);
            let (node, port, list) = (collect.node, collect.port.clone(), port_type.many);
            return Ok(match state.fanned.get(&collect.node) {
                Some(tags) => (Collected::Fanned(tags.clone(), FannedFrom::Step { node, port, list }), port_type),
                None => (Collected::Plain(vec![InputSource::Step { node, port, list }]), port_type),
            });
        }
        let (value, _, items) = self.resolve(collect.node, &collect.port).map_err(|reason| (collect.node, reason))?;
        if !self.charge(bytes, &mut state.plan_bytes, value_bytes(&value)) {
            return Err((collect.node, Reason::PublicInvalid(over_budget(*bytes).into())));
        }
        Ok(match value {
            // A result kept per item: each item's value (or list) in turn.
            Value::List(values) if !items.is_empty() => {
                if port_type.many {
                    return Err((collect.node, Reason::PublicInvalid("a technique's list output kept per batch item can't be reused; run it again".into())));
                }
                (Collected::Fanned(items, FannedFrom::Values(values)), port_type)
            }
            Value::List(values) if port_type.many => (Collected::Plain(values.into_iter().map(InputSource::Value).collect()), port_type),
            value => (Collected::Plain(vec![InputSource::Value(value)]), port_type),
        })
    }

    /// Adds `size` known input bytes to the step's and the plan's totals; `false` when either
    /// is over its budget.
    fn charge(&self, bytes: &mut usize, plan_bytes: &mut usize, size: usize) -> bool {
        *bytes = bytes.saturating_add(size);
        *plan_bytes = plan_bytes.saturating_add(size);
        *bytes <= MAX_STEP_INPUT_BYTES && *plan_bytes <= MAX_PLAN_INPUT_BYTES
    }

    /// A batch's items (#92): what is wired into it, which must already exist, then its own.
    fn batch_items(&self, id: NodeId, run: &BTreeSet<NodeId>) -> Items {
        let node = self.canvas.nodes().get(&id).ok_or(Reason::NoResult)?;
        let kind = self.kinds.get(&node.kind).map_err(|_| Reason::UnknownKind)?;
        kind.validate_instance(node, true).map_err(|error| invalid_kind(self.kinds, &node.kind, error.message))?;
        if let Some(router) = self.routing.invalid.keys().find(|(target, _)| *target == id).and_then(|key| self.routing.invalid.get(key)) {
            let reason = self.routing.invalid_routers.get(router).copied().unwrap_or("invalid upstream router");
            return Err(Reason::PublicInvalid(reason.into()));
        }
        let mut wired = Vec::new();
        for (_, up, up_port, _) in self.into.get(&id).into_iter().flatten() {
            let upstream = self.canvas.nodes().get(up);
            if upstream.is_some_and(|u| u.kind == batch::ID) {
                return Err(Reason::PublicInvalid("a batch can't collect another batch yet".into()));
            }
            if run.contains(up) && !upstream.is_some_and(|u| self.is_source(u)) {
                return Err(Reason::PublicInvalid("a batch collects existing results; run the nodes feeding it first".into()));
            }
            let (value, list, items) = self.resolve(*up, up_port)?;
            match value {
                Value::List(lists) if list && !items.is_empty() => wired.extend(flatten(&lists).cloned()),
                Value::List(values) if list || !items.is_empty() => wired.extend(values),
                value => wired.push(value),
            }
            if wired.len() > batch::MAX_ITEMS {
                return Err(Reason::PublicInvalid(format!("a batch holds at most {} items", batch::MAX_ITEMS)));
            }
        }
        let values = batch::items(node, wired).map_err(|error| invalid_kind(self.kinds, &node.kind, error.message))?;
        let tags = crate::fanout::batch_tags(id, &values);
        let size = values.iter().map(value_bytes).fold(0usize, usize::saturating_add);
        Ok((values, tags, size))
    }

    /// The value an upstream node provides on `port` without running: a source's content, or
    /// the active result of anything else (also when its kind isn't installed), with the items
    /// of a result fanned out by batches.
    fn resolve(&self, id: NodeId, port: &str) -> Result<(Value, bool, Vec<ItemTag>), Reason> {
        let node = self.canvas.nodes().get(&id).ok_or(Reason::NoResult)?;
        let index = node.outputs.iter().position(|p| p.name == port).ok_or(Reason::NoResult)?;
        let list = node.outputs.get(index).is_some_and(|p| p.port_type.many);
        if let Ok(kind) = self.kinds.get(&node.kind)
            && kind.descriptor().category == Category::Source
        {
            let jobs = kind.plan(&PlanCtx { targets: self.targets, node, inputs: &[] }).map_err(|error| invalid_kind(self.kinds, &node.kind, error.message))?;
            return jobs
                .into_iter()
                .find_map(|job| match job {
                    JobSpec::Source { output, value, .. } if output == port => Some((value, list, Vec::new())),
                    JobSpec::Source { .. } | JobSpec::Generate { .. } | JobSpec::Action { .. } => None,
                })
                .ok_or(Reason::NoResult);
        }
        let generation = usable_generation(node).ok_or(Reason::NoResult)?;
        let value = generation.outputs.get(index).cloned().ok_or(Reason::NoResult)?;
        if !generation.items.is_empty()
            && (!crate::fanout::consistent(&generation.items) || !matches!(&value, Value::List(values) if values.len() == generation.items.len()))
        {
            return Err(Reason::PublicInvalid("its result's items don't match its outputs".into()));
        }
        Ok((value, list, generation.items.clone()))
    }
}

fn over_budget(bytes: usize) -> &'static str {
    if bytes > MAX_STEP_INPUT_BYTES { "its inputs are too large to run in one step" } else { "the inputs of this run are too large to plan at once" }
}

/// The values of a fanned list result (#519), one list per item, in item order.
fn flatten(lists: &[Value]) -> impl Iterator<Item = &Value> {
    lists.iter().flat_map(|list| match list {
        Value::List(values) => values.as_slice(),
        other => std::slice::from_ref(other),
    })
}

fn fanned_list(port: &str) -> String {
    format!("its {port} input would receive a list for every batch item, which isn't supported yet")
}

/// The active generation when it can be used: results are positional, so one recorded for a
/// different port layout is not. The upstream walk and resolution share this, so a node whose
/// result can't be used is re-run rather than waited on.
/// A partial result (#480) is never used: its node runs again first.
fn usable_generation(node: &Node) -> Option<&nodecraft_graph::Generation> {
    let active = node.active?;
    node.history.iter().find(|g| g.id == active).filter(|g| g.outputs.len() == node.outputs.len() && !g.is_partial())
}

#[cfg(test)]
fn active_outputs(node: &Node) -> Option<&[Value]> {
    let active = node.active?;
    node.history.iter().find(|g| g.id == active).map(|g| g.outputs.as_slice())
}

fn usable_outputs(node: &Node) -> Option<&[Value]> {
    usable_generation(node).map(|g| g.outputs.as_slice())
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
                InputSource::Step { .. } | InputSource::Item { .. } => None,
            });
            let value = if input.port_type.many { Value::List(values.collect::<Option<Vec<_>>>()?) } else { values.next()?? };
            let origins = if input.port_type.many { input.origins.clone() } else { input.origins.iter().take(1).copied().collect() };
            Some(InputValue { port: input.port.clone(), port_type: input.port_type, value, origins })
        })
        .collect()
}

#[cfg(test)]
mod contract_tests;
#[cfg(test)]
mod options_tests;
#[cfg(test)]
mod tests;
