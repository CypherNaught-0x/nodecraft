//! One source-aware preparation for single and bulk model choices. No services or paid work.
use crate::{commands::Context, generation_catalog::GenerationCatalog};
use nodecraft_graph::{Change, Changeset, MAX_CHANGES, Node};
use nodecraft_nodes::generation::{KIND, MODEL, PROMPT};
use nodecraft_types::{CanvasId, DropReason, Error, ErrorKind, NodeId, ParamValues, Value};
use serde::{Deserialize, Serialize};
use std::io::Write;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub canvas: CanvasId,
    pub nodes: Vec<NodeId>,
    pub model: String,
}

/// Public reports contain parameter names and reasons, never dropped user values.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dropped {
    pub param: String,
    pub reason: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetPreview {
    pub id: NodeId,
    pub previous_model: Option<String>,
    /// Unknown stored selector text is not echoed into public notices.
    pub source_unavailable: bool,
    pub kept: Vec<String>,
    pub dropped: Vec<Dropped>,
    /// Destination defaults that did not carry over from the source contract.
    pub defaults: Vec<String>,
    /// Explicit carried values cleared by the destination's conditional reset policy.
    pub reset: Vec<String>,
    pub changed: bool,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub binding: String,
    pub revision: u64,
    pub model: String,
    pub targets: Vec<TargetPreview>,
}

pub struct PreparedSwap {
    pub preview: Preview,
    pub changes: Changeset,
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn budget() -> Error {
    Error::new(ErrorKind::Budget, "model migration exceeds the existing replay or complete-report limit; select fewer nodes")
}
fn add(a: usize, b: usize) -> Result<usize, Error> {
    a.checked_add(b).ok_or_else(budget)
}
fn allocation() -> Error {
    Error::new(ErrorKind::Internal, "model migration storage could not be reserved")
}

struct Counter {
    bytes: usize,
    limit: usize,
}
impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.checked_add(bytes.len()).filter(|n| *n <= self.limit).ok_or_else(|| std::io::Error::other("migration size limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encoded(value: &impl Serialize, limit: usize) -> Result<usize, Error> {
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value).map_err(|_| budget())?;
    Ok(counter.bytes)
}

/// Checks the exact target set before cloning source payloads or migration values. Sorted
/// IDs make binding independent of request ordering; duplicate IDs are always an error.
fn ids(selection: &Selection) -> Result<Vec<NodeId>, Error> {
    if selection.nodes.is_empty() || selection.nodes.len() > MAX_CHANGES || !nodecraft_types::model_namespace(&selection.model) {
        return Err(invalid("model selection needs bounded unique node IDs and a catalog model ID"));
    }
    let mut ids = Vec::new();
    ids.try_reserve_exact(selection.nodes.len()).map_err(|_| allocation())?;
    ids.extend_from_slice(&selection.nodes);
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair.first() == pair.get(1)) {
        return Err(invalid("model selection contains duplicate node IDs"));
    }
    Ok(ids)
}

/// Cheap stale-preview check. Uses the existing execution/catalog identity; no new nonce,
/// document serialization, service invocation or candidate node clone occurs here.
pub fn binding(c: &Context<'_>, selection: &Selection) -> Result<String, Error> {
    let ids = ids(selection)?;
    let catalog = GenerationCatalog::current(c.resources)?;
    let canvas = c.document.canvases().get(&selection.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    for id in &ids {
        let node = canvas.nodes().get(id).ok_or_else(|| Error::new(ErrorKind::NotFound, "selected node does not exist"))?;
        if node.kind != KIND {
            return Err(Error::new(ErrorKind::Unsupported, "this node has no supported model-swap contract"));
        }
        catalog.validate_instance_binding(c.kinds.get(&node.kind)?, node)?;
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.model-swap.v1");
    c.execution.bind(&mut hash);
    catalog.stamp().bind(&mut hash);
    hash.update(&c.revision.to_le_bytes());
    hash.update(c.document.id().as_bytes());
    hash.update(selection.canvas.as_bytes());
    hash.update(&u64::try_from(ids.len()).map_err(|_| budget())?.to_le_bytes());
    for id in ids {
        hash.update(id.as_bytes());
    }
    hash.update(&u64::try_from(selection.model.len()).map_err(|_| budget())?.to_le_bytes());
    hash.update(selection.model.as_bytes());
    Ok(hash.finalize().to_hex().to_string())
}

fn reason(reason: DropReason) -> &'static str {
    match reason {
        DropReason::NotInSource => "not_in_source",
        DropReason::NotInTarget => "not_in_target",
        DropReason::ControlChanged => "control_changed",
        DropReason::UnitChanged => "unit_changed",
        DropReason::MeaningChanged => "meaning_changed",
        DropReason::UnverifiedMeaning => "unverified_meaning",
        DropReason::InvalidInTarget => "invalid_in_target",
    }
}
fn selected_model(node: &Node) -> Result<Option<&str>, Error> {
    match node.params.get(MODEL) {
        Some(value @ Value::Text(id)) => {
            // Saved Model values predate the current catalog-ID grammar. Their existing
            // bounded value contract still applies; an absent exact lookup means explicit
            // source-unavailable recovery, never an inferred replacement model.
            nodecraft_types::ParamControl::Model.validate_value(value)?;
            Ok(Some(id))
        }
        None => Ok(None),
        _ => Err(invalid("stored model selector must be text")),
    }
}

/// Prepare the complete all-target proposal. The first pass is borrowed: source payloads,
/// destination defaults/ports and complete report envelopes are admitted before migration
/// clones. Session later stages/persists this ONE changeset, including inverse admission.
pub fn prepare(c: &Context<'_>, selection: &Selection) -> Result<PreparedSwap, Error> {
    let token = binding(c, selection)?;
    let ids = ids(selection)?;
    let catalog = GenerationCatalog::current(c.resources)?;
    let destination = catalog.model(&selection.model).ok_or_else(|| Error::new(ErrorKind::Unsupported, "destination model is unavailable"))?;
    destination.parameters.validate()?;
    let canvas = c.document.canvases().get(&selection.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let limit = crate::session::MAX_HISTORY_BYTES;
    // A serialized destination contract conservatively includes every possible default.
    // Every declared role is counted, even when several modes share the same port.
    let mut destination_bytes = add(1024, encoded(&destination.parameters, limit)?)?;
    for mode in &destination.modes {
        for input in &mode.inputs {
            destination_bytes = add(destination_bytes, add(256, encoded(&input.role, limit)?)?)?;
        }
    }
    let mut replay_bytes = 12usize;
    let mut report_bytes = 256usize;
    let mut report_items = 16usize;
    for id in &ids {
        let node = canvas.nodes().get(id).ok_or_else(|| Error::new(ErrorKind::NotFound, "selected node does not exist"))?;
        node.validate_payload()?;
        selected_model(node)?;
        let wiring = catalog.capture_wiring(canvas, *id)?;
        catalog.validate_destination_wiring(&wiring, &selection.model, true)?;
        // Original plus an overestimate of replacement: old fields/history/config remain
        // intact, while destination defaults/ports may all be additional. No source Vec or
        // Value is copied merely to find that the whole selection cannot fit.
        let source = encoded(node, limit)?;
        replay_bytes = add(replay_bytes, add(source, add(source, destination_bytes)?)?)?;
        report_bytes = add(report_bytes, add(512, encoded(&selected_model(node)?, limit)?)?)?;
        let fields = add(node.params.len(), destination.parameters.schema.params.len())?;
        report_items = add(report_items, add(16, fields.checked_mul(8).ok_or_else(budget)?)?)?;
        for spec in &destination.parameters.schema.params {
            report_bytes = add(report_bytes, add(128, encoded(&spec.name, limit)?)?)?;
        }
        for name in node.params.keys() {
            report_bytes = add(report_bytes, add(128, encoded(name, limit)?)?)?;
        }
        if replay_bytes > limit || report_bytes > 1024 * 1024 || report_items > 16_384 {
            return Err(budget());
        }
    }
    let mut targets = Vec::new();
    let mut changes = Vec::new();
    targets.try_reserve_exact(ids.len()).map_err(|_| allocation())?;
    changes.try_reserve_exact(ids.len()).map_err(|_| allocation())?;
    for id in ids {
        let node = canvas.nodes().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "selected node does not exist"))?;
        let previous = selected_model(node)?;
        let source = previous.and_then(|id| catalog.model(id));
        let model_values: ParamValues = node
            .params
            .iter()
            .filter(|(name, _)| name.as_str() != MODEL && name.as_str() != PROMPT)
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let (mut proposed, dropped) = if let Some(source) = source {
            let migration = source.parameters.migrate(&destination.parameters, &model_values)?;
            let dropped = migration.dropped.into_iter().map(|entry| Dropped { param: entry.param, reason: reason(entry.reason).into() }).collect();
            (migration.kept, dropped)
        } else {
            // No fabricated source contract. An explicit preview accounts for every opaque
            // override being removed; the source node remains unchanged and Undo retains it.
            let dropped = model_values.keys().map(|name| Dropped { param: name.clone(), reason: "source_unavailable".into() }).collect();
            (ParamValues::new(), dropped)
        };
        let resolved = destination.parameters.resolve(&proposed, false)?;
        if !resolved.issues.is_empty() {
            return Err(invalid("destination model settings fail their shared parameter contract"));
        }
        let reset: Vec<_> = resolved.params.iter().filter(|state| state.reset).map(|state| state.name.clone()).collect();
        for name in &reset {
            proposed.remove(name);
        }
        let candidate = catalog.prepare_instance(node, &selection.model, &proposed)?;
        catalog.validate_wiring(canvas, &candidate, false)?;
        // All image definitions have the same `out: Image,many` output. Thus every selected
        // candidate's incoming checks see unchanged opposite outputs, even in a bulk swap.
        // A future varying-output kind must validate a complete candidate endpoint overlay.
        let defaults = destination
            .parameters
            .schema
            .params
            .iter()
            .filter(|spec| !proposed.contains_key(&spec.name) && candidate.params.contains_key(&spec.name))
            .map(|spec| spec.name.clone())
            .collect();
        let kept = proposed.keys().cloned().collect();
        let changed = node != &candidate;
        targets.push(TargetPreview {
            id,
            previous_model: source.map(|model| model.id.clone()),
            source_unavailable: source.is_none(),
            kept,
            dropped,
            defaults,
            reset,
            changed,
        });
        if changed {
            changes.push(Change::SetNode { canvas: selection.canvas, node: candidate });
        }
    }
    let preview = Preview { binding: token, revision: c.revision, model: selection.model.clone(), targets };
    // Validate the actual complete public result too; the borrowed preflight intentionally
    // overcounts field names/items rather than truncating any required notice.
    let result = serde_json::to_value(&preview).map_err(|_| invalid("model preview could not be encoded"))?;
    crate::commands::validate_json(&result)?;
    let changes = Changeset::new(changes);
    changes.validate_replay_budget(limit.saturating_sub(12))?;
    Ok(PreparedSwap { preview, changes })
}
