//! A legacy card can preview an additive input migration without editing its document.
use super::*;
use nodecraft_graph::{Node, Port};
use nodecraft_nodes::NodeKind;

/// Only a warning-free extension of the existing inputs can be accepted by dropping a wire.
/// Everything else still uses the explicit Update card command. Called once per schema-cache
/// revision, never retained with the node's history or assets.
pub(super) fn extension(kind: &dyn NodeKind, node: &Node) -> Option<Vec<Port>> {
    let mut migration = kind.prepare_migration(node).ok()??;
    if !migration.warnings.is_empty() || migration.node.inputs.len() <= node.inputs.len() || !migration.node.inputs.starts_with(&node.inputs) {
        return None;
    }
    let inputs = std::mem::replace(&mut migration.node.inputs, node.inputs.clone());
    (migration.node == *node).then_some(inputs)
}

const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;

/// Stable for the whole paint/wire pass: unlike the schema LRU, this cache never evicts a card
/// after it has drawn. Hitting the bound leaves further cards on their saved inputs.
#[derive(Default)]
pub(super) struct Cache {
    identity: Option<Arc<crate::navigation::SessionIdentity>>,
    kinds: Option<Arc<nodecraft_nodes::Registry>>,
    revision: u64,
    entries: BTreeMap<NodeId, Option<Vec<Port>>>,
    bytes: usize,
    exhausted: bool,
}
const _: () = assert!(std::mem::size_of::<Cache>() <= 256);
impl Cache {
    pub fn prepare(&mut self, session: &mut Session) {
        let identity = crate::navigation::Geometry::identity(session);
        let kinds = session.kinds_shared();
        if self.identity.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &identity))
            || self.kinds.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &kinds))
            || self.revision != session.revision()
        {
            *self = Self { identity: Some(identity), kinds: Some(kinds), revision: session.revision(), ..Self::default() };
        }
    }
    pub fn resolve<'a>(&'a mut self, node: &'a Node, kinds: &nodecraft_nodes::Registry) -> &'a [Port] {
        if !self.exhausted && !self.entries.contains_key(&node.id) {
            let inputs = kinds.get(&node.kind).ok().and_then(|kind| extension(kind, node));
            let cost = inputs.as_ref().map_or(128, |ports| {
                ports.iter().fold(128usize.saturating_add(ports.capacity().saturating_mul(std::mem::size_of::<Port>())), |sum, port| {
                    sum.saturating_add(port.name.capacity())
                })
            });
            if self.entries.len() < MAX_ENTRIES && self.bytes.checked_add(cost).is_some_and(|sum| sum <= MAX_BYTES - 256) {
                self.bytes += cost;
                self.entries.insert(node.id, inputs);
            } else {
                self.exhausted = true;
            }
        }
        self.get(node)
    }
    pub fn get<'a>(&'a self, node: &'a Node) -> &'a [Port] {
        self.entries.get(&node.id).and_then(|ports| ports.as_deref()).unwrap_or(&node.inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_nodes::{ModeId, NodeDescriptor, NodeMigration};
    use nodecraft_types::Value;
    use std::{
        borrow::Cow,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Kind {
        descriptor: NodeDescriptor,
        warning: bool,
        change_title: bool,
        calls: Arc<AtomicUsize>,
    }
    impl Kind {
        fn new() -> Self {
            let kinds = nodecraft_nodes::Registry::builtins().unwrap();
            let mut descriptor = kinds.get("source.text").unwrap().descriptor().clone();
            descriptor.id = "test.extension".into();
            descriptor.inputs = (0..2).map(|i| Port { name: format!("in{i}"), port_type: descriptor.outputs.first().unwrap().port_type }).collect();
            Self { descriptor, warning: false, change_title: false, calls: Arc::new(AtomicUsize::new(0)) }
        }
        fn legacy(&self) -> Node {
            let mut node = self.instantiate_instance(NodeId::from_bytes([91; 16]), Default::default(), &Default::default()).unwrap();
            node.inputs.truncate(1);
            node.title = "Keep this title".into();
            node.config.insert("unknown.setting".into(), Value::Text("keep".into()));
            node
        }
    }
    impl NodeKind for Kind {
        fn descriptor(&self) -> &NodeDescriptor {
            &self.descriptor
        }
        fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
            let mut descriptor = self.descriptor.clone();
            if !node.inputs.is_empty() {
                descriptor.inputs.truncate(node.inputs.len());
            }
            Ok(Cow::Owned(descriptor))
        }
        fn migrate(&self, node: &Node) -> Result<Option<NodeMigration>, Error> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if node.inputs == self.descriptor.inputs {
                return Ok(None);
            }
            let mut node = node.clone();
            node.inputs = self.descriptor.inputs.clone();
            if self.change_title {
                node.title = "Changed".into();
            }
            Ok(Some(NodeMigration { node, warnings: if self.warning { vec!["Needs a decision".into()] } else { Vec::new() } }))
        }
        fn mode(&self, _: &[PortType]) -> Option<ModeId> {
            None
        }
        fn plan(&self, _: &nodecraft_nodes::PlanCtx<'_>) -> Result<Vec<nodecraft_nodes::JobSpec>, Error> {
            Ok(Vec::new())
        }
    }
    fn session(kind: Kind) -> Session {
        let node = kind.legacy();
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(kind)).unwrap();
        let canvas = CanvasId::from_bytes([92; 16]);
        let mut project = Project::new(ProjectId::from_bytes([93; 16]), "Input preview").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
        Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap()
    }
    fn node(s: &Session) -> &Node {
        s.document().canvases().values().next().unwrap().nodes().values().next().unwrap()
    }

    #[test]
    fn any_kind_can_preview_only_a_warning_free_addition_of_inputs() {
        let mut kind = Kind::new();
        let before = kind.legacy();
        assert_eq!(extension(&kind, &before).unwrap(), kind.descriptor.inputs);
        assert_eq!(before.title, "Keep this title");
        kind.warning = true;
        assert!(extension(&kind, &before).is_none());
        kind.warning = false;
        kind.change_title = true;
        assert!(extension(&kind, &before).is_none(), "a wire must not accept unrelated changes");
        kind.change_title = false;
        kind.descriptor.inputs.reverse();
        assert!(extension(&kind, &before).is_none(), "existing ports must keep order and types");
    }

    #[test]
    fn preview_cache_is_bounded_and_invalidates_for_a_replacement_session() {
        let kind = Kind::new();
        let calls = Arc::clone(&kind.calls);
        let mut s = session(kind);
        let mut cache = Cache::default();
        cache.prepare(&mut s);
        assert_eq!(cache.resolve(node(&s), s.kinds()).len(), 2);
        assert_eq!(cache.resolve(node(&s), s.kinds()).len(), 2);
        assert_eq!(calls.load(Ordering::Relaxed), 1, "hovering does not repeat migration or clone history");
        let mut kind = Kind::new();
        kind.warning = true;
        let mut replacement = session(kind);
        assert_eq!(replacement.revision(), s.revision());
        cache.prepare(&mut replacement);
        assert_eq!(cache.resolve(node(&replacement), replacement.kinds()).len(), 1, "same node id/revision in another session cannot reuse ports");
        cache.prepare(&mut s);
        cache.bytes = MAX_BYTES;
        assert_eq!(cache.resolve(node(&s), s.kinds()).len(), 1, "a full cache falls back to the ports it can draw and hit consistently");
        assert!(cache.entries.is_empty());
    }
}
