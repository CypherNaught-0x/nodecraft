//! Catalog-bound immutable execution admission. Logical limits cover preparation owners,
//! not arbitrary host queues, native callback peaks, later outputs or all Run state.
use crate::{
    cost::{Quote, StepQuote, Unpriced},
    planner::RunPlan,
    run::RunLimits,
};
use nodecraft_catalog::{Catalog, ModelDescriptor, Pricing};
use nodecraft_graph::{Canvas, Node};
use nodecraft_nodes::{Category, JobSpec, Registry};
use nodecraft_types::{CanvasId, Error, ErrorKind, Money, NodeId, ParamControl, ParamSchema, ParamValues, Value};
use std::{collections::BTreeMap, mem::size_of};
mod characters;
pub mod measure;
#[cfg(test)]
thread_local! {static CANONICAL_COPIES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
thread_local! {static TARGET_COPIES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
thread_local! {static QUOTE_COPIES:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
pub(crate) mod tests;
pub const MAX_WITNESS_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SCRATCH_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;
pub(crate) fn cap() -> Error {
    Error::new(ErrorKind::Budget, "catalog admission ownership exceeds its limit")
}
/// Static failure category. One deterministic first blocker is returned, never a raw job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockerReason {
    CatalogMismatch,
    OwnershipLimit,
}
fn blocked(node: NodeId, reason: BlockerReason) -> Error {
    let (kind, text) = match reason {
        BlockerReason::CatalogMismatch => (ErrorKind::InvalidInput, "catalog model or canonical parameters do not match"),
        BlockerReason::OwnershipLimit => (ErrorKind::Budget, "catalog admission ownership exceeds its limit"),
    };
    Error::new(kind, format!("node {node}: {text}"))
}
fn invalid() -> Error {
    Error::new(ErrorKind::InvalidInput, "generated job does not match its selected catalog model and parameters")
}
fn safe_error(_: Error) -> Error {
    invalid()
}

/// Limits may tighten, never enlarge, the finite preparation policy.
#[derive(Clone, Copy)]
pub struct AdmissionLimits {
    pub work: RunLimits,
    pub witness_bytes: usize,
    pub scratch_bytes: usize,
    pub snapshot_bytes: usize,
}
impl Default for AdmissionLimits {
    fn default() -> Self {
        Self { work: RunLimits::default(), witness_bytes: MAX_WITNESS_BYTES, scratch_bytes: MAX_SCRATCH_BYTES, snapshot_bytes: MAX_SNAPSHOT_BYTES }
    }
}
struct Selected {
    provider: String,
    remote: String,
    schema: ParamSchema,
    semantic: bool,
    /// Each mode's input roles as `(role, min, max)`, in catalog order, to tell which mode a
    /// job was planned for (#559).
    modes: Vec<Vec<(String, u8, u8)>>,
}
impl Selected {
    /// The one mode `inputs` fit, as the planner chose it; none when no mode or several fit.
    fn mode_of(&self, inputs: &[nodecraft_nodes::GenerateInput]) -> Option<usize> {
        let count = |role: &str| inputs.iter().filter(|i| i.role == role).count();
        let mut fitting = self.modes.iter().enumerate().filter(|(_, roles)| {
            inputs.iter().all(|i| roles.iter().any(|(role, _, _)| *role == i.role))
                && roles.iter().all(|(role, min, max)| (usize::from(*min)..=usize::from(*max)).contains(&count(role)))
        });
        match (fitting.next(), fitting.next()) {
            (Some((index, _)), None) => Some(index),
            _ => None,
        }
    }
}
/// Seed parameters a submission draws, with each one's upper bound (#162).
type Seeds = Vec<(String, Option<nodecraft_types::Unsigned>)>;
struct Allowed {
    id: String,
    expected: ParamValues,
    /// Seeds the run draws for this submission, with each one's upper bound (#162).
    seeds: Seeds,
    /// The narrower submissions of modes that send only some settings (#559), by mode index,
    /// each with its seeds. A job planned for that mode matches it exactly; defaults are never
    /// filled back in.
    narrowed: Vec<(usize, ParamValues, Seeds)>,
}
impl Allowed {
    fn new(model: &ModelDescriptor, expected: ParamValues, seeds: Seeds) -> Self {
        let mut narrowed = Vec::new();
        for (index, names) in model.modes.iter().enumerate().filter_map(|(i, mode)| Some((i, mode.params.as_ref()?))) {
            let sends = |name: &String| names.contains(name);
            let params: ParamValues = expected.iter().filter(|(k, _)| sends(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
            let drawn: Vec<_> = seeds.iter().filter(|(k, _)| sends(k)).cloned().collect();
            if params != expected || drawn != seeds {
                narrowed.push((index, params, drawn));
            }
        }
        Self { id: model.id.clone(), expected, seeds, narrowed }
    }
    /// The admitted submission a job with `inputs` is meant to send: the narrower one of the
    /// mode it was planned for, else the full one. Another mode's narrower settings never
    /// admit it.
    fn submission(&self, model: &Selected, inputs: &[nodecraft_nodes::GenerateInput]) -> (&ParamValues, &[(String, Option<nodecraft_types::Unsigned>)], bool) {
        let mode = model.mode_of(inputs);
        match self.narrowed.iter().find(|(index, _, _)| Some(*index) == mode) {
            Some((_, narrow, seeds)) => (narrow, seeds, true),
            None => (&self.expected, &self.seeds, false),
        }
    }
}
pub(crate) struct Witness {
    models: BTreeMap<String, Selected>,
    steps: BTreeMap<NodeId, Vec<Allowed>>,
    pub(crate) bytes: usize,
    pub(crate) limit: usize,
    scratch: usize,
    pub(crate) targets: nodecraft_types::TargetSelection,
}
pub(crate) struct Usage {
    pub(crate) steps: BTreeMap<NodeId, usize>,
    pub(crate) bytes: usize,
    pub(crate) limits: RunLimits,
}
/// Not Clone/Serialize: owns precisely the admitted work and snapshot.
pub struct AdmittedRunPlan {
    pub(crate) plan: RunPlan,
    pub(crate) canvas_id: CanvasId,
    pub(crate) canvas: Canvas,
    pub(crate) witness: Witness,
    pub(crate) usage: Usage,
}
impl AdmittedRunPlan {
    /// Borrow the already-admitted node identities, without allocating or reviving work.
    pub fn planned_nodes(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.plan.steps.iter().map(|s| s.node)
    }
    /// The canvas the run is admitted against: the real one, with any technique nodes inlined.
    pub fn canvas(&self) -> &Canvas {
        &self.canvas
    }
    /// Whether `node` was inlined from a technique, so it is on the admitted canvas only.
    pub fn is_inlined(&self, node: NodeId) -> bool {
        self.plan.inner.contains_key(&node)
    }
    /// Borrow precisely the selected model targets, including implicit defaults.
    pub fn selected_targets(&self) -> impl Iterator<Item = (NodeId, &str, Option<nodecraft_types::ProviderInstanceId>)> {
        self.witness.steps.iter().flat_map(move |(node, allowed)| {
            allowed.iter().filter_map(move |a| {
                let model = self.witness.models.get(&a.id)?;
                Some((*node, model.provider.as_str(), self.witness.targets.get(*node, &model.provider).and_then(|t| t.instance())))
            })
        })
    }
    /// Remaining logical witness allowance, before host-owned metadata is copied.
    pub fn remaining_metadata_bytes(&self) -> usize {
        self.witness.limit.saturating_sub(self.witness.bytes)
    }
    /// Host metadata owners must be admitted before constructing/copying them.
    pub fn reserve_metadata(&mut self, bytes: usize) -> Result<(), Error> {
        self.witness.reserve(bytes)
    }
}
impl Witness {
    fn reserve(&mut self, bytes: usize) -> Result<(), Error> {
        let n = measure::add(self.bytes, bytes)?;
        if n > self.limit {
            return Err(cap());
        }
        self.bytes = n;
        Ok(())
    }
    fn candidate<'a>(&'a self, node: NodeId, job: &'a JobSpec) -> Result<Option<nodecraft_types::BorrowedResolvedParams<'a>>, Error> {
        let JobSpec::Generate { provider, instance, model, params, inputs, .. } = job else { return Ok(None) };
        if self.targets.get(node, provider).is_none_or(|target| target.instance() != *instance) {
            return Err(invalid());
        }
        let mut chosen: Option<nodecraft_types::BorrowedResolvedParams<'a>> = None;
        for a in self.steps.get(&node).into_iter().flatten() {
            let s = self.models.get(&a.id).ok_or_else(invalid)?;
            if s.provider != *provider || s.remote != *model {
                continue;
            }
            if measure::scratch(&s.schema)? > self.scratch {
                return Err(cap());
            }
            let (expected, _, narrowed) = a.submission(s, inputs);
            let v = if s.semantic || narrowed {
                // The source's shared Contract was resolved before admission. A generated
                // job must match that EXACT submission; applying defaults here would revive
                // disabled/hidden settings. Equality only traverses admitted scalar values.
                if params.len() != expected.len() || params.iter().any(|(k, v)| expected.get(k) != Some(v)) {
                    continue;
                }
                nodecraft_types::BorrowedResolvedParams {
                    values: params.iter().map(|(k, v)| (k.as_str(), v)).collect(),
                    visible: params.keys().cloned().collect(),
                }
            } else {
                let Ok(v) = s.schema.resolve_view(params, true, false) else { continue };
                v
            };
            let same = v.values.iter().filter(|(k, _)| v.visible.contains(**k)).count() == expected.len()
                && v.values.iter().filter(|(k, _)| v.visible.contains(**k)).all(|(k, v)| expected.get(*k) == Some(*v));
            if !same {
                continue;
            }
            if let Some(prior) = &chosen {
                let equal = prior.values.iter().filter(|(k, _)| prior.visible.contains(**k)).eq(v.values.iter().filter(|(k, _)| v.visible.contains(**k)));
                if !equal {
                    return Err(invalid());
                }
            } else {
                chosen = Some(v);
            }
        }
        chosen.map(Some).ok_or_else(invalid)
    }
    /// The seeds to draw for a normalized `job` of `node`: those of the admitted submission it
    /// is, which every matching submission must agree on.
    pub(crate) fn seeds(&self, node: NodeId, job: &JobSpec) -> Result<&[(String, Option<nodecraft_types::Unsigned>)], Error> {
        let JobSpec::Generate { provider, model, params, inputs, .. } = job else { return Ok(&[]) };
        let mut chosen: Option<&[(String, Option<nodecraft_types::Unsigned>)]> = None;
        for a in self.steps.get(&node).into_iter().flatten() {
            let s = self.models.get(&a.id).ok_or_else(invalid)?;
            let (expected, seeds, _) = a.submission(s, inputs);
            if !s.semantic || s.provider != *provider || s.remote != *model || expected != params {
                continue;
            }
            match chosen {
                Some(prior) if prior != seeds => return Err(invalid()),
                _ => chosen = Some(seeds),
            }
        }
        Ok(chosen.unwrap_or(&[]))
    }
    /// Full vector preflight precedes first canonical map copy.
    pub(crate) fn normalize(&self, node: NodeId, jobs: &mut [JobSpec], usage: &mut Usage) -> Result<(), Error> {
        let mut charge = 0;
        for job in jobs.iter() {
            let resolved = self
                .candidate(node, job)
                .map_err(|e| blocked(node, if e.kind == ErrorKind::Budget { BlockerReason::OwnershipLimit } else { BlockerReason::CatalogMismatch }))?;
            charge = measure::add(charge, measure::job(job, resolved.as_ref())?)?;
        }
        usage.reserve(node, charge).map_err(|_| blocked(node, BlockerReason::OwnershipLimit))?;
        for job in jobs.iter_mut() {
            let canonical = {
                let resolved = self.candidate(node, job)?;
                resolved.map(|v| v.values.into_iter().filter(|(k, _)| v.visible.contains(*k)).map(|(k, v)| (k.to_owned(), v.clone())).collect::<ParamValues>())
            };
            if let (JobSpec::Generate { params, .. }, Some(map)) = (job, canonical) {
                #[cfg(test)]
                CANONICAL_COPIES.with(|n| n.set(n.get() + 1));
                *params = map;
            }
        }
        Ok(())
    }
}
impl Usage {
    pub(crate) fn reserve(&mut self, node: NodeId, n: usize) -> Result<(), Error> {
        let step = measure::add(self.steps.get(&node).copied().unwrap_or(0), n)?;
        let total = measure::add(self.bytes, n)?;
        if step > self.limits.step_bytes || total > self.limits.run_bytes {
            return Err(cap());
        }
        self.steps.insert(node, step);
        self.bytes = total;
        Ok(())
    }
}
fn resolution_scratch(model: &ModelDescriptor) -> Result<usize, Error> {
    let base = measure::scratch(&model.params)?;
    // Shared semantic resolution retains states and a submission simultaneously. Each
    // validated map/default aggregate is <=1 MiB; shells/keys fit in the extra two MiB.
    // Legacy borrowed resolution keeps its original allowance.
    let n = measure::add(base, if model.parameter_contract.is_some() { 4 * 1024 * 1024 } else { 0 })?;
    if n > MAX_SCRATCH_BYTES { Err(cap()) } else { Ok(n) }
}
struct Selection<'a> {
    node: NodeId,
    model: Result<&'a ModelDescriptor, Unpriced>,
    source: &'a Node,
}
fn selection<'a>(
    plan: &RunPlan,
    canvas: &'a Canvas,
    kinds: &Registry,
    catalog: &'a Catalog,
    scratch_limit: usize,
    strict: bool,
) -> Result<Vec<Selection<'a>>, Error> {
    let mut selected = Vec::new();
    for step in &plan.steps {
        if strict && step.jobs.as_ref().is_some_and(Vec::is_empty) {
            continue;
        }
        let node = canvas.nodes().get(&step.node).ok_or_else(invalid)?;
        node.validate_payload().map_err(safe_error)?;
        let kind = kinds.get(&node.kind).map_err(safe_error)?;
        // Same checked instance contract, but pre-admit validation scratch before its
        // schema Choice uniqueness indexes allocate. Native instance construction is trusted.
        let descriptor = kind.instance(node).map_err(safe_error)?;
        if measure::add(measure::mul(selected.len(), size_of::<Selection<'_>>() + 512)?, measure::scratch(&descriptor.params)?)? > scratch_limit {
            return Err(cap());
        }
        descriptor.validate().map_err(safe_error)?;
        if descriptor.id != node.kind || descriptor.id != kind.descriptor().id {
            return Err(invalid());
        }
        let has_generate = step.jobs.as_ref().is_some_and(|j| j.iter().any(|j| matches!(j, JobSpec::Generate { .. })));
        let controls = descriptor.params.params.iter().filter(|p| p.control == ParamControl::Model);
        let mut count = 0;
        for control in controls {
            if measure::mul(selected.len() + 1, size_of::<Selection<'_>>() + 512)? > scratch_limit {
                return Err(cap());
            }
            count += 1;
            let Some(value) = node.params.get(&control.name).or(control.default.as_ref()) else {
                if strict && (has_generate || step.jobs.is_none()) {
                    return Err(blocked(node.id, BlockerReason::CatalogMismatch));
                }
                selected.push(Selection { node: node.id, model: Err(Unpriced::NoModel), source: node });
                continue;
            };
            control.control.validate_value(value).map_err(safe_error)?;
            let Value::Text(id) = value else { return Err(invalid()) };
            let Some(model) = catalog.get(id) else {
                if strict && (has_generate || step.jobs.is_none()) {
                    return Err(blocked(node.id, BlockerReason::CatalogMismatch));
                }
                selected.push(Selection { node: node.id, model: Err(Unpriced::UnknownModel(id.clone())), source: node });
                continue;
            };
            let scratch = measure::add(measure::mul(selected.len() + 1, size_of::<Selection<'_>>())?, resolution_scratch(model)?)?;
            if scratch > scratch_limit {
                return Err(cap());
            }
            model.validate().map_err(safe_error)?;
            selected.push(Selection { node: node.id, model: Ok(model), source: node });
        }
        if count == 0 && (has_generate || descriptor.category == Category::Generation) {
            if strict && (has_generate || step.jobs.is_none()) {
                return Err(blocked(node.id, BlockerReason::CatalogMismatch));
            }
            selected.push(Selection { node: node.id, model: Err(Unpriced::Invalid("generation has no model selected".into())), source: node });
        }
        if let Some(why) = kind.unpriced(node) {
            if measure::mul(selected.len() + 1, size_of::<Selection<'_>>() + 512)? > scratch_limit {
                return Err(cap());
            }
            selected.push(Selection { node: node.id, model: Err(Unpriced::Invalid(why)), source: node });
        }
    }
    Ok(selected)
}
/// Finite shared selection/pricing path. Executable readiness is enforced by admit_run.
/// Preview callers do not acquire an executable capability.
pub fn quote(plan: &RunPlan, canvas: &Canvas, kinds: &Registry, catalog: &Catalog) -> Result<Quote, Error> {
    quote_with_scratch(plan, canvas, kinds, catalog, MAX_SCRATCH_BYTES)
}
pub fn quote_with_scratch(plan: &RunPlan, canvas: &Canvas, kinds: &Registry, catalog: &Catalog, scratch: usize) -> Result<Quote, Error> {
    let selected = selection(plan, canvas, kinds, catalog, scratch.min(MAX_SCRATCH_BYTES), false)?;
    if quote_bound(plan, &selected)? > scratch.min(MAX_SCRATCH_BYTES) {
        return Err(cap());
    }
    quote_selected(plan, &selected)
}
fn quote_bound(plan: &RunPlan, selected: &[Selection<'_>]) -> Result<usize, Error> {
    // Conservative logical reservation for every Quote/step/model/Unpriced/totals
    // owner; includes simultaneous report construction scratch, not RSS/capacity.
    let n = measure::add(
        size_of::<Quote>(),
        measure::add(measure::mul(plan.steps.len(), size_of::<StepQuote>() + 2048)?, measure::mul(selected.len(), size_of::<String>() + 1024)?)?,
    )?;
    let n = measure::add(n, measure::mul(selected.len(), size_of::<Selection<'_>>())?)?;
    let peak = selected.iter().filter_map(|s| s.model.as_ref().ok()).try_fold(0usize, |max, m| Ok::<_, Error>(max.max(resolution_scratch(m)?)))?;
    let n = measure::add(n, peak)?;
    if n > MAX_SCRATCH_BYTES {
        return Err(cap());
    }
    Ok(n)
}
/// The length of the source video wired into `role` (#179), the longest across a fanned step's
/// items, when every one is already known: a source made earlier in the run, or one whose length
/// wasn't read on import, leaves it unknown.
fn source_ms(step: &crate::planner::Step, role: &str) -> Option<u64> {
    let cells: Vec<&[crate::planner::PlannedInput]> = match &step.fanout {
        Some(fanout) => fanout.cells.iter().map(Vec::as_slice).collect(),
        None => vec![step.inputs.as_slice()],
    };
    let mut longest: Option<u64> = None;
    for inputs in cells {
        let input = inputs.iter().find(|input| input.port == role)?;
        let value = match input.from.as_slice() {
            [crate::planner::InputSource::Value(value)] => value,
            _ => return None,
        };
        let asset = match value {
            Value::Media(asset) => asset,
            Value::List(items) => match items.as_slice() {
                [Value::Media(asset)] => asset,
                _ => return None,
            },
            _ => return None,
        };
        longest = Some(longest.unwrap_or(0).max(asset.meta.duration_ms?));
    }
    longest
}
/// The PDF pages a quote counts for `document`'s role (#591), the most across a fanned step's
/// items; a PDF made earlier in the run, or never counted, counts as the most the mode reads.
fn document_pages(step: &crate::planner::Step, document: &nodecraft_types::DocumentUse) -> u32 {
    let cells: Vec<&[crate::planner::PlannedInput]> = match &step.fanout {
        Some(fanout) => fanout.cells.iter().map(Vec::as_slice).collect(),
        None => vec![step.inputs.as_slice()],
    };
    let mut most = 0u32;
    for inputs in cells {
        let mut counts = Vec::new();
        for input in inputs.iter().filter(|input| input.port == document.role) {
            for source in &input.from {
                let crate::planner::InputSource::Value(value) = source else {
                    counts.push(None);
                    continue;
                };
                let items = match value {
                    Value::List(items) => items.as_slice(),
                    value => std::slice::from_ref(value),
                };
                counts.extend(items.iter().map(|item| match item {
                    Value::Media(asset) => asset.meta.page_count,
                    _ => None,
                }));
            }
        }
        most = most.max(document.quoted_pages(counts));
    }
    most
}
fn quote_selected(plan: &RunPlan, selected: &[Selection<'_>]) -> Result<Quote, Error> {
    quote_bound(plan, selected)?;
    let mut result = Quote::default();
    let mut text_bytes = crate::planner::MAX_PLAN_INPUT_BYTES;
    for step in &plan.steps {
        let mut total: Option<Money> = None;
        let mut reason = None;
        let mut models = Vec::new();
        for s in selected.iter().filter(|s| s.node == step.node) {
            let model = match &s.model {
                Ok(model) => *model,
                Err(why) => {
                    // Only validated catalog identities enter the public models list.
                    // Later control reasons may replace this one, so rendering cannot
                    // safely infer membership from the final Unpriced variant.
                    reason = Some(why.clone());
                    continue;
                }
            };
            // Report owners are bounded before copy; a conservative per-entry text allowance
            // includes ID, Unpriced ID and fixed explanatory metadata.
            if measure::mul(models.len() + 1, 1024)? > MAX_SCRATCH_BYTES {
                return Err(cap());
            }
            #[cfg(test)]
            QUOTE_COPIES.with(|n| n.set(n.get() + 1));
            models.push(model.id.clone());
            if model.pricing == Pricing::Free && model.parameter_contract.is_none() {
                continue;
            }
            let source_ms = model.edit().and_then(|edit| source_ms(step, &edit.role));
            // The PDFs' prompt tokens, read by the mode this step runs.
            let document = model.mode(step.mode.as_str()).and_then(|mode| mode.document.as_ref());
            let input_tokens = document.map_or(0, |document| document.tokens(document_pages(step, document)));
            let price = model.quote_inputs(&s.source.params, source_ms, input_tokens).and_then(|price| match price {
                Some(_) => Ok(price),
                None => characters::quote(model, step, &mut text_bytes),
            });
            match price {
                Ok(Some(m)) => match total {
                    Some(t) => match t.checked_add(m) {
                        Ok(sum) => total = Some(sum),
                        Err(_) => reason = Some(Unpriced::Invalid("selected model currencies do not match or total overflows".into())),
                    },
                    None => total = Some(m),
                },
                Ok(None) if matches!(model.pricing, Pricing::PerSourceSecond { .. }) => reason = Some(Unpriced::AwaitsSource(model.id.clone())),
                Ok(None) => reason = Some(Unpriced::ByUsage(model.id.clone())),
                Err(_) => reason = Some(Unpriced::Invalid("invalid model pricing parameters".into())),
            }
        }
        let per_job = reason.map_or(Ok(total), Err);
        let item = StepQuote {
            node: step.node,
            models,
            per_job,
            // A fanned step (#92) planned later runs at least one job per item it runs, and one
            // of several variations (#81) at least one per variation.
            jobs: match (&step.jobs, &step.fanout) {
                (Some(jobs), _) => u64::try_from(jobs.len()).unwrap_or(u64::MAX),
                (None, Some(fanout)) => u64::try_from(fanout.running().max(1)).unwrap_or(u64::MAX),
                (None, None) => u64::try_from(step.variations.max(1)).unwrap_or(u64::MAX),
            },
            jobs_known: step.jobs.is_some(),
        };
        if let Ok(Some(m)) = item.total() {
            let old = result.totals.get(&m.currency).copied().unwrap_or(Money::zero(m.currency));
            if let Ok(sum) = old.checked_add(m) {
                result.totals.insert(m.currency, sum);
            } else {
                return Err(cap());
            }
        }
        result.steps.push(item);
    }
    Ok(result)
}
/// Consumes caller-owned inputs. The engine preflights its borrowed Canvas before cloning it.
pub fn admit_run(
    plan: RunPlan,
    canvas_id: CanvasId,
    canvas: Canvas,
    kinds: &Registry,
    catalog: &Catalog,
    limits: AdmissionLimits,
) -> Result<(AdmittedRunPlan, Quote), Error> {
    admit_run_with_targets(plan, canvas_id, canvas, kinds, catalog, limits, &nodecraft_types::EMPTY_TARGET_SELECTION)
}

/// Admit the same pure endpoint selection used by the first planning hook.
pub fn admit_run_with_targets(
    mut plan: RunPlan,
    canvas_id: CanvasId,
    canvas: Canvas,
    kinds: &Registry,
    catalog: &Catalog,
    limits: AdmissionLimits,
    targets: &nodecraft_types::TargetSelection,
) -> Result<(AdmittedRunPlan, Quote), Error> {
    use measure::{add, mul};
    // Fixed retained work roots are global run ownership, not a per-step fee.
    // Admit before selection/report/witness copies and transfer exactly once.
    let mut work = limits.work;
    work.list_items = work.list_items.min(crate::planner::MAX_LIST_ITEMS);
    work.step_bytes = work.step_bytes.min(crate::planner::MAX_STEP_INPUT_BYTES);
    work.run_bytes = work.run_bytes.min(crate::planner::MAX_PLAN_INPUT_BYTES);
    let mut usage = Usage { steps: BTreeMap::new(), bytes: measure::add(size_of::<RunPlan>(), size_of::<Usage>())?, limits: work };
    if usage.bytes > work.run_bytes {
        return Err(cap());
    }
    if canvas.id() != canvas_id || measure::canvas(&canvas)? > limits.snapshot_bytes.min(MAX_SNAPSHOT_BYTES) {
        return Err(cap());
    }
    if plan.steps.len() > canvas.nodes().len() {
        return Err(invalid());
    }
    let mut count = 0usize;
    for (index, step) in plan.steps.iter().enumerate() {
        if plan.steps.iter().take(index).any(|s| s.node == step.node) || step.after.iter().any(|id| !plan.steps.iter().take(index).any(|s| s.node == *id)) {
            return Err(invalid());
        }
        if canvas.nodes().get(&step.node).is_none_or(|n| n.kind != step.kind) {
            return Err(invalid());
        }
        if let Some(jobs) = &step.jobs {
            count = add(count, jobs.len())?;
            if jobs.len() > crate::run::MAX_STEP_JOBS || count > crate::run::MAX_RUN_JOBS {
                return Err(cap());
            }
        }
    }
    let selected = selection(&plan, &canvas, kinds, catalog, limits.scratch_bytes.min(MAX_SCRATCH_BYTES), true)?;
    // Inline canvas and work roots are charged by their own ledgers.
    let mut witness_size = size_of::<AdmittedRunPlan>().checked_sub(size_of::<Canvas>() + size_of::<RunPlan>() + size_of::<Usage>()).ok_or_else(cap)?;
    // Read-only aggregate preflight before copying any selected schema or default.
    for (index, s) in selected.iter().enumerate() {
        let Ok(model) = &s.model else {
            continue;
        };
        if !selected.iter().take(index).any(|other| other.model.as_ref().is_ok_and(|m| m.id == model.id)) {
            witness_size = add(
                witness_size,
                add(
                    size_of::<(String, Selected)>() + measure::LINK,
                    add(model.id.len(), add(model.provider.len(), add(model.provider_model.len(), measure::schema(&model.params)?)?)?)?,
                )?,
            )?;
        }
        let target = targets.get(s.node, &model.provider).ok_or_else(invalid)?;
        {
            witness_size = add(witness_size, target.logical_bytes().saturating_add(size_of::<nodecraft_types::TargetChoice>()))?;
        }
        let expected_charge = if model.parameter_contract.is_some() {
            // Temporary owned resolution/submission was pre-admitted by resolution_scratch;
            // this pass measures the retained witness BEFORE its later construction.
            let expected = model.effective_params(&s.source.params, true).map_err(|_| blocked(s.node, BlockerReason::CatalogMismatch))?;
            let seeds = model.random_seeds(&s.source.params, true).map_err(|_| blocked(s.node, BlockerReason::CatalogMismatch))?;
            let seeds = seeds.iter().try_fold(0, |n, (name, _)| add(n, add(name.len(), size_of::<(String, Option<nodecraft_types::Unsigned>)>())?))?;
            add(measure::params(&expected)?, seeds)?
        } else {
            let expected = model.params.resolve_view(&s.source.params, true, true).map_err(|_| blocked(s.node, BlockerReason::CatalogMismatch))?;
            measure::resolved(&expected)?
        };
        // Each mode that sends only some settings may keep one narrower copy as well.
        let copies = add(1, model.modes.iter().filter(|mode| mode.params.is_some()).count())?;
        witness_size = add(witness_size, add(size_of::<Allowed>(), add(model.id.len(), mul(copies, expected_charge)?)?)?)?;
    }
    witness_size = add(witness_size, mul(plan.steps.len(), size_of::<(NodeId, Vec<Allowed>)>() + measure::LINK)?)?;
    let limit = limits.witness_bytes.min(MAX_WITNESS_BYTES);
    if witness_size > limit {
        return Err(cap());
    }
    let report_charge = quote_bound(&plan, &selected)?;
    if report_charge > limits.scratch_bytes.min(MAX_SCRATCH_BYTES) {
        return Err(cap());
    }
    witness_size = add(witness_size, report_charge)?;
    if witness_size > limit {
        return Err(cap());
    }
    let priced = quote_selected(&plan, &selected)?;
    let mut witness = Witness {
        models: BTreeMap::new(),
        steps: BTreeMap::new(),
        bytes: witness_size,
        limit,
        scratch: limits.scratch_bytes.min(MAX_SCRATCH_BYTES),
        targets: nodecraft_types::TargetSelection::empty(),
    };
    for s in selected {
        let Ok(model) = s.model else {
            continue;
        };
        if let Some(target) = targets.get(s.node, &model.provider)
            && witness.targets.get(s.node, &model.provider).is_none()
        {
            #[cfg(test)]
            TARGET_COPIES.with(|n| n.set(n.get() + 1));
            witness.targets.insert(s.node, target)?;
        }
        witness.models.entry(model.id.clone()).or_insert_with(|| Selected {
            provider: model.provider.clone(),
            remote: model.provider_model.clone(),
            schema: model.params.clone(),
            semantic: model.parameter_contract.is_some(),
            modes: model.modes.iter().map(|mode| mode.inputs.iter().map(|i| (i.role.clone(), if i.inline { 0 } else { i.min }, i.max)).collect()).collect(),
        });
        let expected = model.effective_params(&s.source.params, true).map_err(safe_error)?;
        let seeds = model.random_seeds(&s.source.params, true).map_err(safe_error)?;
        witness.steps.entry(s.node).or_default().push(Allowed::new(model, expected, seeds));
    }
    for step in &plan.steps {
        usage.reserve(step.node, add(measure::step(step)?, size_of::<(NodeId, usize)>() + measure::LINK)?)?;
    }
    if let Some(first) = plan.steps.first() {
        usage.reserve(first.node, mul(plan.edges.len(), size_of::<(NodeId, NodeId)>())?)?;
    } else if !plan.edges.is_empty() {
        return Err(invalid());
    }
    for step in &mut plan.steps {
        if let Some(jobs) = &mut step.jobs {
            let node = canvas.nodes().get(&step.node).ok_or_else(invalid)?;
            crate::generation_contract::validate_outputs(node, jobs).map_err(|_| invalid())?;
            witness.normalize(step.node, jobs, &mut usage)?;
        }
    }
    Ok((AdmittedRunPlan { plan, canvas_id, canvas, witness, usage }, priced))
}

#[cfg(test)]
pub(crate) fn admit_default_fixture(
    plan: RunPlan,
    canvas_id: CanvasId,
    canvas: Canvas,
    kinds: &Registry,
    catalog: &Catalog,
    limits: AdmissionLimits,
) -> Result<(AdmittedRunPlan, Quote), Error> {
    let mut targets = nodecraft_types::TargetSelection::empty();
    for node in canvas.nodes().keys() {
        for adapter in ["mock", "fixture"] {
            targets.insert(*node, &nodecraft_types::ProviderTarget::default_instance(adapter)?)?;
        }
    }
    admit_run_with_targets(plan, canvas_id, canvas, kinds, catalog, limits, &targets)
}
