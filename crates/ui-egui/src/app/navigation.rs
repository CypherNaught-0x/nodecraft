//! Live host bridge: view capabilities exist only during the supported dispatch/query call.
use super::*;
use nodecraft_engine::navigation_cmds::NavigationContext;
use serde_json::Value as Json;
use std::collections::BTreeSet;

const FIT_CLAMP_NOTICE: &str = "Nodes are too spread out to fit at minimum zoom; the view is centered at 10%.";

impl NodeCraftApp {
    /// Whether a command uses the live canvas navigation bridge. This is routing metadata;
    /// automation must still apply its own human-only, paid and classification checks.
    pub fn is_navigation_command(id: &str) -> bool {
        matches!(id, "canvas.fit_all" | "canvas.fit_selection" | "canvas.minimap_toggle" | "canvas.pan_to")
    }
    fn navigation_contexts(&mut self, ctx: &egui::Context) -> [NavigationContext; 4] {
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        let t = self.theme.tokens();
        let viewport = self.navigation.usable(self.viewport, t);
        let padding = t.fit_padding + t.title_min_screen_px + t.handle_min_screen_radius;
        let fit_available = canvas::valid_rect(viewport) && viewport.width() > 2.0 * padding && viewport.height() > 2.0 * padding;
        let viewport_available = canvas::valid_rect(self.viewport) && self.viewport.width() > 0.0 && self.viewport.height() > 0.0;
        #[cfg(not(target_arch = "wasm32"))]
        let file_owned = self.files.owns_navigation(ctx);
        #[cfg(target_arch = "wasm32")]
        let file_owned = false;
        let blocked = file_owned
            || egui::Popup::is_any_open(ctx)
            || self.model_choice.is_some()
            || self.model_frame_owned
            || self.text_focused
            || canvas::text_input_focused(ctx)
            || self.wiring.is_some()
            || self.drag.is_some()
            || self.marquee.is_some()
            || self.comments.drag.is_some()
            || self.comments.gesture_cancelled
            || self.drop_pointer_owned();
        let dragging = ctx.dragged_id().is_some() || self.navigation.drag.is_some();
        let fit_all = self.fit_all_bounds().is_some();
        let fit_selection = self.fit_selection_bounds().is_some();
        std::array::from_fn(|index| NavigationContext {
            canvas: self.canvas,
            viewport_available: if index < 2 { fit_available } else { viewport_available },
            blocked: blocked || (dragging && !(index == 3 && self.navigation.pointer_owned)),
            fit_all,
            fit_selection,
        })
    }
    /// What Fit all frames: the cards the canvas shows and collapsed field headers, the same
    /// shapes the minimap draws (#739). None while geometry is unavailable or nothing shows.
    fn fit_all_bounds(&mut self) -> Option<Rect> {
        if self.navigation.geometry.error {
            return None;
        }
        // Headers at their least zoomed-out size: sized at the current zoom, a header seen from far
        // out is taller in world points than the fitted view draws it, so a second Fit all moves.
        let (membership, collapsed) = self.collapsed_fields(1.0);
        let t = self.theme.tokens();
        self.navigation
            .shapes(None, &membership.hidden, &collapsed)
            .into_iter()
            .map(|(_, rect)| crate::navigation::decorated(rect, t))
            .reduce(|a, b| a.union(b))
    }
    /// Frame selected visible cards and the headers standing in for selected hidden cards.
    /// Use the same membership and header geometry as the canvas and minimap, at Fit all's
    /// stable header zoom rather than the previous view's zoom. Like the plain selection bounds,
    /// an invalid unselected node does not matter, but a selected visible node without valid
    /// geometry refuses the fit rather than being left out of it.
    fn fit_selection_bounds(&mut self) -> Option<Rect> {
        let nodes = self.selection.nodes().clone();
        self.fit_nodes_bounds(&nodes)
    }
    fn fit_nodes_bounds(&mut self, nodes: &BTreeSet<NodeId>) -> Option<Rect> {
        if nodes.is_empty() {
            return None;
        }
        let membership = self.membership();
        let t = self.theme.tokens();
        let rects = &self.navigation.geometry.rects;
        let visible: Vec<Rect> = nodes.iter().filter(|id| !membership.hidden.contains(*id)).map(|id| rects.get(id).copied()).collect::<Option<_>>()?;
        let hidden = self.session.document().canvases().get(&self.canvas).into_iter().flat_map(|canvas| {
            fields::stand_ins(canvas, &membership.hidden, t, 1.0, self.field_shift()).filter(|(id, _)| nodes.contains(id)).map(|(_, rect)| rect)
        });
        visible.into_iter().chain(hidden).map(|rect| crate::navigation::decorated(rect, t)).reduce(|a, b| a.union(b))
    }
    fn navigation_context(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> NavigationContext {
        let [all, mut selected, toggle, pan] = self.navigation_contexts(ctx);
        if id == "canvas.fit_selection"
            && let Some(nodes) = nodecraft_engine::navigation_cmds::requested_nodes(params)
        {
            let wanted: BTreeSet<_> = nodes.iter().copied().collect();
            selected.fit_selection = wanted.len() == nodes.len() && self.fit_nodes_bounds(&wanted).is_some();
        }
        match id {
            "canvas.fit_all" => all,
            "canvas.fit_selection" => selected,
            "canvas.minimap_toggle" => toggle,
            _ => pan,
        }
    }

    fn navigation_enabled_with_context(&mut self, context: NavigationContext, id: &str, params: &Json) -> Result<(), Error> {
        let previous = self.session.swap_resource(Some(context));
        let result = self.session.enabled(id, params);
        self.session.swap_resource(previous);
        result
    }
    /// The supported programmatic enabled query for a live canvas. Headless Session calls have
    /// no scoped live-view capability and return Unsupported instead of using stale snapshots.
    pub fn navigation_enabled(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<(), Error> {
        if !Self::is_navigation_command(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a canvas navigation command"));
        }
        let context = self.navigation_context(ctx, id, params);
        self.navigation_enabled_with_context(context, id, params)
    }
    /// Dispatch through the same engine command/service path used by buttons and shortcuts,
    /// then apply the acknowledged intent immediately to shell view state. No dialogs/history.
    pub fn dispatch_navigation(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<Json, Error> {
        if !Self::is_navigation_command(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a canvas navigation command"));
        }
        let context = self.navigation_context(ctx, id, params);
        {
            let mut slot = self.navigation_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "navigation host is busy"))?;
            *slot = None;
        }
        let previous = self.session.swap_resource(Some(context));
        let result = self.session.dispatch(id, params);
        // Dispatch guards panics and returns errors; restoration is unconditional before `?`.
        self.session.swap_resource(previous);
        let intent = self.navigation_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "navigation host is busy"))?.take();
        result?;
        let intent = intent.ok_or_else(|| Error::new(ErrorKind::Conflict, "navigation host did not acknowledge the command"))?;
        let previous_view = self.view;
        let previous_visibility = self.navigation.visible;
        let mut clamped = false;
        match intent.get("action").and_then(Json::as_str) {
            Some("fit_all" | "fit_selection") => {
                let t = self.theme.tokens();
                let bounds = if id == "canvas.fit_selection" {
                    if let Some(nodes) = nodecraft_engine::navigation_cmds::requested_nodes(&intent) {
                        self.fit_nodes_bounds(&nodes.into_iter().collect())
                    } else {
                        self.fit_selection_bounds()
                    }
                } else {
                    self.fit_all_bounds()
                };
                let bounds = bounds.ok_or_else(|| Error::new(ErrorKind::Conflict, "node geometry is unavailable"))?;
                let viewport = self.navigation.usable(self.viewport, t);
                let padding = t.fit_padding + t.title_min_screen_px + t.handle_min_screen_radius;
                clamped = self.view.fit(viewport, bounds, padding).ok_or_else(|| Error::new(ErrorKind::Conflict, "canvas is too small to fit nodes"))?.clamped;
            }
            Some("minimap_toggle") => self.navigation.visible = !self.navigation.visible,
            Some("pan_to") => {
                let x = intent.get("x").and_then(Json::as_f64).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "pan target is unavailable"))?;
                let y = intent.get("y").and_then(Json::as_f64).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "pan target is unavailable"))?;
                if !self.view.center_on(self.viewport, x, y) {
                    return Err(Error::new(ErrorKind::InvalidInput, "pan target cannot be represented"));
                }
            }
            _ => return Err(Error::new(ErrorKind::InvalidInput, "unknown navigation intent")),
        }
        if matches!(id, "canvas.fit_all" | "canvas.fit_selection") && self.notice.as_deref().is_none_or(|notice| notice == FIT_CLAMP_NOTICE) {
            self.notice = clamped.then(|| FIT_CLAMP_NOTICE.into());
        }
        if self.view != previous_view || self.navigation.visible != previous_visibility {
            ctx.request_repaint();
        }
        Ok(json!({"zoom":self.view.zoom,"minimap_visible":self.navigation.visible,"clamped":clamped}))
    }
    pub(super) fn minimap_input_owned(&mut self, ctx: &egui::Context) -> bool {
        if self.navigation.pointer_owned || self.navigation.drag.is_some() {
            return true;
        }
        // The common case needs no geometry or capability query. A fresh press uses
        // exactly the pan admission and hit-test used by the later overlay capture.
        if !self.navigation.visible || !ctx.input(|i| i.pointer.primary_pressed()) {
            return false;
        }
        let context = self.navigation_context(ctx, "canvas.pan_to", &json!({"x":0,"y":0}));
        let pan = self.navigation_enabled_with_context(context, "canvas.pan_to", &json!({"x":0,"y":0})).is_ok() && !self.navigation.geometry.error;
        self.navigation.press_pending(ctx, self.viewport, pan, self.theme.tokens())
    }
    pub(super) fn navigation_overlay(&mut self, ui: &mut egui::Ui, press_admitted: Option<bool>) -> bool {
        // Decide ownership/availability before consuming any key. An editor, popup,
        // unavailable fit or active canvas gesture must retain the original event.
        let [all_context, selected_context, toggle_context, pan_context] = self.navigation_contexts(ui.ctx());
        let reset_blocked = toggle_context.blocked;
        let all = self.navigation_enabled_with_context(all_context, "canvas.fit_all", &json!({})).is_ok();
        let selected = self.navigation_enabled_with_context(selected_context, "canvas.fit_selection", &json!({})).is_ok();
        let toggle = self.navigation_enabled_with_context(toggle_context, "canvas.minimap_toggle", &json!({})).is_ok();
        let pan = self.navigation_enabled_with_context(pan_context, "canvas.pan_to", &json!({"x":0,"y":0})).is_ok() && !self.navigation.geometry.error;
        let specs: Vec<_> = self.session.registry().specs().filter(|spec| Self::is_navigation_command(spec.id)).copied().collect();
        for spec in specs {
            let enabled = match spec.id {
                "canvas.fit_all" => all,
                "canvas.fit_selection" => selected,
                "canvas.minimap_toggle" => toggle,
                _ => false,
            };
            if enabled
                && ui.input_mut(|input| {
                    spec.shortcut.into_iter().chain(spec.alternate_shortcuts.iter().copied()).any(|shortcut| consume_navigation_shortcut(input, shortcut))
                })
            {
                if let Err(error) = self.dispatch_navigation(ui.ctx(), spec.id, &json!({})) {
                    self.notice = Some(error.message);
                }
                break;
            }
        }
        let preview = self.drag.as_ref().and_then(|drag| {
            self.session
                .document()
                .canvases()
                .get(&self.canvas)?
                .nodes()
                .get(&drag.id)
                .map(|node| (drag.id, Vec2::new(drag.pos.x - node.pos.x, drag.pos.y - node.pos.y)))
        });
        // The minimap shows a collapsed field as its header, as the canvas does (#707).
        let (membership, collapsed) = self.collapsed_fields(self.view.zoom);
        let (blocked, actions) = self.navigation.show(
            ui,
            crate::navigation::OverlayInput {
                view: self.view,
                viewport: self.viewport,
                selection: &self.selection,
                all_enabled: all,
                selection_enabled: selected,
                toggle_enabled: toggle,
                pan_enabled: pan,
                press_admitted,
                preview,
                hidden: &membership.hidden,
                collapsed: &collapsed,
                canvas_gesture: self.wiring.is_some()
                    || self.drag.is_some()
                    || self.marquee.is_some()
                    || self.comments.drag.is_some()
                    || self.comments.gesture_cancelled
                    || (ui.ctx().dragged_id().is_some() && !self.navigation.pointer_owned),
                tokens: self.theme.tokens(),
            },
        );
        for (id, params) in actions {
            // Reset is a view-only action rather than an engine command; share admission.
            if id == "reset" && reset_blocked {
                continue;
            }
            if id == "reset" {
                let home = self.home();
                self.view.reset(self.viewport, home);
            } else if let Err(error) = self.dispatch_navigation(ui.ctx(), id, &params) {
                self.notice = Some(error.message);
            }
        }
        blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    fn app() -> NodeCraftApp {
        let base = NodeCraftApp::headless().unwrap();
        let canvas = base.canvas;
        let mut doc = base.session.document().clone();
        let original = doc.canvases().get(&canvas).unwrap().nodes().values().next().unwrap().clone();
        for (n, x, y) in [(3, -1000.0, -300.0), (4, 1200.0, 650.0)] {
            let mut node = original.clone();
            node.id = NodeId::from_bytes([n; 16]);
            node.pos = nodecraft_graph::Position { x, y };
            let change = if n == 3 { Change::SetNode { canvas, node } } else { Change::AddNode { canvas, node } };
            change.apply(&mut doc).unwrap();
        }
        NodeCraftApp::from_session(Session::new(doc).unwrap(), canvas).unwrap()
    }
    fn harness() -> Harness<'static, NodeCraftApp> {
        Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app())
    }
    fn button(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, down: bool) {
        h.hover_at(pos);
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: down, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    fn native_key(logical: egui::Key, physical: Option<egui::Key>, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key { key: logical, physical_key: physical, pressed: true, repeat: false, modifiers }
    }
    #[test]
    fn generic_shortcuts_keep_logical_matching_while_navigation_is_exact() {
        for (shortcut, key) in [("Ctrl+M", egui::Key::M), ("Ctrl+1", egui::Key::Num1), ("Ctrl+2", egui::Key::Num2)] {
            let ctx = egui::Context::default();
            let raw = egui::RawInput { events: vec![native_key(key, Some(key), egui::Modifiers::CTRL | egui::Modifiers::SHIFT)], ..Default::default() };
            let mut output = ctx.run_ui(raw, |ui| {
                ui.input_mut(|input| {
                    assert!(!consume_navigation_shortcut(input, shortcut), "navigation rejects extra Shift");
                    assert!(consume_shortcut(input, shortcut), "ordinary shortcuts retain egui logical modifier matching");
                })
            });
            output.textures_delta.clear();
        }
    }
    #[test]
    fn batched_navigation_capabilities_match_fresh_queries_and_restore_resource() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        for small in [false, true] {
            for pointer_owned in [false, true] {
                h.state_mut().viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, if small { 40.0 } else { 700.0 }));
                h.state_mut().navigation.pointer_owned = pointer_owned;
                ctx.set_dragged_id(egui::Id::new("capability-test"));
                let contexts = h.state_mut().navigation_contexts(&ctx);
                for (context, (id, params)) in contexts.into_iter().zip([
                    ("canvas.fit_all", json!({})),
                    ("canvas.fit_selection", json!({})),
                    ("canvas.minimap_toggle", json!({})),
                    ("canvas.pan_to", json!({"x":0,"y":0})),
                ]) {
                    let blocked = context.blocked;
                    assert_eq!(blocked, !pointer_owned || id != "canvas.pan_to");
                    let batch = h.state_mut().navigation_enabled_with_context(context, id, &params);
                    let fresh = h.state_mut().navigation_enabled(&ctx, id, &params);
                    assert_eq!(batch.is_ok(), fresh.is_ok());
                    assert!(h.state_mut().session.swap_resource::<NavigationContext>(None).is_none());
                }
                ctx.stop_dragging();
            }
        }
    }
    #[test]
    fn native_shaped_shifted_digits_and_fallback_fit_once_with_exact_modifiers() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        let fitted = h.state().view;
        h.state_mut().selection.only(NodeId::from_bytes([4; 16]));
        for (logical, physical) in [(egui::Key::Quote, Some(egui::Key::Num2)), (egui::Key::Num2, None)] {
            h.event(native_key(logical, physical, egui::Modifiers::SHIFT));
            h.run();
            assert!(h.state().view.zoom > fitted.zoom);
            h.event(native_key(egui::Key::Exclamationmark, Some(egui::Key::Num1), egui::Modifiers::SHIFT));
            h.run();
            assert_eq!(h.state().view, fitted);
        }
        h.state_mut().view.zoom = 0.5;
        h.event(native_key(egui::Key::Num1, None, egui::Modifiers::SHIFT));
        h.run();
        assert_eq!(h.state().view, fitted);
        let visible = h.state().navigation.visible;
        h.event(native_key(egui::Key::M, Some(egui::Key::M), egui::Modifiers::SHIFT));
        h.run();
        assert_eq!(h.state().navigation.visible, !visible);
        h.run();
        assert_eq!(h.state().navigation.visible, !visible, "one press dispatches once");
        for event in [
            native_key(egui::Key::Num1, Some(egui::Key::Num1), egui::Modifiers::NONE),
            native_key(egui::Key::Exclamationmark, None, egui::Modifiers::SHIFT),
            native_key(egui::Key::Exclamationmark, Some(egui::Key::Num3), egui::Modifiers::SHIFT),
            native_key(egui::Key::Num1, Some(egui::Key::Num1), egui::Modifiers::SHIFT | egui::Modifiers::ALT),
            native_key(egui::Key::M, Some(egui::Key::M), egui::Modifiers::SHIFT | egui::Modifiers::CTRL),
        ] {
            let view = h.state().view;
            h.event(event);
            h.run();
            assert_eq!(h.state().view, view);
            assert_eq!(h.state().navigation.visible, !visible);
        }
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn popup_and_gesture_retain_shortcut_events_and_document_state() {
        for popup in [false, true] {
            let retained = Rc::new(RefCell::new(false));
            let observed = Rc::clone(&retained);
            let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(
                move |ui, app: &mut NodeCraftApp| {
                    if popup {
                        egui::Popup::open_id(ui.ctx(), egui::Id::new("shortcut_owner"));
                    } else {
                        app.marquee = Some(Marquee { start: Pos2::ZERO, base: Selection::default(), cancelled: false });
                    }
                    app.viewport = ui.max_rect();
                    app.navigation_overlay(ui, None);
                    *observed.borrow_mut() |= ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { pressed: true, .. })));
                },
                app(),
            );
            let document = h.state().session.document().clone();
            let view = h.state().view;
            let selection = h.state().selection.clone();
            let visible = h.state().navigation.visible;
            h.event(native_key(egui::Key::M, Some(egui::Key::M), egui::Modifiers::SHIFT));
            h.run();
            assert!(*retained.borrow());
            assert_eq!(h.state().view, view);
            assert_eq!(h.state().selection, selection);
            assert_eq!(h.state().navigation.visible, visible);
            assert_eq!(h.state().session.document(), &document);
            assert_eq!(h.state().session.revision(), 0);
            assert!(!h.state().session.can_undo());
        }
    }
    #[test]
    fn fits_and_toggle_preserve_unrelated_notice_and_clear_owned_notice() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().notice = Some("Provider failed earlier".into());
        for id in ["canvas.fit_all", "canvas.minimap_toggle"] {
            h.state_mut().dispatch_navigation(&ctx, id, &json!({})).unwrap();
            assert_eq!(h.state().notice.as_deref(), Some("Provider failed earlier"));
        }
        h.state_mut().notice = Some(FIT_CLAMP_NOTICE.into());
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        assert!(h.state().notice.is_none());
    }
    #[test]
    fn fit_commands_shortcuts_and_toggle_preserve_document_history() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let original = h.state().session.document().clone();
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_selection", &json!({})).is_err());
        assert!(h.state_mut().session.dispatch("canvas.fit_all", &json!({})).is_err());
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        h.run();
        let fitted = h.state().view;
        let usable = h.state().navigation.usable(h.state().viewport, h.state().theme.tokens());
        for rect in h.state().navigation.geometry.rects.values() {
            assert!(usable.contains_rect(h.state().view.rect_to_screen(h.state().viewport.min, crate::navigation::decorated(*rect, h.state().theme.tokens()))));
        }
        h.state_mut().selection.only(NodeId::from_bytes([4; 16]));
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Num2);
        h.run();
        assert!(h.state().view.zoom > fitted.zoom);
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Num1);
        h.run();
        assert_eq!(h.state().view, fitted);
        let visible = h.state().navigation.visible;
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::M);
        h.run();
        assert_eq!(h.state().navigation.visible, !visible);
        use egui_kittest::kittest::Queryable;
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Show map").click();
        h.run();
        assert!(h.state().navigation.visible);
        assert_eq!(h.state().session.document(), &original);
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
        assert!(h.state_mut().session.swap_resource::<NavigationContext>(None).is_none());
    }
    #[test]
    fn minimap_click_drag_freezes_mapping_and_never_selects_or_moves_nodes() {
        let mut h = harness();
        let original = h.state().session.document().clone();
        h.state_mut().notice = Some("Provider failed earlier".into());
        h.state_mut().selection.only(NodeId::from_bytes([3; 16]));
        let selection = h.state().selection.clone();
        let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
        let from = map.center() + Vec2::new(30.0, 0.0);
        button(&mut h, from, true);
        let mapping = h.state().navigation.drag.unwrap();
        let expected = mapping.world(from);
        let center = h.state().view.visible_world(h.state().viewport).center();
        assert!((f64::from(center.x) - expected.0).abs() < 0.01);
        let to = map.right_bottom() + Vec2::new(100.0, 100.0);
        h.hover_at(to);
        h.run();
        let expected = mapping.world(to);
        let center = h.state().view.visible_world(h.state().viewport).center();
        assert!((f64::from(center.x) - expected.0).abs() < 0.01);
        assert!((f64::from(center.y) - expected.1).abs() < 0.01);
        button(&mut h, to, false);
        assert!(h.state().navigation.drag.is_none());
        assert_eq!(h.state().notice.as_deref(), Some("Provider failed earlier"));
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.document(), &original);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn scoped_context_restores_prior_resources_on_success_and_bad_params() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let old = NavigationContext { canvas: h.state().canvas, viewport_available: false, blocked: true, fit_all: false, fit_selection: false };
        h.state_mut().session.swap_resource(Some(old));
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.pan_to", &json!({"x":1e100,"y":0})).is_err());
        let restored = h.state_mut().session.swap_resource::<NavigationContext>(None).unwrap();
        assert!(restored.blocked);
        assert!(!restored.viewport_available);
        h.state_mut().dispatch_navigation(&ctx, "canvas.pan_to", &json!({"x":50,"y":70})).unwrap();
        assert!(h.state_mut().session.swap_resource::<NavigationContext>(None).is_none());
    }
    #[test]
    fn open_popup_over_minimap_owns_the_click_without_panning() {
        use egui_kittest::kittest::Queryable;
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(
            |ui, app: &mut NodeCraftApp| {
                let id = egui::Id::new("navigation_popup_test");
                egui::Popup::open_id(ui.ctx(), id);
                app.show(ui);
                let map = crate::navigation::layout(app.viewport, true, app.theme.tokens()).map;
                egui::Popup::new(id, ui.ctx().clone(), map, ui.layer_id()).open_memory(None).at_position(map.center()).show(|ui| {
                    ui.button("Popup action").clicked();
                });
            },
            app(),
        );
        let original = h.state().view;
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Popup action").click();
        h.run();
        assert_eq!(h.state().view, original);
        assert!(!h.state().navigation.pointer_owned);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn fit_contains_the_entire_painted_unknown_kind_placeholder() {
        let mut document = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([41; 16]), "Unknown node").unwrap();
        let canvas = nodecraft_types::CanvasId::from_bytes([42; 16]);
        Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let node = nodecraft_graph::Node::new(NodeId::from_bytes([43; 16]), "missing.plugin");
        Change::AddNode { canvas, node: node.clone() }.apply(&mut document).unwrap();
        let app = NodeCraftApp::from_session(Session::new(document).unwrap(), canvas).unwrap();
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        h.run();
        let app = h.state();
        let actual = app.painted[&node.id];
        assert_eq!(app.navigation.geometry.rects[&node.id], actual);
        assert_eq!(actual, card::world_rect(&node, None, app.theme.tokens()));
        let decorated = crate::navigation::decorated(actual, app.theme.tokens());
        assert!(app.navigation.usable(app.viewport, app.theme.tokens()).contains_rect(app.view.rect_to_screen(app.viewport.min, decorated)));
    }
    #[test]
    fn an_invalid_unselected_node_does_not_disable_fit_selected() {
        let mut h = harness();
        h.run();
        let ids: Vec<NodeId> = h.state().navigation.geometry.rects.keys().copied().collect();
        assert!(ids.len() >= 2);
        let (selected, broken) = (ids[0], ids[1]);
        // A node whose geometry fails (a corrupt payload or a NaN position) has no rect and marks
        // the geometry invalid; the cache keeps that until the document changes.
        let geometry = &mut h.state_mut().navigation.geometry;
        geometry.rects.remove(&broken);
        geometry.error = true;
        h.state_mut().selection.only(selected);
        let ctx = h.ctx.clone();
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_err(), "Fit all never frames a partial graph");
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_selection", &json!({})).is_ok(), "an unselected bad node leaves Fit selected on");
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_selection", &json!({})).unwrap();
        let app = h.state();
        let t = app.theme.tokens();
        let decorated = crate::navigation::decorated(app.navigation.geometry.rects[&selected], t);
        assert!(app.navigation.usable(app.viewport, t).contains_rect(app.view.rect_to_screen(app.viewport.min, decorated)));
        // A selected visible node without geometry refuses the fit rather than being left out.
        h.state_mut().selection.add(broken);
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_selection", &json!({})).is_err());
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.fit_selection", &json!({})).is_err());
    }
    #[test]
    fn node_text_editor_owns_shifted_navigation_events_without_mutation() {
        use egui_kittest::kittest::Queryable;
        let retained = Rc::new(RefCell::new(false));
        let observed = Rc::clone(&retained);
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(120).build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                app.show(ui);
                *observed.borrow_mut() |=
                    ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { pressed: true, key: egui::Key::Exclamationmark, .. })));
            },
            NodeCraftApp::headless().unwrap(),
        );
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Text").click();
        h.run();
        let view = h.state().view;
        let document = h.state().session.document().clone();
        let selection = h.state().selection.clone();
        let visible = h.state().navigation.visible;
        h.event(native_key(egui::Key::Exclamationmark, Some(egui::Key::Num1), egui::Modifiers::SHIFT));
        h.run();
        assert!(*retained.borrow(), "navigation leaves the node editor's original event available");
        assert_eq!(h.state().view, view);
        assert_eq!(h.state().navigation.visible, visible);
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn text_focus_suppresses_navigation_shortcuts() {
        let mut text = String::new();
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                ui.text_edit_singleline(&mut text).request_focus();
                app.show(ui);
            },
            app(),
        );
        let original = h.state().view;
        let visible = h.state().navigation.visible;
        for key in [egui::Key::Num1, egui::Key::Num2, egui::Key::M] {
            h.key_press_modifiers(egui::Modifiers::SHIFT, key);
            h.run();
        }
        assert_eq!(h.state().view, original);
        assert_eq!(h.state().navigation.visible, visible);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn active_box_gesture_cannot_be_stolen_by_the_minimap() {
        let mut h = harness();
        let original = h.state().view;
        let from = Pos2::new(50.0, 50.0);
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
        h.run();
        h.hover_at(from + Vec2::splat(40.0));
        h.run();
        let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
        h.hover_at(map.center());
        h.run();
        assert!(h.state().marquee.is_some());
        assert!(!h.state().navigation.pointer_owned);
        assert_eq!(h.state().view, original);
        button(&mut h, map.center(), false);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn empty_canvas_disables_fit_but_keeps_toggle_and_unrelated_host_state() {
        let base = app();
        let canvas = base.canvas;
        let mut document = base.session.document().clone();
        let ids: Vec<_> = document.canvases().get(&canvas).unwrap().nodes().keys().copied().collect();
        for id in ids {
            Change::RemoveNode { canvas, id }.apply(&mut document).unwrap();
        }
        let mut session = Session::new(document).unwrap();
        session.swap_resource(Some(42_u32));
        session.register_host_service("test.unrelated", |_| Ok(json!({"ok":true}))).unwrap();
        let app = NodeCraftApp::from_session(session, canvas).unwrap();
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        let ctx = h.ctx.clone();
        for id in ["canvas.fit_all", "canvas.fit_selection"] {
            assert!(h.state_mut().navigation_enabled(&ctx, id, &json!({})).is_err());
        }
        assert!(h.state().view.pan.is_finite());
        h.state_mut().dispatch_navigation(&ctx, "canvas.minimap_toggle", &json!({})).unwrap();
        assert_eq!(h.state_mut().session.swap_resource::<u32>(None), Some(42));
        assert!(h.state_mut().session.register_host_service("test.unrelated", |_| Ok(json!({}))).is_err());
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn small_viewport_can_hide_map_and_fit_enabled_matches_dispatch_padding() {
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 240.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
        let ctx = h.ctx.clone();
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_err());
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).is_err());
        h.state_mut().dispatch_navigation(&ctx, "canvas.minimap_toggle", &json!({})).unwrap();
        assert!(!h.state().navigation.visible);
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_ok());
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).is_ok());
        // Fit padding includes minimum screen-sized title and handles, not only base padding.
        let t = h.state().theme.tokens();
        let boundary = 2.0 * (t.fit_padding + t.title_min_screen_px + t.handle_min_screen_radius);
        let reserved = t.navigation_toolbar_height + t.navigation_margin + t.navigation_gap;
        h.state_mut().viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, reserved + boundary));
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_err());
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).is_err());
        h.state_mut().viewport.max.y += 1.0;
        assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_all", &json!({})).is_ok());
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).is_ok());
    }
}
