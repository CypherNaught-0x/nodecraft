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
use nodecraft_types::{CanvasId, Currency, Error, ErrorKind, Money, NodeId, ParamControl, ParamSchema, ParamValues, Value};
use std::{collections::BTreeMap, mem::size_of};
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
}
struct Allowed {
    id: String,
    expected: ParamValues,
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
        let JobSpec::Generate { provider, instance, model, params, .. } = job else { return Ok(None) };
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
            let Ok(v) = s.schema.resolve_view(params, true, false) else { continue };
            let same = v.values.iter().filter(|(k, _)| v.visible.contains(**k)).count() == a.expected.len()
                && v.values.iter().filter(|(k, _)| v.visible.contains(**k)).all(|(k, v)| a.expected.get(*k) == Some(*v));
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
            let scratch = measure::add(measure::mul(selected.len() + 1, size_of::<Selection<'_>>())?, measure::scratch(&model.params)?)?;
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
    let peak = selected.iter().filter_map(|s| s.model.as_ref().ok()).try_fold(0usize, |max, m| Ok::<_, Error>(max.max(measure::scratch(&m.params)?)))?;
    let n = measure::add(n, peak)?;
    if n > MAX_SCRATCH_BYTES {
        return Err(cap());
    }
    Ok(n)
}
fn quote_selected(plan: &RunPlan, selected: &[Selection<'_>]) -> Result<Quote, Error> {
    quote_bound(plan, selected)?;
    let mut result = Quote::default();
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
            if model.pricing == Pricing::Free {
                continue;
            }
            let v = model.params.resolve_view(&s.source.params, false, true).map_err(safe_error)?;
            match model
                .pricing
                .quote_lookup(|key| if v.visible.contains(key) { v.values.get(key).copied() } else { None }, model.pricing.currency().unwrap_or(Currency::USD))
            {
                Ok(Some(m)) => match total {
                    Some(t) => match t.checked_add(m) {
                        Ok(sum) => total = Some(sum),
                        Err(_) => reason = Some(Unpriced::Invalid("selected model currencies do not match or total overflows".into())),
                    },
                    None => total = Some(m),
                },
                Ok(None) => reason = Some(Unpriced::ByUsage(model.id.clone())),
                Err(_) => reason = Some(Unpriced::Invalid("invalid model pricing parameters".into())),
            }
        }
        let per_job = reason.map_or(Ok(total), Err);
        let item = StepQuote {
            node: step.node,
            models,
            per_job,
            jobs: step.jobs.as_ref().map_or(1, |j| u64::try_from(j.len()).unwrap_or(u64::MAX)),
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
        let expected = model.params.resolve_view(&s.source.params, true, true).map_err(|_| blocked(s.node, BlockerReason::CatalogMismatch))?;
        witness_size = add(witness_size, add(size_of::<Allowed>(), add(model.id.len(), measure::resolved(&expected)?)?)?)?;
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
        });
        let v = model.params.resolve_view(&s.source.params, true, true).map_err(safe_error)?;
        let expected = v.values.into_iter().filter(|(k, _)| v.visible.contains(*k)).map(|(k, v)| (k.to_owned(), v.clone())).collect();
        witness.steps.entry(s.node).or_default().push(Allowed { id: model.id.clone(), expected });
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
