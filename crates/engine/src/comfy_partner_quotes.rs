//! Runtime evidence for legacy workflow cards that predate stored partner-node names (#872).
//! Only the selected server's last successful node list may change a free quote. This does not
//! migrate the document; refresh/forget changes the quote binding instead.
use crate::{Session, commands::Resources};
use nodecraft_comfy::{ApiWorkflow, NodeCatalog};
use nodecraft_exec::cost::{Quote, Unpriced};
use nodecraft_graph::Canvas;
use nodecraft_nodes::comfy_workflow;
use nodecraft_types::{Error, ErrorKind, ProviderInstanceId, TargetSelection, Value};
use std::collections::BTreeMap;

const MAX_CLASSES: usize = 4_096;
const REASON: &str = "its ComfyUI partner nodes spend comfy.org credits NodeCraft can't price";

/// Fixed-size hashes avoid retaining entire server catalogs or arbitrary class-name payloads.
/// An oversized list conservatively marks workflows unknown rather than forgetting paid nodes.
#[derive(PartialEq, Eq)]
struct Classes {
    hashes: Vec<[u8; 32]>,
    incomplete: bool,
    digest: [u8; 32],
}

impl Classes {
    fn read(catalog: &NodeCatalog) -> Self {
        let mut hashes = Vec::new();
        let mut incomplete = false;
        for node in catalog.iter().filter(|node| node.api_node) {
            if hashes.len() == MAX_CLASSES {
                incomplete = true;
                break;
            }
            hashes.push(*blake3::hash(node.class_type.as_bytes()).as_bytes());
        }
        hashes.sort_unstable();
        hashes.dedup();
        let mut hash = blake3::Hasher::new();
        hash.update(b"nodecraft.comfy-partner-classes.v1");
        hash.update(&[u8::from(incomplete)]);
        hash.update(&(hashes.len() as u64).to_le_bytes());
        for class in &hashes {
            hash.update(class);
        }
        Self { hashes, incomplete, digest: *hash.finalize().as_bytes() }
    }

    fn contains(&self, class: &str) -> bool {
        self.incomplete || self.hashes.binary_search(blake3::hash(class.as_bytes()).as_bytes()).is_ok()
    }
}

#[derive(Default)]
struct Known {
    servers: BTreeMap<Option<ProviderInstanceId>, Classes>,
}

/// Install only after a successful fetch. `None` is the unnamed/default compatibility route;
/// named routes use the same stable instance identity as the provider router.
pub(crate) fn keep(session: &mut Session, instance: Option<ProviderInstanceId>, catalog: &NodeCatalog) {
    let mut known = session.swap_resource::<Known>(None).unwrap_or_default();
    if known.servers.contains_key(&instance) || known.servers.len() < crate::settings::MAX_COMFY_SERVERS.saturating_add(1) {
        let classes = Classes::read(catalog);
        if known.servers.get(&instance) != Some(&classes) {
            known.servers.insert(instance, classes);
        }
    }
    session.swap_resource(Some(known));
}

pub(crate) fn forget(session: &mut Session, instance: Option<ProviderInstanceId>) {
    if let Some(mut known) = session.swap_resource::<Known>(None) {
        known.servers.remove(&instance);
        session.swap_resource(Some(known));
    }
}

/// Bind current evidence only for routes selected in this execution context. A panel check of
/// an unrelated profile or an identical refresh must not revoke open consent. The routing
/// authority already binds its selections separately; no planner pass or workflow parse belongs
/// here. At most 33 fixed-size server digests are added to this intermediate hash.
pub(crate) fn bind(resources: &Resources, targets: &TargetSelection, hash: &mut blake3::Hasher) {
    let mut evidence = blake3::Hasher::new();
    evidence.update(b"nodecraft.comfy-partners.v2");
    if let Some(known) = resources.get::<Known>() {
        for (instance, classes) in &known.servers {
            let selected = targets.iter().any(|choice| choice.target().adapter() == comfy_workflow::PROVIDER && choice.target().instance() == *instance);
            if !selected {
                continue;
            }
            evidence.update(&[u8::from(instance.is_some())]);
            if let Some(instance) = instance {
                evidence.update(instance.as_bytes());
            }
            evidence.update(&classes.digest);
        }
    }
    hash.update(evidence.finalize().as_bytes());
}

/// The same adjustment runs for the displayed quote and its admitted run. Hosted GPU time is
/// unknown even when a custom catalog prices the logical model; local partner evidence changes
/// only free steps, since stored partner evidence already takes the kind path.
pub(crate) fn apply(resources: &Resources, routing: &crate::provider_routing::ProviderRouting, canvas: &Canvas, quote: &mut Quote) -> Result<(), Error> {
    let known = resources.get::<Known>();
    let mut hosted = false;
    for step in &mut quote.steps {
        let Some(node) = canvas.nodes().get(&step.node) else { continue };
        let Some(target) = routing.targets().get(node.id, comfy_workflow::PROVIDER) else { continue };
        if routing.find_parts(target.adapter(), target.instance())?.metered_comfy() {
            step.per_job = Err(Unpriced::Invalid("the hosted ComfyUI profile bills GPU time; the amount is unknown before execution".into()));
            hosted = true;
            continue;
        }
        if step.per_job != Ok(None) || node.kind != comfy_workflow::ID {
            continue;
        }
        let Some(classes) = known.and_then(|known| known.servers.get(&target.instance())) else { continue };
        if classes.hashes.is_empty() && !classes.incomplete {
            continue;
        }
        let Some(Value::Text(text)) =
            node.config.get(comfy_workflow::WORKFLOW).filter(|value| matches!(value, Value::Text(text) if text.len() <= comfy_workflow::MAX_WORKFLOW_BYTES))
        else {
            return Err(Error::new(ErrorKind::InvalidInput, "a ComfyUI workflow card's workflow is missing or too large"));
        };
        let workflow =
            ApiWorkflow::parse(text.as_bytes(), None).map_err(|_| Error::new(ErrorKind::InvalidInput, "a ComfyUI workflow card's workflow is malformed"))?;
        if workflow.nodes.values().any(|node| classes.contains(&node.class_type)) {
            step.per_job = Err(Unpriced::Invalid(REASON.into()));
        }
    }
    if hosted {
        // Preserve every unrelated price/currency, including known zero and zero-job steps.
        // Rebuilding avoids leaving a custom catalog's removed hosted price in the totals.
        quote.totals.clear();
        for step in &quote.steps {
            if let Ok(Some(total)) = step.total() {
                let sum = quote.totals.entry(total.currency).or_insert_with(|| nodecraft_types::Money::zero(total.currency));
                *sum = sum.checked_add(total)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
