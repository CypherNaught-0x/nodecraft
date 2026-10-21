//! Shared renderer/marquee admission. No descriptor or effective Value is retained by geometry.
use super::*;

pub(crate) struct Admitted<'a> {
    pub descriptor: Cow<'a, nodecraft_nodes::NodeDescriptor>,
    pub visible: BTreeSet<String>,
    pub enabled: Option<BTreeSet<String>>,
    pub roles: crate::card::Roles,
    pub bytes: usize,
}
pub(super) fn admit<'a>(session: &'a Session, node: &Node, cache: Option<&SchemaCache>) -> Option<Admitted<'a>> {
    let descriptor = session.kinds().get(&node.kind).ok()?.resolve_instance(node).ok()?;
    if node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
        return None;
    }
    let enabled: Option<BTreeSet<String>>;
    let visible: BTreeSet<String>;
    let mut roles = crate::card::Roles::default();
    if nodecraft_nodes::generation::is_generation(&node.kind) {
        let catalog = session.generation_catalog().ok()?;
        let kind = session.kinds().get(&node.kind).ok()?;
        catalog.validate_definition_binding(kind).ok()?;
        let contract = kind.instance_contract(node).ok()?;
        let resolution = contract.resolve(&node.params, false).ok()?;
        roles = crate::card::Roles::of(&contract);
        // A model setting the wired mode doesn't send is hidden (#559); the card's own stay.
        let mode = wired_mode(session, node, &contract);
        let shown = |state: &&nodecraft_types::ParamState| {
            mode.is_none_or(|mode| {
                mode.sends(&state.name) || !contract.owned_groups.iter().any(|group| group.schema.params.iter().any(|param| param.name == state.name))
            })
        };
        visible = resolution.params.iter().filter(|state| state.visible).filter(shown).map(|state| state.name.clone()).collect();
        enabled = Some(resolution.params.iter().filter(|state| state.enabled).filter(shown).map(|state| state.name.clone()).collect());
    } else {
        let proof = descriptor.params.validated().ok()?;
        visible = proof.resolve_borrowed(&node.params, false).ok()?.visible;
        enabled = None;
    }
    let known = match &descriptor {
        Cow::Borrowed(value) => cache.and_then(|cache| cache.shared.get(&(*value as *const nodecraft_nodes::NodeDescriptor as usize))).map(|slot| slot.1),
        Cow::Owned(_) => None,
    };
    let bytes = known.or_else(|| descriptor_bytes(&descriptor))?;
    let overhead = visible
        .iter()
        .chain(enabled.iter().flatten())
        .fold(256usize, |total, name| total.saturating_add(name.len().saturating_add(96)))
        .saturating_add(roles.bytes());
    if bytes.checked_add(overhead)? > MAX_SCHEMA_CACHE_BYTES {
        return None;
    }
    Some(Admitted { descriptor, visible, enabled, roles, bytes })
}

/// The mode `node`'s wiring chooses, when it chooses one; `None` while it fits none or several.
fn wired_mode<'c>(session: &Session, node: &Node, contract: &'c nodecraft_nodes::NodeContract) -> Option<&'c nodecraft_nodes::ModeContract> {
    if contract.modes.is_empty() {
        return None;
    }
    let canvas = session.document().canvases().values().find(|canvas| canvas.nodes().contains_key(&node.id))?;
    let id = contract.resolve_mode(&wire_counts(canvas, contract, node), false).ok()??;
    contract.modes.iter().find(|mode| mode.mode == *id)
}

/// How many items reach each of `node`'s inputs, counted as the planner counts them: the items
/// each wired output holds now, or one for an output with no result yet.
pub(super) fn wire_counts<'a>(canvas: &'a Canvas, contract: &nodecraft_nodes::NodeContract, node: &Node) -> BTreeMap<&'a str, usize> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for edge in canvas.edges().values().filter(|edge| edge.to.node == node.id) {
        if contract.inputs.iter().any(|input| input.port == edge.to.port) {
            let items = canvas.nodes().get(&edge.from.node).and_then(|from| held(from, &edge.from.port)).unwrap_or(1);
            let count = counts.entry(edge.to.port.as_str()).or_default();
            *count = count.saturating_add(items);
        }
    }
    counts
}

/// How many items `node`'s output `port` holds in its shown result, lists flattened.
fn held(node: &Node, port: &str) -> Option<usize> {
    fn items(value: &Value, depth: usize) -> usize {
        match value {
            Value::List(values) if depth < 4 => values.iter().fold(0usize, |total, value| total.saturating_add(items(value, depth.saturating_add(1)))),
            Value::List(_) => 0,
            _ => 1,
        }
    }
    let at = node.outputs.iter().position(|output| output.name == port)?;
    let shown = node.history.iter().find(|generation| Some(generation.id) == node.active)?;
    shown.outputs.get(at).map(|value| items(value, 0))
}

/// Navigation needs no view-cache access; keep the cached implementation private to App.
pub(crate) fn admit_uncached<'a>(session: &'a Session, node: &Node) -> Option<Admitted<'a>> {
    admit(session, node, None)
}

/// Existing runtime identity only; no retained catalog/descriptor owner or document scan.
pub(crate) fn definition_snapshot(session: &Session) -> Option<[u8; 32]> {
    let catalog = session.generation_catalog().ok()?;
    // Every generation kind is installed from one snapshot, so the image kind (always present)
    // stands for all of them.
    let kind = session.kinds().get(nodecraft_nodes::generation::KIND).ok()?;
    catalog.validate_definition_binding(kind).ok()?;
    kind.definition_snapshot()
}

// Logical retention includes a fixed root allowance and conservative per-entry overhead.
// The sorted Vec has no per-entry heap children. Capacity is checked separately, not assumed.
const MAX_ENTRIES: usize = 4095;
const MAX_BYTES: usize = 512 * 1024;
const _: () = assert!(std::mem::size_of::<(NodeId, Rect, bool)>() <= 128);
#[derive(Default)]
pub(super) struct GeometryCache {
    identity: Option<Arc<crate::navigation::SessionIdentity>>,
    key: Option<(CanvasId, u64, ThemeKind)>,
    catalog: Option<[u8; 32]>,
    entries: Vec<(NodeId, Rect, bool)>,
}
const _: () = assert!(std::mem::size_of::<GeometryCache>() <= 128);
impl GeometryCache {
    pub fn prepare(&mut self, session: &mut Session, canvas: CanvasId, theme: ThemeKind) -> bool {
        let identity = crate::navigation::Geometry::identity(session);
        let changed = self.identity.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &identity));
        let key = (canvas, session.revision(), theme);
        // Catalog resources can be replaced without a document revision. Match the
        // generated renderer's opaque definition binding before reusing rect/placeholder.
        let catalog = definition_snapshot(session);
        let invalidated = changed || self.key != Some(key) || self.catalog != catalog;
        if invalidated {
            self.clear();
        }
        self.identity = Some(identity);
        self.key = Some(key);
        self.catalog = catalog;
        invalidated
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub fn retained_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<(NodeId, Rect, bool)>()
    }
    pub fn clear(&mut self) {
        self.entries = Vec::new();
    }
    pub fn placeholder(&self, id: NodeId) -> bool {
        self.entries.binary_search_by_key(&id, |x| x.0).ok().and_then(|i| self.entries.get(i)).is_some_and(|x| x.2)
    }
    pub fn rect(&mut self, session: &Session, node: &Node, t: &Tokens) -> Rect {
        let index = match self.entries.binary_search_by_key(&node.id, |x| x.0) {
            Ok(i) => return self.entries.get(i).map_or_else(|| card::world_rect(node, None, t), |x| x.1),
            Err(i) => i,
        };
        let admitted = admit(session, node, None);
        let rect = card::world_rect(node, admitted.as_ref().map(|a| a.descriptor.as_ref()), t);
        let placeholder = admitted.is_none();
        let entry_limit = MAX_ENTRIES.min(MAX_BYTES.saturating_sub(128) / std::mem::size_of::<(NodeId, Rect, bool)>());
        if self.entries.len() < entry_limit {
            if self.entries.len() == self.entries.capacity() {
                // Batch growth for ascending candidates; sorted insertion may still shift other entries.
                let target = self.entries.capacity().saturating_mul(2).max(16).min(entry_limit);
                if self.entries.try_reserve_exact(target.saturating_sub(self.entries.len())).is_err() {
                    return rect;
                }
            }
            if self.entries.capacity().checked_mul(std::mem::size_of::<(NodeId, Rect, bool)>()).and_then(|n| n.checked_add(128)).is_none_or(|n| n > MAX_BYTES) {
                self.clear();
                return rect;
            }
            self.entries.insert(index, (node.id, rect, placeholder));
        }
        rect
    }
}
