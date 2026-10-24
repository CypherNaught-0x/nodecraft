//! Reuse ready adapters and their endpoint authority between headless estimate requests.
use super::*;
use nodecraft_engine::provider_setup::ProviderSetup;

#[derive(Default)]
pub struct Routing {
    setup: ProviderSetup,
    routed: Option<BTreeMap<NodeId, String>>,
    /// Which ComfyUI server each card goes to, and how many jobs it takes at once (#864).
    settings: ProviderSettings,
}
impl Routing {
    pub fn new(settings: ProviderSettings) -> Self {
        Self { settings, ..Self::default() }
    }

    pub fn prepare(&mut self, session: &mut Session, canvas: CanvasId, targets: &[NodeId], maker: &mut Maker<'_>, upstream: bool) -> Result<(), Error> {
        let graph = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such canvas"))?;
        let mut reached = if upstream {
            let stale = nodecraft_exec::staleness::stale(graph, session.kinds())?;
            with_upstream(graph, targets, &stale)
        } else {
            targets.iter().copied().collect()
        };
        let (selected, inner) = adapters_of(session, graph, &self.settings)?;
        reached.extend(inner.iter().filter(|(_, owner)| reached.contains(owner)).map(|(id, _)| *id).collect::<Vec<_>>());
        let needed: BTreeSet<&str> = reached.iter().filter_map(|node| selected.get(node).map(String::as_str)).collect();
        // An empty/local-only estimate must not replace endpoint authority or earlier prices.
        if needed.is_empty() {
            return Ok(());
        }
        let mut unready = Vec::new();
        for adapter in needed {
            // A key may have been added after a previous refusal. Ready adapters stay captured.
            if self.setup.readiness(adapter).is_some_and(|state| state != &Readiness::Ready) {
                self.setup.forget(adapter);
            }
            if let Some(message) = self.setup.ensure(adapter, &mut *maker).message(adapter) {
                unready.push(message);
            }
        }
        if !unready.is_empty() {
            return Err(Error::new(ErrorKind::Unsupported, unready.join(" ")));
        }
        // Include all cards using already-built adapters, even on another canvas. Quoting a
        // different card on the same endpoint keeps the nonce and every earlier unexpired price.
        let mut ready = BTreeMap::new();
        for graph in session.document().canvases().values().filter(|graph| graph.kind().is_canvas()) {
            let (adapters, _) = adapters_of(session, graph, &self.settings)?;
            ready.extend(adapters.into_iter().filter(|(_, adapter)| self.setup.readiness(adapter) == Some(&Readiness::Ready)));
        }
        if self.routed.as_ref() == Some(&ready) {
            return Ok(());
        }
        session.set_provider_routing(self.setup.routing_for(ready.iter().map(|(node, adapter)| (*node, adapter.as_str())), &self.settings)?)?;
        self.routed = Some(ready);
        Ok(())
    }
}
