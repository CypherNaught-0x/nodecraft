//! One bounded port-count capture reused for model picker candidates. No payload owners.
use nodecraft_graph::{Canvas, Port};
use nodecraft_nodes::{NodeContract, NodeDescriptor};
use nodecraft_types::{Error, ErrorKind, NodeId};
use std::collections::BTreeMap;
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
pub struct GenerationWiring<'a> {
    inputs: Vec<(&'a Port, usize)>,
    outputs: &'a [Port],
}
impl<'a> GenerationWiring<'a> {
    pub(super) fn capture(canvas: &'a Canvas, id: NodeId) -> Result<Self, Error> {
        let node = canvas.nodes().get(&id).ok_or_else(|| invalid("model node is missing"))?;
        if node.inputs.len() > 256 || node.outputs.len() > 256 {
            return Err(invalid("model node has too many ports"));
        }
        let mut inputs: Vec<_> = node.inputs.iter().map(|port| (port, 0usize)).collect();
        for edge in canvas.edges().values().filter(|edge| edge.to.node == id) {
            let (input, count) = inputs.iter_mut().find(|(p, _)| p.name == edge.to.port).ok_or_else(|| invalid("connected input role is missing"))?;
            let source = canvas.nodes().get(&edge.from.node).ok_or_else(|| invalid("connected source is missing"))?;
            let output = source.outputs.iter().find(|p| p.name == edge.from.port).ok_or_else(|| invalid("connected output role is missing"))?;
            if output.port_type.modality != input.port_type.modality || (!input.port_type.many && output.port_type.many) {
                return Err(invalid("connected input has incompatible modality or collection"));
            }
            *count = count.checked_add(1).ok_or_else(|| invalid("too many model input wires"))?;
        }
        Ok(Self { inputs, outputs: &node.outputs })
    }
    pub(super) fn validate(&self, descriptor: &NodeDescriptor, contract: &NodeContract, ready: bool) -> Result<(), Error> {
        // Production generation outputs have a stable image collection port. Exact equality
        // protects ALL outgoing edges at once, without rescanning/cloning arbitrary fan-out.
        if self.outputs != descriptor.outputs {
            return Err(invalid("model swap changes connected output roles or collection"));
        }
        let mut counts = BTreeMap::new();
        for (prior, count) in &self.inputs {
            if *count == 0 {
                continue;
            }
            let next = descriptor.inputs.iter().find(|p| p.name == prior.name).ok_or_else(|| invalid("model swap removes a connected input role"))?;
            if next.port_type != prior.port_type {
                return Err(invalid("model swap changes a connected input modality or collection"));
            }
            counts.insert(next.name.as_str(), *count);
        }
        contract.resolve_mode(&counts, ready)?;
        Ok(())
    }
}
