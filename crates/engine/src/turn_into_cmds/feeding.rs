//! Create a compatible upstream node for an existing input, using the same atomic command.
use super::*;

/// A default node with an output that can feed `input`, and that output's name.
pub fn instance_feeding(kinds: &Registry, kind: &str, id: NodeId, input: PortType) -> Result<(Node, String), Error> {
    let node = kinds.get(kind)?.instantiate_instance(id, config_for(kind, input), &ParamValues::new())?;
    let output = node.outputs.iter().find(|output| compatible(output.port_type, input)).map(|output| output.name.clone());
    match output {
        Some(output) => Ok((node, output)),
        None => Err(Error::new(ErrorKind::Conflict, "that kind of node has no output for this input")),
    }
}

/// Upstream kinds in registry order. Resolve each candidate once when a chooser opens, so
/// configurable router/batch ports agree with the node the eventual command creates.
pub fn kinds_feeding(kinds: &Registry, input: PortType) -> Vec<&NodeDescriptor> {
    let probe = NodeId::from_bytes([0; 16]);
    kinds.descriptors().filter(|descriptor| instance_feeding(kinds, &descriptor.id, probe, input).is_ok()).collect()
}

pub(super) fn create_connected(c: &Context<'_>, p: CreateConnectedParams) -> Result<Effect, Error> {
    let to = p.to.ok_or_else(|| invalid("an existing input is required"))?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let upgraded = crate::graph_cmds::upgraded(c, p.canvas, to.node, &to.port);
    let target = upgraded.as_ref().or_else(|| canvas.nodes().get(&to.node)).ok_or_else(|| Error::new(ErrorKind::NotFound, "the target node does not exist"))?;
    let input = target.inputs.iter().find(|port| port.name == to.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "the target node has no such input"))?;
    let mut occupants = canvas.edges().values().filter(|edge| edge.to == to);
    let order = if input.port_type.many {
        match occupants.map(|edge| edge.order).max() {
            Some(order) => order.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "this input can't take more connections"))?,
            None => 0,
        }
    } else {
        if occupants.next().is_some() {
            return Err(Error::new(ErrorKind::Conflict, "the input is already connected; disconnect it before adding an upstream node"));
        }
        0
    };
    let (mut node, output) = instance_feeding(c.kinds, &p.kind, p.id, input.port_type)?;
    node.pos = p.pos;
    let edge = Edge { id: p.edge, from: Endpoint { node: p.id, port: output.clone() }, to, order };
    let mut changes = Vec::new();
    if let Some(node) = upgraded {
        changes.push(Change::SetNode { canvas: p.canvas, node });
    }
    changes.push(Change::AddNode { canvas: p.canvas, node });
    changes.push(Change::Connect { canvas: p.canvas, edge });
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id, "output": output}), gesture: None })
}
