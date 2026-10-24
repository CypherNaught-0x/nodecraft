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
struct Classes {
    hashes: Vec<[u8; 32]>,
    incomplete: bool,
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
        Self { hashes, incomplete }
    }

    fn contains(&self, class: &str) -> bool {
        self.incomplete || self.hashes.binary_search(blake3::hash(class.as_bytes()).as_bytes()).is_ok()
    }
}

#[derive(Default)]
struct Known {
    servers: BTreeMap<Option<ProviderInstanceId>, Classes>,
    stamp: [u8; 32],
}

/// Install only after a successful fetch. `None` is the unnamed/default compatibility route;
/// named routes use the same stable instance identity as the provider router.
pub(crate) fn keep(session: &mut Session, instance: Option<ProviderInstanceId>, catalog: &NodeCatalog) {
    let mut known = session.swap_resource::<Known>(None).unwrap_or_default();
    if known.servers.contains_key(&instance) || known.servers.len() < crate::settings::MAX_COMFY_SERVERS.saturating_add(1) {
        known.servers.insert(instance, Classes::read(catalog));
        changed(&mut known);
    }
    session.swap_resource(Some(known));
}

pub(crate) fn forget(session: &mut Session, instance: Option<ProviderInstanceId>) {
    if let Some(mut known) = session.swap_resource::<Known>(None) {
        if known.servers.remove(&instance).is_some() {
            changed(&mut known);
        }
        session.swap_resource(Some(known));
    }
}

fn changed(known: &mut Known) {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nodecraft.comfy-partners.v1");
    hash.update(&known.stamp);
    for (instance, classes) in &known.servers {
        hash.update(&[u8::from(instance.is_some()), u8::from(classes.incomplete)]);
        if let Some(instance) = instance {
            hash.update(instance.as_bytes());
        }
        hash.update(&(classes.hashes.len() as u64).to_le_bytes());
        for class in &classes.hashes {
            hash.update(class);
        }
    }
    known.stamp = *hash.finalize().as_bytes();
}

pub(crate) fn bind(resources: &Resources, hash: &mut blake3::Hasher) {
    hash.update(&resources.get::<Known>().map_or([0; 32], |known| known.stamp));
}

/// The same adjustment runs for the displayed quote and its admitted run. It changes only a
/// free step, so totals remain unchanged; stored partner evidence already takes the kind path.
pub(crate) fn apply(resources: &Resources, targets: &TargetSelection, canvas: &Canvas, quote: &mut Quote) -> Result<(), Error> {
    let Some(known) = resources.get::<Known>() else { return Ok(()) };
    for step in &mut quote.steps {
        if step.per_job != Ok(None) {
            continue;
        }
        let Some(node) = canvas.nodes().get(&step.node).filter(|node| node.kind == comfy_workflow::ID) else { continue };
        let Some(target) = targets.get(node.id, comfy_workflow::PROVIDER) else { continue };
        let Some(classes) = known.servers.get(&target.instance()) else { continue };
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
    Ok(())
}

#[cfg(test)]
mod tests;
