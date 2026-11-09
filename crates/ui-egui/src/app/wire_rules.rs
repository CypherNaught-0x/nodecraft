//! What a new or moved noodle may connect to, and the command that connection becomes. Pointer
//! drags and the keyboard connect flow (#1109) share these, so both refuse the same ports with
//! the same reasons and land the same single undo step.
use super::*;

/// Where a wire starts when its port is grabbed or chosen from the keyboard: an output starts a
/// new wire, a connected input picks up its last noodle to reroute it, and an unconnected input
/// starts a wire looking for an upstream output. `Err` says why an output can't start one.
pub(super) fn begin(
    canvas: &Canvas,
    kinds: &nodecraft_nodes::Registry,
    inputs: &input_ports::Cache,
    node: &Node,
    side: Side,
    port: &str,
) -> Result<Option<Wiring>, String> {
    let mut proposed = match side {
        Side::Output => node.outputs.iter().find(|p| p.name == port).map(|p| Wiring {
            anchor: Endpoint { node: node.id, port: port.to_owned() },
            side: Side::Output,
            port_type: p.port_type,
            moving: None,
        }),
        Side::Input => {
            let to = Endpoint { node: node.id, port: port.to_owned() };
            match noodle::into_port(canvas, &to).last() {
                Some(edge) => canvas.nodes().get(&edge.from.node).and_then(|source| {
                    source.outputs.iter().find(|p| p.name == edge.from.port).map(|source| Wiring {
                        anchor: edge.from.clone(),
                        side: Side::Output,
                        port_type: source.port_type,
                        moving: Some(edge.id),
                    })
                }),
                None => inputs.get(node).iter().find(|p| p.name == port).map(|input| Wiring {
                    anchor: to,
                    side: Side::Input,
                    port_type: input.port_type,
                    moving: None,
                }),
            }
        }
    };
    if let Some(wiring) = &mut proposed
        && wiring.side == Side::Output
    {
        wiring.port_type = nodecraft_engine::turn_into_cmds::output_type(canvas, kinds, &wiring.anchor).map_err(|error| error.message)?;
    }
    Ok(proposed)
}

/// The inputs an output-side wire may land on.
pub(super) struct IntoInputs<'a> {
    wiring: &'a Wiring,
    /// Nodes that already feed the source: connecting into any of them would close a loop.
    feeding: BTreeSet<NodeId>,
    /// Inputs this output already feeds (other than the noodle being moved), built once so each
    /// candidate input is a set lookup rather than a scan of every edge.
    fed: BTreeSet<(NodeId, &'a str)>,
}

impl<'a> IntoInputs<'a> {
    pub(super) fn new(canvas: &'a Canvas, wiring: &'a Wiring) -> Self {
        let feeding = selection::upstream(canvas, &[wiring.anchor.node].into_iter().collect());
        let fed = canvas.edges().values().filter(|e| e.from == wiring.anchor && Some(e.id) != wiring.moving).map(|e| (e.to.node, e.to.port.as_str())).collect();
        Self { wiring, feeding, fed }
    }

    pub(super) fn accepts(&self, node: &Node, input: &nodecraft_graph::Port) -> Result<(), String> {
        let wiring = self.wiring;
        if node.id == wiring.anchor.node {
            Err("A node can't connect to itself.".into())
        } else if !noodle::compatible(wiring.port_type, input.port_type) {
            Err(if wiring.port_type.modality == input.port_type.modality {
                "A list output needs a list input.".into()
            } else {
                let mut from = noodle::modality_phrase(wiring.port_type.modality).chars();
                let from: String = from.next().map(|c| c.to_uppercase().chain(from).collect()).unwrap_or_default();
                format!("{from} output can't connect to {} input.", noodle::modality_phrase(input.port_type.modality))
            })
        } else if self.feeding.contains(&node.id) {
            Err("That connection would create a loop.".into())
        } else if self.fed.contains(&(node.id, input.name.as_str())) {
            Err("These ports are already connected.".into())
        } else {
            Ok(())
        }
    }
}

/// The outputs an input-side wire (from an unconnected input) may come from.
pub(super) struct FromOutputs<'a, 'k> {
    target: NodeId,
    input_type: PortType,
    downstream: BTreeSet<NodeId>,
    fed: BTreeSet<(NodeId, &'a str)>,
    routing: nodecraft_exec::routing::OutputResolver<'a, 'k>,
}

impl<'a, 'k> FromOutputs<'a, 'k> {
    pub(super) fn new(canvas: &'a Canvas, kinds: &'k nodecraft_nodes::Registry, wiring: &Wiring, input_type: PortType) -> Self {
        let target = wiring.anchor.node;
        let downstream = selection::downstream(canvas, &[target].into_iter().collect());
        let fed = canvas.edges().values().filter(|edge| edge.to == wiring.anchor).map(|edge| (edge.from.node, edge.from.port.as_str())).collect();
        Self { target, input_type, downstream, fed, routing: nodecraft_exec::routing::OutputResolver::new(canvas, kinds) }
    }

    /// Whether the input already has a connection (a single input then refuses another).
    pub(super) fn occupied(&self) -> bool {
        !self.fed.is_empty()
    }

    pub(super) fn accepts(&mut self, node: &Node, output: &nodecraft_graph::Port) -> Result<(), String> {
        let from = Endpoint { node: node.id, port: output.name.clone() };
        let output_type = self.routing.output_type(&from).map_err(|error| error.message)?;
        if node.id == self.target {
            Err("A node can't connect to itself.".into())
        } else if !noodle::compatible(output_type, self.input_type) {
            Err(if output_type.modality == self.input_type.modality {
                "A list output needs a list input.".into()
            } else {
                "That output has the wrong media type for this input.".into()
            })
        } else if self.downstream.contains(&node.id) {
            Err("That connection would create a loop.".into())
        } else if self.fed.contains(&(node.id, output.name.as_str())) {
            Err("These ports are already connected.".into())
        } else {
            Ok(())
        }
    }
}

/// A command to run, if any: `Ok(None)` when the drop changes nothing.
pub(super) type WireCommand = Result<Option<(&'static str, serde_json::Value)>, String>;

impl NodeCraftApp {
    /// The command that lands an output-side `wiring` on `input` of `node`, already accepted by
    /// [`IntoInputs`]: `edge.connect`, or `edge.rewire` when it moves a noodle or replaces the one
    /// a single input holds, so one undo restores the graph.
    pub(super) fn connect_into(&self, canvas: &Canvas, wiring: &Wiring, node: &Node, input: &nodecraft_graph::Port) -> WireCommand {
        let to = Endpoint { node: node.id, port: input.name.clone() };
        let occupants: Vec<_> = noodle::into_port(canvas, &to).into_iter().filter(|e| Some(e.id) != wiring.moving).collect();
        if wiring.moving.is_some_and(|id| canvas.edges().get(&id).is_some_and(|e| e.to == to)) {
            return Ok(None);
        }
        let mut replace: Vec<EdgeId> = wiring.moving.into_iter().collect();
        let order = if input.port_type.many {
            noodle::next_order(canvas, &to, wiring.moving).ok_or_else(|| "This input can't take more connections.".to_owned())?
        } else {
            // A single input swaps its current noodle for the new one, in the same undo step.
            replace.extend(occupants.first().map(|e| e.id));
            0
        };
        let id = wiring
            .moving
            .or_else(|| noodle::new_edge_id(self.session.document(), &wiring.anchor, &to, self.session.revision()))
            .ok_or_else(|| "Couldn't allocate a connection id; try again.".to_owned())?;
        let edge = json!({"id": id, "from": wiring.anchor, "to": to, "order": order});
        Ok(Some(if replace.is_empty() {
            ("edge.connect", json!({"canvas": self.canvas, "edge": edge}))
        } else {
            ("edge.rewire", json!({"canvas": self.canvas, "edge": edge, "replace": replace}))
        }))
    }

    /// The command that connects `output` of `node` into the input an input-side `wiring` started
    /// from, already accepted by [`FromOutputs`].
    pub(super) fn connect_from(&self, canvas: &Canvas, wiring: &Wiring, sources: &FromOutputs, node: &Node, output: &nodecraft_graph::Port) -> WireCommand {
        let order = if sources.input_type.many {
            noodle::next_order(canvas, &wiring.anchor, None).ok_or_else(|| "This input can't take more connections.".to_owned())?
        } else {
            if sources.occupied() {
                return Err("The input is already connected.".into());
            }
            0
        };
        let from = Endpoint { node: node.id, port: output.name.clone() };
        let id = noodle::new_edge_id(self.session.document(), &from, &wiring.anchor, self.session.revision())
            .ok_or_else(|| "Couldn't allocate a connection id; try again.".to_owned())?;
        let edge = json!({"id":id,"from":from,"to":wiring.anchor,"order":order});
        Ok(Some(("edge.connect", json!({"canvas":self.canvas,"edge":edge}))))
    }
}
