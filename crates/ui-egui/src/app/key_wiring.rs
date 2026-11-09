//! Connecting cards from the keyboard (#1109). Enter or Space on a focused port starts a pending
//! connection, and focus moves to the first port on the other side that would accept it; Tab or
//! the arrow keys move between those ports (the rules a dragged noodle follows, from
//! [`wire_rules`]); Enter or Space connects through the command the same drop would run, one undo
//! step; Escape cancels and is consumed, so it clears nothing else. Shift+Delete on a focused port
//! removes its connection through `edge.disconnect`. Like a drag, the flow reaches the cards drawn
//! on screen.
use super::*;

/// A port handle drawn on a card.
#[derive(Clone, Debug)]
pub(super) struct PortWidget {
    pub(super) id: egui::Id,
    pub(super) side: Side,
    pub(super) at: Endpoint,
}

/// The port handles drawn this frame, and last frame's, which keys are matched against before
/// this frame's cards are drawn.
#[derive(Default)]
pub(super) struct PortWidgets {
    current: Vec<PortWidget>,
    seen: Vec<PortWidget>,
}

impl PortWidgets {
    fn begin(&mut self) {
        self.seen = std::mem::take(&mut self.current);
    }

    pub(super) fn push(&mut self, id: egui::Id, side: Side, at: Endpoint) {
        self.current.push(PortWidget { id, side, at });
    }

    fn by_id(&self, id: egui::Id) -> Option<&PortWidget> {
        self.seen.iter().find(|port| port.id == id)
    }

    pub(super) fn id_of(&self, side: Side, at: &Endpoint) -> Option<egui::Id> {
        self.seen.iter().chain(&self.current).find(|port| port.side == side && port.at == *at).map(|port| port.id)
    }
}

/// A connection being chosen from the keyboard.
pub(super) struct KeyWiring {
    wiring: Wiring,
    /// The port the flow started on; focus returns there when it ends.
    origin: (Side, Endpoint),
    /// The port it would land on, which holds keyboard focus.
    target: Option<Endpoint>,
}

/// What this frame's keys ask of the flow.
enum Step {
    Cancel { refocus: bool },
    Move(bool),
    Commit,
    Start(PortWidget),
    Disconnect(PortWidget),
}

/// Keys handled while a port has focus: they never move egui's focus or reach other handlers.
const LOCK: egui::EventFilter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: true };

/// Consume presses of `key` whose modifiers satisfy `modifiers`, reporting whether there were any.
fn take(input: &mut egui::InputState, key: egui::Key, modifiers: impl Fn(egui::Modifiers) -> bool) -> bool {
    let mut found = false;
    input.events.retain(|event| {
        let hit = matches!(event, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && modifiers(*m));
        found |= hit;
        !hit
    });
    found
}

fn bare(m: egui::Modifiers) -> bool {
    !m.any()
}

fn shift(m: egui::Modifiers) -> bool {
    m.shift_only()
}

impl NodeCraftApp {
    /// The ports on screen a keyboard connection may land on, in reading order of their cards
    /// (left to right, then top to bottom) and port order within a card, with their anchors.
    fn key_candidates(&self, canvas: &Canvas, wiring: &Wiring) -> Vec<(Endpoint, Pos2)> {
        let mut cards: Vec<_> = drop_targets(canvas, &self.painted).collect();
        cards.sort_by(|(a, ra), (b, rb)| ra.min.x.total_cmp(&rb.min.x).then(ra.min.y.total_cmp(&rb.min.y)).then(a.id.cmp(&b.id)));
        let mut found = Vec::new();
        match wiring.side {
            Side::Output => {
                let rules = wire_rules::IntoInputs::new(canvas, wiring);
                for (node, rect) in cards {
                    let inputs = self.input_ports.get(node);
                    for (index, input) in inputs.iter().enumerate() {
                        if rules.accepts(node, input).is_ok() {
                            found.push((Endpoint { node: node.id, port: input.name.clone() }, noodle::anchor(rect, Side::Input, index, inputs.len())));
                        }
                    }
                }
            }
            Side::Input => {
                let Some(input) =
                    canvas.nodes().get(&wiring.anchor.node).and_then(|target| self.input_ports.get(target).iter().find(|p| p.name == wiring.anchor.port))
                else {
                    return found;
                };
                let mut rules = wire_rules::FromOutputs::new(canvas, self.session.kinds(), wiring, input.port_type);
                for (node, rect) in cards {
                    for (index, output) in node.outputs.iter().enumerate() {
                        if rules.accepts(node, output).is_ok() {
                            found.push((Endpoint { node: node.id, port: output.name.clone() }, noodle::anchor(rect, Side::Output, index, node.outputs.len())));
                        }
                    }
                }
            }
        }
        found
    }

    /// The side the pending connection's candidates are on.
    fn candidate_side(wiring: &Wiring) -> Side {
        match wiring.side {
            Side::Output => Side::Input,
            Side::Input => Side::Output,
        }
    }

    fn focus_port(&self, ctx: &egui::Context, side: Side, at: &Endpoint) {
        if let Some(id) = self.port_widgets.id_of(side, at) {
            ctx.memory_mut(|m| m.request_focus(id));
        }
    }

    /// Read this frame's keys for the keyboard connect flow, before anything else reads them.
    /// `live` is false while a text field, menu, palette, dialog or gesture owns the keyboard: a
    /// pending connection then ends without changing anything.
    pub(super) fn key_wiring_keys(&mut self, ctx: &egui::Context, live: bool) {
        self.port_widgets.begin();
        let was_pending = self.key_wiring.is_some();
        let focused = ctx.memory(|m| m.focused());
        let step = if self.key_wiring.is_some() {
            let stale = self.session.document().canvases().get(&self.canvas).is_none_or(|canvas| {
                self.key_wiring
                    .as_ref()
                    .is_some_and(|k| !canvas.nodes().contains_key(&k.wiring.anchor.node) || k.wiring.moving.is_some_and(|id| !canvas.edges().contains_key(&id)))
            });
            let elsewhere = focused.is_some_and(|id| self.port_widgets.by_id(id).is_none());
            if !live || stale || elsewhere || self.wiring.is_some() || ctx.input(|i| i.pointer.any_pressed()) {
                Some(Step::Cancel { refocus: false })
            } else {
                ctx.input_mut(|i| {
                    if take(i, egui::Key::Escape, bare) {
                        Some(Step::Cancel { refocus: true })
                    } else if take(i, egui::Key::Enter, bare) | take(i, egui::Key::Space, bare) {
                        Some(Step::Commit)
                    } else {
                        let next = take(i, egui::Key::Tab, bare) | take(i, egui::Key::ArrowDown, bare) | take(i, egui::Key::ArrowRight, bare);
                        let previous = take(i, egui::Key::Tab, shift) | take(i, egui::Key::ArrowUp, bare) | take(i, egui::Key::ArrowLeft, bare);
                        match (next, previous) {
                            (true, false) => Some(Step::Move(true)),
                            (false, true) => Some(Step::Move(false)),
                            _ => None,
                        }
                    }
                })
            }
        } else if let Some(port) = focused.filter(|_| live && self.wiring.is_none()).and_then(|id| self.port_widgets.by_id(id)).cloned() {
            ctx.input_mut(|i| {
                if take(i, egui::Key::Enter, bare) | take(i, egui::Key::Space, bare) {
                    Some(Step::Start(port))
                } else if take(i, egui::Key::Delete, shift) | take(i, egui::Key::Backspace, shift) {
                    Some(Step::Disconnect(port))
                } else {
                    None
                }
            })
        } else {
            None
        };
        let handled = step.is_some();
        match step {
            None => {}
            Some(Step::Cancel { refocus }) => {
                if let Some(pending) = self.key_wiring.take()
                    && refocus
                {
                    self.focus_port(ctx, pending.origin.0, &pending.origin.1);
                }
                self.notices.clear();
            }
            Some(Step::Move(forward)) => self.key_wiring_move(ctx, forward),
            Some(Step::Commit) => self.key_wiring_commit(ctx),
            Some(Step::Start(port)) => self.key_wiring_start(ctx, port),
            Some(Step::Disconnect(port)) => self.key_disconnect(&port),
        }
        let pending = self.key_wiring.is_some();
        if pending || handled {
            // The flow owns Tab, the arrows and Escape: egui must not also move or drop focus.
            ctx.memory_mut(|m| {
                m.move_focus(egui::FocusDirection::None);
                if pending && let Some(id) = m.focused() {
                    m.set_focus_lock_filter(id, LOCK);
                }
            });
        }
        if was_pending && !pending {
            // However the flow ended, the port left holding focus gets its ordinary keys back:
            // egui keeps a lock filter until focus moves, which would trap Tab, arrows and Escape.
            // Only a port: a widget that took focus meanwhile keeps any lock filter of its own.
            if let Some(id) = ctx.memory(|m| m.focused()).filter(|id| self.port_widgets.by_id(*id).is_some()) {
                ctx.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter::default()));
            }
        }
    }

    fn key_wiring_start(&mut self, ctx: &egui::Context, port: PortWidget) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let Some(node) = canvas.nodes().get(&port.at.node) else { return };
        let wiring = match wire_rules::begin(canvas, self.session.kinds(), &self.input_ports, node, port.side, &port.at.port) {
            Ok(Some(wiring)) => wiring,
            Ok(None) => return,
            Err(message) => {
                self.notices.error(message);
                return;
            }
        };
        let candidates = self.key_candidates(canvas, &wiring);
        // A picked-up noodle starts where it lands now, so Enter straight away changes nothing.
        let current = wiring.moving.and_then(|id| canvas.edges().get(&id)).map(|edge| edge.to.clone());
        let target = current.filter(|to| candidates.iter().any(|(at, _)| at == to)).or_else(|| candidates.first().map(|(at, _)| at.clone()));
        let Some(target) = target else {
            self.notices.warning("No port on screen can connect to this one.");
            return;
        };
        let side = Self::candidate_side(&wiring);
        self.focus_port(ctx, side, &target);
        self.notices.info("Connecting: Tab or the arrow keys choose a port, Enter connects, Escape cancels.");
        self.key_wiring = Some(KeyWiring { wiring, origin: (port.side, port.at), target: Some(target) });
    }

    fn key_wiring_move(&mut self, ctx: &egui::Context, forward: bool) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let Some(pending) = &self.key_wiring else { return };
        let candidates = self.key_candidates(canvas, &pending.wiring);
        let count = candidates.len();
        if count == 0 {
            return;
        }
        let at = pending.target.as_ref().and_then(|target| candidates.iter().position(|(at, _)| at == target));
        let index = match (at, forward) {
            (Some(at), true) => at.saturating_add(1) % count,
            (Some(at), false) => at.checked_sub(1).unwrap_or(count.saturating_sub(1)),
            (None, true) => 0,
            (None, false) => count.saturating_sub(1),
        };
        let Some((target, _)) = candidates.into_iter().nth(index) else { return };
        let side = Self::candidate_side(&pending.wiring);
        self.focus_port(ctx, side, &target);
        if let Some(pending) = &mut self.key_wiring {
            pending.target = Some(target);
        }
    }

    fn key_wiring_commit(&mut self, ctx: &egui::Context) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let Some(pending) = &self.key_wiring else { return };
        let Some(target) = &pending.target else { return };
        let Some(node) = canvas.nodes().get(&target.node) else { return };
        let wiring = &pending.wiring;
        let command = match wiring.side {
            Side::Output => match self.input_ports.get(node).iter().find(|p| p.name == target.port) {
                Some(input) => {
                    let rules = wire_rules::IntoInputs::new(canvas, wiring);
                    rules.accepts(node, input).and_then(|()| self.connect_into(canvas, wiring, node, input))
                }
                None => Err("That port is no longer on the card.".into()),
            },
            Side::Input => {
                let input = canvas.nodes().get(&wiring.anchor.node).and_then(|card| self.input_ports.get(card).iter().find(|p| p.name == wiring.anchor.port));
                match (input, node.outputs.iter().find(|p| p.name == target.port)) {
                    (Some(input), Some(output)) => {
                        let mut rules = wire_rules::FromOutputs::new(canvas, self.session.kinds(), wiring, input.port_type);
                        rules.accepts(node, output).and_then(|()| self.connect_from(canvas, wiring, &rules, node, output))
                    }
                    _ => Err("That port is no longer on the card.".into()),
                }
            }
        };
        let result = match command {
            Err(message) => {
                // Refused: the flow stays open so another port can be chosen.
                self.notices.error(message);
                return;
            }
            Ok(None) => Ok(()),
            Ok(Some((command, params))) => self.session.dispatch(command, &params).map(|_| ()),
        };
        match result {
            Ok(()) => {
                self.notices.clear();
                if let Some(pending) = self.key_wiring.take() {
                    self.focus_port(ctx, pending.origin.0, &pending.origin.1);
                }
            }
            Err(error) => self.notices.report(error),
        }
    }

    /// Shift+Delete on a focused port: an input drops its last connection, an output its only
    /// one. An output feeding several inputs says to choose one from its input instead.
    fn key_disconnect(&mut self, port: &PortWidget) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let edges: Vec<EdgeId> = match port.side {
            Side::Input => noodle::into_port(canvas, &port.at).last().map(|edge| edge.id).into_iter().collect(),
            Side::Output => canvas.edges().values().filter(|edge| edge.from == port.at).map(|edge| edge.id).collect(),
        };
        let id = match edges.as_slice() {
            [] => {
                self.notices.warning("This port has no connection to remove.");
                return;
            }
            [id] => *id,
            more => {
                self.notices.warning(format!("This output has {} connections; focus the input to remove one.", more.len()));
                return;
            }
        };
        match self.session.dispatch("edge.disconnect", &json!({"canvas": self.canvas, "id": id})) {
            Ok(_) => self.notices.clear(),
            Err(error) => self.notices.report(error),
        }
    }

    /// Paint the pending keyboard connection: a ring on every port it may land on, and its noodle
    /// to the one holding focus. Called after this frame's cards are drawn.
    pub(super) fn key_wiring_paint(&self, ui: &egui::Ui, canvas: &Canvas, t: &Tokens) {
        let Some(pending) = &self.key_wiring else { return };
        let wiring = &pending.wiring;
        let kinds = self.session.kinds();
        let origin = self.viewport.min;
        let Some(anchor) = canvas.nodes().get(&wiring.anchor.node).and_then(|node| {
            let rect = card_geometry(&self.painted, kinds, node, t);
            match wiring.side {
                Side::Output => noodle::port_anchor(node, rect, Side::Output, &wiring.anchor.port),
                Side::Input => {
                    let inputs = self.input_ports.get(node);
                    inputs.iter().position(|p| p.name == wiring.anchor.port).map(|index| noodle::anchor(rect, Side::Input, index, inputs.len()))
                }
            }
        }) else {
            return;
        };
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("key_wiring"))).with_clip_rect(self.viewport);
        let radius = match wiring.side {
            Side::Output => t.port_radius,
            Side::Input => t.handle_radius,
        };
        let mut end = None;
        for (at, point) in self.key_candidates(canvas, wiring) {
            let ring = radius * self.view.zoom + t.drop_ring_gap;
            painter.circle_stroke(self.view.to_screen(origin, point), ring, egui::Stroke::new(t.drop_ring_width, t.wire(wiring.port_type.modality)));
            if pending.target.as_ref() == Some(&at) {
                end = Some(point);
            }
        }
        let Some(end) = end else { return };
        let (from, to) = match wiring.side {
            Side::Output => (anchor, end),
            Side::Input => (end, anchor),
        };
        let points = noodle::bezier(from, to).map(|p| self.view.to_screen(origin, p));
        let stroke = egui::Stroke::new(t.wire_width, t.wire(wiring.port_type.modality));
        painter.add(egui::epaint::CubicBezierShape::from_points_stroke(points, false, egui::Color32::TRANSPARENT, stroke));
    }

    /// The edge a pending keyboard connection is rerouting, which is drawn as the pending noodle.
    pub(super) fn key_wiring_moving(&self) -> Option<EdgeId> {
        self.key_wiring.as_ref().and_then(|pending| pending.wiring.moving)
    }

    /// Every connection as an AccessKit node (#1109), "Image from A to B · reference", over its
    /// noodle and marked when selected, so a screen reader can list and announce them.
    pub(super) fn edge_accessibility(&self, ctx: &egui::Context, canvas: &Canvas, curves: &[(EdgeId, [Pos2; 4], Modality)], origin: Pos2) {
        let peers = std::cell::OnceCell::new();
        let links = card::PortLinks { canvas, kinds: self.session.kinds(), peers: &peers };
        for (id, curve, modality) in curves {
            let Some(edge) = canvas.edges().get(id) else { continue };
            let bounds = self.view.rect_to_screen(origin, Rect::from_points(curve));
            ctx.accesskit_node_builder(egui::Id::new(("edge", *id)), |node| {
                let from = canvas.nodes().get(&edge.from.node).map(|card| links.title(card)).unwrap_or_default();
                let target = canvas.nodes().get(&edge.to.node);
                let to = target.map(|card| links.title(card)).unwrap_or_default();
                let input = target
                    .and_then(|card| self.input_ports.get(card).iter().find(|port| port.name == edge.to.port))
                    .cloned()
                    .unwrap_or_else(|| nodecraft_graph::Port { name: edge.to.port.clone(), port_type: PortType { modality: *modality, many: false } });
                node.set_role(egui::accesskit::Role::Label);
                // As egui names its own labels: a Label role reads its value.
                node.set_value(noodle::edge_label(*modality, &from, &to, &input));
                node.set_selected(self.selection.contains_edge(*id));
                if [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y].iter().all(|v| v.is_finite()) {
                    node.set_bounds(egui::accesskit::Rect {
                        x0: bounds.min.x.into(),
                        y0: bounds.min.y.into(),
                        x1: bounds.max.x.into(),
                        y1: bounds.max.y.into(),
                    });
                }
            });
        }
    }
}
