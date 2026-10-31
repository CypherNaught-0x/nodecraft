//! `run.estimate` (#663): asks providers what a run's by-usage jobs cost, through the host's
//! [`ESTIMATE_SERVICE`]; see [`crate::provider_quotes`].
use super::{RunCatalog, RunParams, check_requested, invalid, mode_of, run_canvas};
use crate::commands::{CommandSpec, Context, Effect, Phase, params};
use crate::provider_quotes::{ESTIMATE_SERVICE, MAX_ESTIMATED, QuoteKey};
use nodecraft_exec::planner::{Planned, RunMode, plan_expanded};
use nodecraft_nodes::JobSpec;
use nodecraft_providers::{Provider, ProviderJob};
use nodecraft_types::{Error, ErrorKind, NodeId};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// One job to ask a quote for: exactly what the run would send, with no idempotency key.
pub struct EstimateJob {
    pub key: QuoteKey,
    pub node: NodeId,
    pub provider: Arc<dyn Provider>,
    pub job: ProviderJob,
}

/// What a host asks providers for one `run.estimate`.
pub struct Estimating {
    pub canvas: nodecraft_types::CanvasId,
    /// The provider routing the jobs were built under; quotes are kept for it only.
    pub routing: Option<[u8; 32]>,
    pub jobs: Vec<EstimateJob>,
    /// By-usage jobs not asked about: their inputs aren't made yet, or the request reached
    /// [`MAX_ESTIMATED`].
    pub skipped: usize,
}

/// The by-usage jobs of the run `value` names, as the run would send them: planned and admitted
/// as `run.start` does, and handed out as a run would (narrowed to their mode, seeds drawn). Only
/// jobs whose inputs are known now can be asked about. Reads the session; sends nothing.
pub(crate) fn jobs(c: &Context<'_>, value: &Value) -> Result<Estimating, Error> {
    check_requested(value)?;
    let p: RunParams = params(value)?;
    let catalog = c.resources.require::<RunCatalog>()?;
    let canvas = run_canvas(c.document, p.canvas)?;
    let expanded = nodecraft_exec::technique::expand(c.document.canvases(), canvas, c.kinds)?;
    nodecraft_exec::admission::measure::canvas(&expanded.canvas)?;
    let mode = mode_of(&p)?;
    let requested = p.nodes.iter().copied().collect();
    let stale = if matches!(mode, RunMode::WithUpstream | RunMode::AllStale) { nodecraft_exec::staleness::stale(canvas, c.kinds)? } else { BTreeSet::new() };
    let targets = c.routing.routing.targets();
    let plan = match plan_expanded(&expanded, c.kinds, &mode, &requested, &stale, targets, crate::option_cmds::lookup(c.resources))? {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => return Err(invalid(super::blocked_reasons(&blockers).join("; "))),
    };
    crate::generation_catalog::validate_plan(c.resources, c.kinds, &expanded.canvas, plan.steps.iter().map(|s| s.node))?;
    let quote = nodecraft_exec::admission::quote(&plan, &expanded.canvas, c.kinds, catalog.catalog())
        .map_err(|error| Error::new(error.kind, super::admission_reason(&error)))?;
    // The planned identity of each by-usage step's jobs, in order.
    let mut wanted: BTreeMap<NodeId, Vec<QuoteKey>> = BTreeMap::new();
    let mut skipped = 0usize;
    for step in quote.steps.iter().filter(|step| crate::provider_quotes::wants(step)) {
        let planned = plan.steps.iter().find(|s| s.node == step.node).and_then(|s| s.jobs.as_ref());
        let keys = planned.into_iter().flatten().map(crate::provider_quotes::key).collect::<Result<Option<Vec<_>>, _>>()?;
        match keys {
            Some(keys) if !keys.is_empty() => {
                wanted.insert(step.node, keys);
            }
            _ => skipped = skipped.saturating_add(usize::try_from(step.jobs).unwrap_or(usize::MAX)),
        }
    }
    let mut out = Estimating { canvas: p.canvas, routing: c.routing.nonce, jobs: Vec::new(), skipped };
    if wanted.is_empty() {
        return Ok(out);
    }
    let (admitted, _) = nodecraft_exec::admission::admit_run_with_targets(
        plan,
        p.canvas,
        nodecraft_exec::run::snapshot(expanded.canvas),
        c.kinds,
        catalog.catalog(),
        nodecraft_exec::admission::AdmissionLimits::default(),
        targets,
    )
    .map_err(|error| Error::new(error.kind, super::admission_reason(&error)))?;
    // A run that is never started: it hands out the jobs whose inputs are known, as a real one
    // would, but under its own id, so its drawn seeds are not the ones the real run sends.
    let mut id = [0u8; 16];
    getrandom::fill(&mut id).map_err(|_| Error::new(ErrorKind::Internal, "run identity entropy is unavailable"))?;
    let options = nodecraft_exec::run::RunOptions { id, first_job: 1, limits: Default::default(), retry: Default::default() };
    let (run, progress) = nodecraft_exec::run::Run::start_admitted(admitted, c.kinds, options)?;
    let mut handed: BTreeMap<NodeId, Vec<&JobSpec>> = BTreeMap::new();
    for job in &progress.jobs {
        if let Some(work @ JobSpec::Generate { node, .. }) = run.work(job.id)
            && wanted.contains_key(node)
        {
            handed.entry(*node).or_default().push(work);
        }
    }
    let mut asked = BTreeSet::new();
    for (node, keys) in &wanted {
        let works = handed.get(node).map(Vec::as_slice).unwrap_or_default();
        // A step that hands out other jobs than were planned (a batch resuming only the items its
        // partial result lacks) can't be matched to its quote.
        if works.len() != keys.len() {
            skipped = skipped.saturating_add(keys.len());
            continue;
        }
        for (key, work) in keys.iter().zip(works) {
            // A repeated job shares its quote.
            if asked.contains(key) {
                continue;
            }
            if asked.len() >= MAX_ESTIMATED {
                skipped = skipped.saturating_add(1);
                continue;
            }
            let JobSpec::Generate { provider, instance, .. } = work else { continue };
            let binding = c.routing.routing.find_parts(provider, *instance)?;
            let (_, job) = crate::generate::provider_job_with_media(work, "", c.assets)?;
            asked.insert(*key);
            out.jobs.push(EstimateJob { key: *key, node: *node, provider: Arc::clone(binding.provider()), job });
        }
    }
    out.skipped = skipped;
    Ok(out)
}

fn run_estimate(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    check_requested(value)?;
    let p: RunParams = params(value)?;
    run_canvas(c.document, p.canvas)?;
    mode_of(&p)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Value::Null });
    }
    Ok(Effect::Service { capability: ESTIMATE_SERVICE, request: value.clone() })
}

fn enabled_estimate(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    check_requested(value)?;
    let p: RunParams = params(value)?;
    run_canvas(c.document, p.canvas)?;
    c.resources.require::<RunCatalog>()?;
    if !c.services.contains(ESTIMATE_SERVICE) {
        return Err(Error::new(ErrorKind::Unsupported, "this host can't ask providers for prices"));
    }
    mode_of(&p).map(|_| ())
}

pub(super) fn spec() -> CommandSpec {
    CommandSpec {
        id: "run.estimate",
        label: "Ask providers for prices",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: super::quote_schema,
        params_doc: "{canvas, mode, nodes?, field?} (as run.quote) -> {accepted, request}; asks the providers of the run's by-usage steps what each job would cost (free; no idempotency key is used). The answers arrive later, as the host's notice for `request`; run.quote then prices a by-usage step whose every job was quoted, and lists it in `quoted`.",
        enabled: enabled_estimate,
        run: run_estimate,
    }
}
