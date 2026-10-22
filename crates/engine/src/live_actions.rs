//! Live recompute for action nodes (#88): when an edit leaves action nodes stale, the host plans
//! them (with whatever upstream they need), runs the plan off the UI thread and records the
//! results into the edit's own undo step ([`Session::record_derived`]).
//!
//! Only local work is ever run live: a plan with any non-local job (a model call, which costs
//! money) is left for an explicit run. Technique nodes are planned with their versions inlined
//! (#532), so one made only of local work refreshes like an action node; only it records
//! results, and one with a model call inside is never refreshed, not even from the cache. The
//! three steps are separate so the host can move the middle one to a worker: [`pending`] (UI
//! thread, cheap), [`compute`] (anywhere: it borrows nothing from the session) and [`apply`]
//! (UI thread).
//!
//! While a slider drags, [`preview`] plans the same work on a copy of the canvas holding the
//! dragged value, on scaled-down images: the host shows its results and records nothing, and
//! the full-size round follows when the slider is released and the value is recorded.
use crate::Session;
use crate::action_exec::LocalActions;
use nodecraft_exec::planner::{Planned, RunMode, RunPlan, Step, plan_expanded, ready_alone_expanded};
use nodecraft_exec::run::{Run, RunOptions};
use nodecraft_exec::scheduler::RetryPolicy;
use nodecraft_format::AssetStore;
use nodecraft_graph::{Canvas, Change, Changeset, Generation, Project};
use nodecraft_nodes::{Category, JobSpec, Registry, technique};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Live work for one revision of one canvas.
pub struct LiveJob {
    pub canvas: CanvasId,
    /// The revision it was planned from; results for another revision are dropped.
    pub base: u64,
    /// Random per round, like any run's: generation ids derive from it, so a reopened project
    /// (whose revisions count from zero again) never makes ids its history already holds.
    id: [u8; 16],
    plan: Option<RunPlan>,
    /// Cached results to apply instead of computing (#91): a round either reuses or computes.
    hits: Vec<crate::cache::Hit>,
    /// The canvas the plan was made on, with its technique nodes inlined.
    live: Canvas,
    assets: AssetStore,
    /// For a drag preview: the edge input images are scaled down to. Never recorded.
    preview: Option<u32>,
}

impl LiveJob {
    /// Whether this is a drag preview, shown but never recorded.
    pub fn is_preview(&self) -> bool {
        self.preview.is_some()
    }
}

/// What a live run produced.
pub struct LiveOutcome {
    pub canvas: CanvasId,
    pub base: u64,
    pub results: Vec<(NodeId, Generation)>,
    /// Failed nodes, why, and the input hash they failed with (to not retry until it changes).
    pub failed: Vec<(NodeId, String, Option<nodecraft_types::Blake3>)>,
    hits: Vec<crate::cache::Hit>,
    staged: AssetStore,
    preview: bool,
}

/// A drag preview's results (#88): view state, shown in place of the nodes' results until the
/// document changes. `staged` holds their images.
pub struct LivePreview {
    pub canvas: CanvasId,
    pub base: u64,
    pub results: BTreeMap<NodeId, Generation>,
    pub staged: AssetStore,
}

impl LiveOutcome {
    /// Whether this is a drag preview's outcome: [`LiveOutcome::into_preview`], not [`apply`].
    pub fn is_preview(&self) -> bool {
        self.preview
    }

    /// The preview this outcome is, if it is one.
    pub fn into_preview(self) -> Option<LivePreview> {
        self.preview.then(|| LivePreview { canvas: self.canvas, base: self.base, results: self.results.into_iter().collect(), staged: self.staged })
    }
}

/// The live work `session`'s current document needs on `canvas`: its stale action nodes, if
/// they can run now with local work only. `failed` names nodes that failed and the input hash
/// they failed with: they wait until their inputs or settings change. Nothing is planned while
/// there is something to redo (the results would be refused) or the project is read-only.
pub fn pending(session: &Session, canvas: CanvasId, failed: &BTreeMap<NodeId, nodecraft_types::Blake3>) -> Result<Option<LiveJob>, Error> {
    if session.can_redo() || session.file_status().read_only {
        return Ok(None);
    }
    plan(session, session.document(), canvas, failed, None)
}

/// A slider's value while it drags (#88): shown in a preview, not yet in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub node: NodeId,
    pub name: String,
    pub value: Value,
}

/// Longest edge, in pixels, input images are scaled down to for a drag preview.
pub const PREVIEW_EDGE: u32 = 512;

/// The preview `draft` needs on `canvas` (#88): the action and technique nodes it changes (the
/// drafted node and what it feeds), run on a copy of the canvas holding the draft, on input
/// images scaled down to [`PREVIEW_EDGE`]. Its outcome is only shown ([`LiveOutcome::preview`]),
/// never recorded: releasing the slider records the value, and the full render follows. No
/// cached results are used, and nothing is planned for a read-only project or a model node.
pub fn preview(session: &Session, canvas: CanvasId, draft: &Draft) -> Result<Option<LiveJob>, Error> {
    if session.file_status().read_only {
        return Ok(None);
    }
    let kinds = session.kinds();
    let live = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let node = live.nodes().get(&draft.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    let kind = kinds.get(&node.kind)?;
    if kind.descriptor().category == Category::Generation {
        return Ok(None);
    }
    // The draft is checked as the edit it becomes would be.
    let descriptor = kind.resolve_instance(node)?;
    let spec =
        descriptor.params.params.iter().find(|spec| spec.name == draft.name).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "unknown node parameter"))?;
    spec.control.validate_value(&draft.value)?;
    let mut proposed = node.params.clone();
    proposed.insert(draft.name.clone(), draft.value.clone());
    descriptor.params.resolve(&proposed, false)?;
    // What the draft changes: its node and everything downstream of it (edges can form cycles).
    let mut outgoing: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in live.edges().values() {
        outgoing.entry(edge.from.node).or_default().push(edge.to.node);
    }
    let mut affected = BTreeSet::from([draft.node]);
    let mut frontier = vec![draft.node];
    while let Some(from) = frontier.pop() {
        for to in outgoing.get(&from).into_iter().flatten() {
            if affected.insert(*to) {
                frontier.push(*to);
            }
        }
    }
    // Only this canvas and the techniques it may inline are copied, not the whole project; a
    // project whose parts don't stand alone is copied whole.
    let change = Changeset::new(vec![Change::SetParam { canvas, id: draft.node, name: draft.name.clone(), value: Some(draft.value.clone()) }]);
    let part = session.document().subset(|c| c.id() == canvas || c.kind() == nodecraft_graph::CanvasKind::Technique);
    let (drafted, _) = match change.stage(&part) {
        Ok(staged) => staged,
        Err(_) => change.stage(session.document())?,
    };
    let Some(mut job) = plan(session, &drafted, canvas, &BTreeMap::new(), Some(&affected))? else {
        return Ok(None);
    };
    job.preview = Some(PREVIEW_EDGE);
    Ok(Some(job))
}

/// The live work on `canvas` of `document` (the session's, or a copy holding a draft): its stale
/// action and technique nodes, or only those in `only`, which also skips the cache.
fn plan(
    session: &Session,
    document: &Project,
    canvas: CanvasId,
    failed: &BTreeMap<NodeId, nodecraft_types::Blake3>,
    only: Option<&BTreeSet<NodeId>>,
) -> Result<Option<LiveJob>, Error> {
    let kinds = session.kinds();
    let live = document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    // Cheap first: most canvases have no action node, and checking staleness walks them all.
    if !live.nodes().iter().any(|(id, n)| refreshes(&n.kind, kinds) && only.is_none_or(|only| only.contains(id))) {
        return Ok(None);
    }
    let skip: BTreeSet<NodeId> =
        failed.iter().filter(|(id, hash)| nodecraft_exec::staleness::input_hash(live, kinds, **id).is_ok_and(|now| now == **hash)).map(|(id, _)| *id).collect();
    let stale = nodecraft_exec::staleness::stale(live, kinds)?;
    // A technique node runs its version's nodes, inlined. One whose version can't be inlined, or
    // holds a model call, never refreshes live: not even from the cache, where its result would
    // be a model's.
    // Only a canvas holding a technique node pays for the inlined copy before the early return.
    let expanded = live
        .nodes()
        .values()
        .any(|node| node.kind == technique::ID)
        .then(|| nodecraft_exec::technique::expand(document.canvases(), live, kinds).ok())
        .flatten();
    let local_technique = |id: &NodeId| {
        expanded.as_ref().is_some_and(|expanded| {
            !expanded.blockers.iter().any(|b| b.node == *id)
                && expanded.inner.iter().filter(|(_, owner)| *owner == id).all(|(inner, _)| {
                    expanded.canvas.nodes().get(inner).is_some_and(|n| !renders_video(&n.kind))
                        && expanded
                            .canvas
                            .nodes()
                            .get(inner)
                            .and_then(|n| kinds.get(&n.kind).ok())
                            .is_some_and(|k| k.descriptor().category != Category::Generation)
                })
        })
    };
    // Stale action and technique nodes, and ones with no result yet. A Timeline or a video action
    // renders a video, which takes too long to redo on every edit: it runs when asked.
    let wanted: BTreeSet<NodeId> = live
        .nodes()
        .iter()
        .filter(|(_, node)| !renders_video(&node.kind))
        .filter(|(id, node)| refreshes(&node.kind, kinds) && !skip.contains(id) && (node.active.is_none() || stale.contains(id)))
        .filter(|(id, _)| only.is_none_or(|only| only.contains(id)))
        .filter(|(id, node)| node.kind != technique::ID || local_technique(id))
        .map(|(id, _)| *id)
        .collect();
    if wanted.is_empty() {
        return Ok(None);
    }
    // Results already made for these exact inputs are reused first (free); what they feed is
    // planned in the next round, from the document with them in place.
    // A preview computes: a cached result is full size, and only a recorded round may apply it.
    let hits = if only.is_none() { crate::cache::hits(document, canvas, kinds, wanted.iter().copied(), crate::cache::fresh_id)? } else { Vec::new() };
    if !hits.is_empty() {
        return Ok(Some(LiveJob {
            canvas,
            base: session.revision(),
            id: [0; 16],
            plan: None,
            hits,
            live: live.clone(),
            assets: AssetStore::default(),
            preview: None,
        }));
    }
    // A canvas that can't be inlined has no technique node wanted, and plans as it is.
    let expanded = expanded.unwrap_or_else(|| nodecraft_exec::technique::Expanded::plain(live));
    let planned: &Canvas = &expanded.canvas;
    // Each wanted node on its own terms: one that can't run yet (nothing connected, or a model
    // upstream with no result) mustn't hold the others back. One walk decides for all (#549).
    let runnable = ready_alone_expanded(&expanded, kinds, &wanted, &stale, &|step| local_step(step, planned, kinds)).unwrap_or_default();
    if runnable.is_empty() {
        return Ok(None);
    }
    // One round records at most one step's worth; the rest follow in the next round.
    let runnable: BTreeSet<NodeId> = runnable.into_iter().take(crate::run_cmds::MAX_RESULTS).collect();
    let targets = &nodecraft_types::EMPTY_TARGET_SELECTION;
    let Planned::Ready(plan) = plan_expanded(&expanded, kinds, &RunMode::WithUpstream, &runnable, &stale, targets, &nodecraft_types::NoOptions)? else {
        return Ok(None);
    };
    if !local_only(&plan, planned, kinds) {
        return Ok(None);
    }
    let mut id = [0u8; 16];
    id.copy_from_slice(crate::execution_identity::nonce()?.get(..16).unwrap_or(&[0; 16]));
    let live = expanded.canvas.into_owned();
    Ok(Some(LiveJob { canvas, base: session.revision(), id, plan: Some(plan), hits: Vec::new(), live, assets: session.assets().clone(), preview: None }))
}

/// A canvas [`pending`] found nothing to do on. Until it changes in more than layout (moving a
/// node, grouping), asking again gives the same answer, and at a thousand nodes asking costs a
/// frame (#549). It only holds while there is nothing to redo and the project is writable, as
/// when it was taken; the host drops it when a failure is recorded, since `failed` changes.
pub struct Settled {
    canvas: CanvasId,
    live: Canvas,
}

impl Settled {
    /// `canvas` as `session` holds it now, after [`pending`] found nothing to do on it.
    pub fn now(session: &Session, canvas: CanvasId) -> Option<Settled> {
        if session.can_redo() || session.file_status().read_only {
            return None;
        }
        Some(Settled { canvas, live: session.document().canvases().get(&canvas)?.clone() })
    }

    /// Whether [`pending`] would still find nothing to do on `canvas`.
    pub fn holds(&self, session: &Session, canvas: CanvasId) -> bool {
        self.canvas == canvas
            && !session.can_redo()
            && !session.file_status().read_only
            && session.document().canvases().get(&canvas).is_some_and(|live| live.same_but_layout(&self.live))
    }
}

/// Whether nodes of `kind` work on the video service (rendering or decoding a video), which never runs live.
fn renders_video(kind: &str) -> bool {
    kind == nodecraft_nodes::timeline::ID
        || nodecraft_nodes::video_edit::IDS.contains(&kind)
        || nodecraft_nodes::video_frames::IDS.contains(&kind)
        || nodecraft_nodes::video_look::IDS.contains(&kind)
        || nodecraft_nodes::video_split::IDS.contains(&kind)
}

/// Whether nodes of `kind` refresh live: actions, and technique nodes made only of local work.
fn refreshes(kind: &str, kinds: &Registry) -> bool {
    kind == technique::ID || kinds.get(kind).is_ok_and(|k| k.descriptor().category == Category::Action)
}

/// Whether every step is a source, an action or structure: nothing that calls a model.
fn local_only(plan: &RunPlan, live: &Canvas, kinds: &Registry) -> bool {
    plan.steps.iter().all(|step| local_step(step, live, kinds))
}

/// Whether `step` is a source, an action or structure.
fn local_step(step: &Step, live: &Canvas, kinds: &Registry) -> bool {
    let jobs_local = step.jobs.as_ref().is_none_or(|jobs| jobs.iter().all(|j| matches!(j, JobSpec::Source { .. } | JobSpec::Action { .. })));
    let kind_local = live.nodes().get(&step.node).and_then(|n| kinds.get(&n.kind).ok()).is_some_and(|k| k.descriptor().category != Category::Generation);
    jobs_local && kind_local
}

/// Runs `job` (any thread): sources emit their values, actions run on [`LocalActions`].
pub fn compute(job: LiveJob, kinds: &Registry) -> Result<LiveOutcome, Error> {
    let Some(plan) = &job.plan else {
        return Ok(LiveOutcome {
            canvas: job.canvas,
            base: job.base,
            results: Vec::new(),
            failed: Vec::new(),
            hits: job.hits,
            staged: AssetStore::default(),
            preview: job.preview.is_some(),
        });
    };
    // The worker's kinds must be the planner's: a kind it doesn't know can't run here.
    if let Some(step) = plan.steps.iter().find(|step| kinds.get(&step.kind).is_err()) {
        return Err(Error::new(ErrorKind::Unsupported, format!("live recompute doesn't know the {} node kind", step.kind)));
    }
    let options = RunOptions { id: job.id, first_job: 1, limits: Default::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(plan, job.canvas, &job.live, kinds, options)?;
    let actions = match job.preview {
        Some(edge) => LocalActions::new(job.assets).with_preview_edge(edge),
        None => LocalActions::new(job.assets),
    };
    let mut queue: VecDeque<_> = first.jobs.into_iter().collect();
    let mut finished: BTreeMap<NodeId, Generation> = first.finished.into_iter().map(|r| (r.node, r.generation)).collect();
    let mut failed: Vec<(NodeId, String)> = first.failed.into_iter().map(|f| (f.node, f.reason)).collect();
    let hash = |id: &NodeId| nodecraft_exec::staleness::input_hash(&job.live, kinds, *id).ok();
    while let Some(scheduled) = queue.pop_front() {
        let progress = match run.work(scheduled.id).cloned() {
            Some(JobSpec::Source { output, value, .. }) => run.job_done(kinds, scheduled.id, vec![(output, value)]),
            Some(JobSpec::Action { action, params, inputs, output, .. }) => {
                match nodecraft_exec::run::ActionExecutor::execute(&actions, &action, &params, &inputs) {
                    Ok(value) => run.job_done(kinds, scheduled.id, vec![(output, value)]),
                    Err(reason) => run.job_failed(kinds, scheduled.id, &reason),
                }
            }
            _ => run.job_failed(kinds, scheduled.id, "live recompute runs only local work"),
        };
        queue.extend(progress.jobs);
        finished.extend(progress.finished.into_iter().map(|r| (r.node, r.generation)));
        failed.extend(progress.failed.into_iter().map(|f| (f.node, f.reason)));
    }
    // Sources re-emit their values: record only the action and technique nodes' results (the run
    // keeps inlined nodes' results to itself).
    let refreshed = |id: &NodeId| job.live.nodes().get(id).is_some_and(|n| refreshes(&n.kind, kinds));
    let results = finished.into_iter().filter(|(id, _)| refreshed(id)).collect();
    let failed = failed.into_iter().map(|(id, reason)| (id, reason, hash(&id))).collect();
    Ok(LiveOutcome { canvas: job.canvas, base: job.base, results, failed, hits: Vec::new(), staged: actions.take_staged(), preview: job.preview.is_some() })
}

/// Records `outcome` if the document is still the one it was computed from. `Ok(false)` when
/// it is out of date (another recompute follows) or produced nothing.
pub fn apply(session: &mut Session, outcome: LiveOutcome) -> Result<bool, Error> {
    // A preview was made from a draft, at a reduced size: it is never recorded.
    if outcome.preview || (outcome.results.is_empty() && outcome.hits.is_empty()) || session.revision() != outcome.base {
        return Ok(false);
    }
    match session.record_derived_with(outcome.canvas, outcome.results, &outcome.hits, &outcome.staged, outcome.base) {
        Ok(()) => Ok(true),
        Err(error) if error.kind == ErrorKind::Conflict => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests;
