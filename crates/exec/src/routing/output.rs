//! Output shape for connection/model choosers. Reuse the planner's bounded router resolution;
//! a Router's deliberately scalar stored port cannot certify its resolved source collection.
use super::*;
use nodecraft_graph::{Endpoint, Node, Port};
use nodecraft_types::PortType;

/// One terminal output carried through a Router, borrowing metadata, never result payloads.
pub struct OutputSource<'a> {
    pub node: &'a Node,
    pub port: &'a Port,
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn stored<'a>(canvas: &'a Canvas, id: NodeId, name: &str) -> Result<OutputSource<'a>, Error> {
    let node = canvas.nodes().get(&id).ok_or_else(|| invalid("the connected source is missing"))?;
    let port = node.outputs.iter().find(|port| port.name == name).ok_or_else(|| invalid("the connected output is missing"))?;
    Ok(OutputSource { node, port })
}

/// The ordered terminal sources of an output. Direct outputs retain their existing shape.
/// A Router with no source, an invalid branch, or unresolved cycle refuses a chooser instead
/// of treating absent provenance as a single value. Returned entries borrow source metadata;
/// dynamic descriptor validation is performed once per terminal node per resolver.
pub fn output_sources<'a>(canvas: &'a Canvas, kinds: &Registry, from: &Endpoint) -> Result<Vec<OutputSource<'a>>, Error> {
    OutputResolver::new(canvas, kinds).sources(from)
}

/// Ephemeral metadata resolver: prepare the bounded Router index at most once for a chooser.
/// It borrows the live canvas and is never retained by the UI across document revisions.
pub struct OutputResolver<'a, 'k> {
    canvas: &'a Canvas,
    kinds: &'k Registry,
    routes: Option<Routes<'a>>,
    validated: BTreeSet<NodeId>,
}
impl<'a, 'k> OutputResolver<'a, 'k> {
    pub fn new(canvas: &'a Canvas, kinds: &'k Registry) -> Self {
        Self { canvas, kinds, routes: None, validated: BTreeSet::new() }
    }
    pub fn sources(&mut self, from: &Endpoint) -> Result<Vec<OutputSource<'a>>, Error> {
        let canvas = self.canvas;
        let kinds = self.kinds;
        let output = stored(canvas, from.node, &from.port)?;
        if output.node.kind != router::ID {
            return Ok(vec![output]);
        }
        if self.routes.is_none() {
            self.routes = Some(routes(canvas, kinds)?);
        }
        let routes = self.routes.as_ref().ok_or_else(|| invalid("Router output metadata is unavailable"))?;
        // Inspect every reachable Router, including empty branches hidden beside valid feeds.
        // Each Router and incident edge is visited once; the shared resolver caps retained feeds.
        let mut pending = vec![from.node];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if routes.failures.contains_key(&id) {
                return Err(invalid("the connected Router has an invalid configuration or source branch"));
            }
            if routes.memo.get(&id).is_none_or(Vec::is_empty) {
                return Err(invalid("the connected Router has no resolved source; connect its input and remove any Router cycle first"));
            }
            pending.extend(routes.raw.get(&id).into_iter().flatten().filter(|feed| routes.routers.contains(&feed.2)).map(|feed| feed.2));
        }
        let sources = routes.memo.get(&from.node).ok_or_else(|| invalid("the connected Router could not be resolved"))?;
        if sources.len() >= MAX_FEEDS_PER_PORT {
            return Err(invalid("the connected Router exceeds the supported source count"));
        }
        let mut resolved = Vec::new();
        resolved.try_reserve_exact(sources.len()).map_err(|_| invalid("Router output metadata could not be reserved"))?;
        for (id, port) in sources {
            let source = stored(canvas, *id, port)?;
            // Duplicate feeds remain distinct, but dynamic kinds can rebuild substantial
            // descriptors. Cache only a successful check under this immutable canvas borrow.
            if !self.validated.contains(id) {
                let descriptor = kinds.get(&source.node.kind)?.resolve_instance(source.node)?;
                if source.node.inputs != descriptor.inputs || source.node.outputs != descriptor.outputs {
                    return Err(invalid("a Router source has stale or invalid ports"));
                }
                self.validated.insert(*id);
            }
            if source.port.port_type.modality != output.port.port_type.modality {
                return Err(invalid("a Router source has an incompatible modality"));
            }
            resolved.push(source);
        }
        Ok(resolved)
    }
    pub fn output_type(&mut self, from: &Endpoint) -> Result<PortType, Error> {
        let sources = self.sources(from)?;
        shape(&sources)
    }
}

fn shape(sources: &[OutputSource<'_>]) -> Result<PortType, Error> {
    let source = sources.first().ok_or_else(|| invalid("the output has no resolved source"))?;
    Ok(PortType { modality: source.port.port_type.modality, many: sources.len() > 1 || sources.iter().any(|source| source.port.port_type.many) })
}

/// The connection shape of the resolved sources. Several scalar feeds are a collection;
/// one list stays a list even when empty or not run yet. This is metadata, not a paid input quote.
pub fn output_type(canvas: &Canvas, kinds: &Registry, from: &Endpoint) -> Result<PortType, Error> {
    OutputResolver::new(canvas, kinds).output_type(from)
}

#[cfg(test)]
mod tests;
