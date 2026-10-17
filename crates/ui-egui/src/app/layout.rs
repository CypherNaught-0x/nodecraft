//! Live geometry capability for one atomic engine layout command. No graph edits in the UI.
use super::*;
use nodecraft_engine::layout_cmds::LayoutContext;
use nodecraft_graph::layout::Extent;
use serde_json::Value as Json;
impl NodeCraftApp {
    fn layout_context(&mut self, ctx: &egui::Context, owner: Option<egui::Id>) -> LayoutContext {
        self.navigation.geometry.refresh(&self.session, self.canvas, self.theme);
        let t = self.theme.tokens();
        let blocked = self.layout_busy(ctx) || (egui::Popup::is_any_open(ctx) && !owner.is_some_and(|id| egui::Popup::is_id_open(ctx, id)));
        let mut extents = Vec::new();
        let mut selection = Vec::new();
        if let Some(scene) = self.session.document().canvases().get(&self.canvas)
            && self.selection.len() <= scene.nodes().len()
            && self.selection.len() <= nodecraft_graph::MAX_CHANGES
        {
            selection.extend(self.selection.nodes().iter().copied());
            for id in &selection {
                if let (Some(node), Some(size)) = (scene.nodes().get(id), self.navigation.geometry.sizes.get(id)) {
                    let (left, top, right, bottom) = card::outer_insets(t, self.view.zoom, !node.outputs.is_empty());
                    extents.push(Extent {
                        id: *id,
                        width: f64::from(size.x),
                        height: f64::from(size.y),
                        left: f64::from(left),
                        top: f64::from(top),
                        right: f64::from(right),
                        bottom: f64::from(bottom),
                    });
                }
            }
        }
        LayoutContext {
            project: self.session.document().id(),
            canvas: self.canvas,
            revision: self.session.revision(),
            selection,
            extents,
            gap: f64::from(t.layout_gap),
            blocked,
        }
    }
    /// Fresh live geometry at the current zoom; raw headless Session requires its own trusted
    /// revision-stamped LayoutContext. Existing host resources are restored on every result.
    pub fn layout_enabled(&mut self, ctx: &egui::Context, params: &Json) -> Result<(), Error> {
        self.layout_enabled_scoped(ctx, params, None)
    }
    pub(super) fn layout_enabled_scoped(&mut self, ctx: &egui::Context, params: &Json, owner: Option<egui::Id>) -> Result<(), Error> {
        let host = self.layout_context(ctx, owner);
        let old = self.session.swap_resource(Some(host));
        let result = self.session.enabled("canvas.auto_format", params);
        self.session.swap_resource(old);
        result
    }
    pub fn dispatch_layout(&mut self, ctx: &egui::Context, params: &Json) -> Result<Json, Error> {
        self.dispatch_layout_scoped(ctx, params, None)
    }
    fn dispatch_layout_scoped(&mut self, ctx: &egui::Context, params: &Json, owner: Option<egui::Id>) -> Result<Json, Error> {
        let host = self.layout_context(ctx, owner);
        let old = self.session.swap_resource(Some(host));
        let result = self.session.dispatch("canvas.auto_format", params);
        self.session.swap_resource(old);
        if result.is_ok() {
            ctx.request_repaint();
        }
        result
    }
    fn layout_busy(&self, ctx: &egui::Context) -> bool {
        !(canvas::MIN_ZOOM..=canvas::MAX_ZOOM).contains(&self.view.zoom)
            || self.text_focused
            || canvas::text_input_focused(ctx)
            || self.drag.is_some()
            || self.marquee.is_some()
            || ctx.dragged_id().is_some()
            || self.navigation.pointer_owned
            || self.navigation.drag.is_some()
    }
    pub(super) fn layout_base_available(&mut self, ctx: &egui::Context) -> bool {
        let host = self.layout_context(ctx, None);
        // Used only inside the card's verified context menu. Its actual root ID is checked
        // separately there, then checked again at scoped dispatch.
        !self.layout_busy(ctx) && host.selection.len() >= 2 && host.extents.len() == host.selection.len()
    }
    pub(super) fn layout_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        let popup = egui::Id::new("format_selection_popup");
        let owned_before = egui::Popup::is_id_open(&ctx, popup);
        let host = self.layout_context(&ctx, Some(popup));
        let can_open = !host.blocked && host.selection.len() >= 2 && host.extents.len() == host.selection.len();
        let mut mode = None;
        let t = self.theme.tokens();
        let area = egui::Area::new(egui::Id::new("selection_format_toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(self.viewport.left_top() + Vec2::splat(t.navigation_margin))
            .show(&ctx, |ui| {
                let button =
                    ui.add_enabled(can_open, egui::Button::new("Format selection").min_size(Vec2::new(t.layout_button_width, t.navigation_toolbar_height)));
                button.clone().on_hover_text("Arrange selected nodes in one undo step. Finish text editing or gestures first.");
                egui::Popup::menu(&button).id(popup).show(|ui| {
                    for (label, choice) in [("Grid", "grid"), ("Dependency order", "layered")] {
                        let enabled = self.layout_enabled_scoped(&ctx, &json!({"mode":choice}), Some(popup));
                        let response = ui.add_enabled(enabled.is_ok(), egui::Button::new(label));
                        if response.clicked() {
                            mode = Some(choice);
                            ui.close();
                        }
                        if let Err(error) = enabled {
                            response.on_hover_text(error.message);
                        }
                    }
                });
            });
        // Egui clears focus for clicks elsewhere even on disabled widgets. Preserve exactly
        // the prior editor for this disabled toolbar interaction before card lost_focus runs.
        if !can_open
            && self.text_focused
            && !egui::Popup::is_any_open(&ctx)
            && ctx.memory(|m| m.focused()).is_none_or(|id| Some(id) == self.last_text_focus)
            && ctx.input(|i| i.pointer.any_pressed() || i.pointer.any_released())
            && ctx.pointer_interact_pos().is_some_and(|p| area.response.rect.contains(p) && ctx.layer_id_at(p) == Some(area.response.layer_id))
            && let Some(id) = self.last_text_focus
        {
            ctx.memory_mut(|m| m.request_focus(id));
            // Widget response creation also surrenders focus for the same click. Suppress that
            // only for the rest of this app.show; the caller restores the original option.
            ctx.options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        if let Some(mode) = mode {
            self.apply_layout_menu(&ctx, mode, popup);
        }
        owned_before
            || egui::Popup::is_id_open(&ctx, popup)
            || ctx.pointer_hover_pos().is_some_and(|p| area.response.rect.contains(p) && ctx.layer_id_at(p) == Some(area.response.layer_id))
    }
    pub(super) fn apply_layout_menu(&mut self, ctx: &egui::Context, mode: &str, owner: egui::Id) {
        // Owner is an actual popup ID captured from this toolbar or the card response.
        if let Err(error) = self.dispatch_layout_scoped(ctx, &json!({"mode":mode}), Some(owner)) {
            self.notice = Some(error.message);
        } else {
            self.notice = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    fn id(n: u8) -> NodeId {
        NodeId::from_bytes([n; 16])
    }
    fn app() -> NodeCraftApp {
        let base = NodeCraftApp::headless().unwrap();
        let canvas = base.canvas;
        let mut doc = base.session.document().clone();
        let template = doc.canvases().get(&canvas).unwrap().nodes().get(&id(3)).unwrap().clone();
        for (n, x, y) in [(4, 300.0, -80.0), (5, 90.0, 400.0)] {
            let mut node = template.clone();
            node.id = id(n);
            node.title = if n == 4 { "Second" } else { "Anchor" }.into();
            node.pos = nodecraft_graph::Position { x, y };
            Change::AddNode { canvas, node }.apply(&mut doc).unwrap();
        }
        let mut app = NodeCraftApp::from_session(Session::new(doc).unwrap(), canvas).unwrap();
        app.selection.only(id(3));
        app.selection.add(id(4));
        app
    }
    fn harness() -> Harness<'static, NodeCraftApp> {
        Harness::builder().with_size(Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app())
    }
    #[test]
    fn real_format_menu_both_modes_one_undo_and_preserved_selection_view_unselected() {
        for label in ["Grid", "Dependency order"] {
            let mut h = harness();
            let before = h.state().session.document().clone();
            let selection = h.state().selection.clone();
            let view = h.state().view;
            h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").click();
            h.run();
            h.get_by_role_and_label(egui::accesskit::Role::Button, label).click();
            h.run();
            assert_eq!(h.state().session.revision(), 1);
            let after = h.state().session.document().clone();
            assert_ne!(after, before);
            let canvas = h.state().canvas;
            assert_eq!(after.canvases().get(&canvas).unwrap().nodes().get(&id(5)), before.canvases().get(&canvas).unwrap().nodes().get(&id(5)));
            assert_eq!(h.state().selection, selection);
            assert_eq!(h.state().view, view);
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
            h.run();
            assert_eq!(h.state().session.document(), &before);
            assert!(!h.state().session.can_undo());
            h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
            h.run();
            assert_eq!(h.state().session.document(), &after);
        }
    }
    #[test]
    fn selected_card_context_menu_uses_its_exact_popup_owner() {
        let mut h = harness();
        let selected = h.state().selection.clone();
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Node Second").rect().min + Vec2::splat(6.0);
        h.hover_at(point);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
            h.run();
        }
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Grid").click();
        h.run();
        assert_eq!(h.state().session.revision(), 1);
        assert_eq!(h.state().selection, selected);
    }
    #[test]
    fn live_bridge_restores_resource_rejects_popup_gesture_and_small_selection() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let params = json!({"mode":"grid"});
        let mut old = h.state_mut().layout_context(&ctx, None);
        old.blocked = true;
        h.state_mut().session.swap_resource(Some(old));
        assert!(h.state_mut().dispatch_layout(&ctx, &json!({"mode":"bad"})).is_err());
        assert!(h.state_mut().session.swap_resource::<LayoutContext>(None).unwrap().blocked);
        egui::Popup::open_id(&ctx, egui::Id::new("unrelated_popup"));
        assert!(h.state_mut().layout_enabled(&ctx, &params).is_err());
        assert!(h.state_mut().dispatch_layout(&ctx, &params).is_err());
        egui::Popup::close_all(&ctx);
        h.state_mut().navigation.pointer_owned = true;
        assert!(h.state_mut().dispatch_layout(&ctx, &params).is_err());
        h.state_mut().navigation.pointer_owned = false;
        h.state_mut().selection.only(id(3));
        assert!(h.state_mut().dispatch_layout(&ctx, &params).is_err());
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn unrelated_popup_blocks_toolbar_from_first_pointer_press() {
        let owner = egui::Id::new("unrelated_layout_popup");
        let mut h = Harness::builder().with_size(Vec2::new(1200.0, 800.0)).build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                egui::Popup::open_id(ui.ctx(), owner);
                app.show(ui);
                egui::Popup::new(owner, ui.ctx().clone(), app.viewport, ui.layer_id())
                    .open_memory(None)
                    .at_position(app.viewport.left_top() + Vec2::splat(app.theme.tokens().navigation_margin))
                    .show(|ui| {
                        ui.button("Unrelated action").clicked();
                    });
            },
            app(),
        );
        let selection = h.state().selection.clone();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Unrelated action").click();
        h.run();
        assert!(!egui::Popup::is_id_open(&h.ctx, egui::Id::new("format_selection_popup")));
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Grid").is_none());
        assert_eq!(h.state().session.revision(), 0);
        assert_eq!(h.state().selection, selection);
    }
    #[test]
    fn offscreen_instance_metrics_and_revision_change_control_layout_without_static_fallback() {
        use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
        use nodecraft_types::{ParamControl, ParamSchema, ParamSpec, PortType};
        use std::borrow::Cow;
        struct Kind(NodeDescriptor, NodeDescriptor);
        impl NodeKind for Kind {
            fn descriptor(&self) -> &NodeDescriptor {
                &self.0
            }
            fn instance<'a>(&'a self, node: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
                Ok(Cow::Borrowed(if node.title == "Tall" { &self.1 } else { &self.0 }))
            }
            fn mode(&self, _: &[PortType]) -> Option<ModeId> {
                None
            }
            fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
                Ok(vec![])
            }
        }
        let short = NodeDescriptor {
            id: "test.layout".into(),
            label: "Layout".into(),
            category: Category::Source,
            inputs: vec![],
            outputs: vec![],
            params: ParamSchema { params: vec![] },
            quick_add: None,
        };
        let mut tall = short.clone();
        tall.params.params = (0..9)
            .map(|n| ParamSpec {
                name: format!("p{n}"),
                label: format!("P{n}"),
                description: None,
                control: ParamControl::Toggle,
                required: false,
                default: Some(Value::Bool(false)),
                visible_if: None,
            })
            .collect();
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(Kind(short, tall))).unwrap();
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut doc = Project::new(ProjectId::from_bytes([2; 16]), "Dynamic").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut doc).unwrap();
        for (n, y, title) in [(3, 0.0, "Short"), (4, -1000.0, "Tall")] {
            let mut node = nodecraft_graph::Node::new(id(n), "test.layout");
            node.pos = nodecraft_graph::Position { x: 100_000.0, y };
            node.title = title.into();
            Change::AddNode { canvas, node }.apply(&mut doc).unwrap();
        }
        let session = Session::with_registries(doc, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let mut app = NodeCraftApp::from_session(session, canvas).unwrap();
        app.selection.only(id(3));
        app.selection.add(id(4));
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
        let ctx = h.ctx.clone();
        h.state_mut().schemas.clear(); // an evicted visible schema never controls layout
        h.state_mut().dispatch_layout(&ctx, &json!({"mode":"grid","columns":1})).unwrap();
        let before = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id(3)).unwrap().pos.y;
        assert_eq!(h.state().navigation.geometry.sizes.get(&id(4)).unwrap().y, h.state().theme.tokens().card_max_height);
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":id(4),"title":"Short"})).unwrap();
        h.state_mut().dispatch_layout(&ctx, &json!({"mode":"grid","columns":1})).unwrap();
        let after = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id(3)).unwrap().pos.y;
        assert!(before - after > 400.0);
        assert_eq!(h.state().navigation.geometry.sizes.get(&id(4)).unwrap().y, h.state().theme.tokens().card_min_height);
    }
    #[test]
    fn dirty_card_parameter_clicking_disabled_format_preserves_document_and_draft() {
        let mut h = harness();
        let before = h.state().session.document().clone();
        let policy = h.ctx.memory(|m| m.options.input_options.surrender_focus_on);
        h.get_all_by_role_and_label(egui::accesskit::Role::TextInput, "Text").next().unwrap().click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("Unaccepted draft".into()));
        h.run();
        assert!(canvas::text_input_focused(&h.ctx));
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").click();
        h.run();
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
        assert!(canvas::text_input_focused(&h.ctx));
        assert_eq!(h.ctx.memory(|m| m.options.input_options.surrender_focus_on), policy);
        h.key_press(egui::Key::Escape);
        h.run();
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
    #[test]
    fn dirty_title_clicking_disabled_format_preserves_document_and_focus() {
        let mut h = harness();
        let before = h.state().session.document().clone();
        let policy = h.ctx.memory(|m| m.options.input_options.surrender_focus_on);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Node Second").focus();
        h.run();
        h.key_press(egui::Key::F2);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("Unaccepted title".into()));
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").click();
        h.run();
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
        assert!(canvas::text_input_focused(&h.ctx));
        assert_eq!(h.ctx.memory(|m| m.options.input_options.surrender_focus_on), policy);
        h.key_press(egui::Key::Escape);
        h.run();
        h.run();
        assert_eq!(h.state().session.document(), &before);
    }
    #[test]
    fn overlapping_popup_editor_keeps_ownership_and_stale_focus_is_not_restored() {
        use std::{cell::Cell, rc::Rc};
        let open = Rc::new(Cell::new(false));
        let show = open.clone();
        let mut text = String::new();
        let owner = egui::Id::new("layout_focus_popup");
        let mut h = Harness::builder().with_size(Vec2::new(1200.0, 800.0)).build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                if show.get() {
                    egui::Popup::open_id(ui.ctx(), owner);
                }
                app.show(ui);
                if show.get() {
                    egui::Popup::new(owner, ui.ctx().clone(), app.viewport, ui.layer_id())
                        .open_memory(None)
                        .at_position(app.viewport.left_top() + Vec2::splat(app.theme.tokens().navigation_margin))
                        .show(|ui| {
                            let label = ui.label("Popup editor");
                            ui.text_edit_singleline(&mut text).labelled_by(label.id);
                        });
                }
            },
            app(),
        );
        h.get_all_by_role_and_label(egui::accesskit::Role::TextInput, "Text").next().unwrap().click();
        h.run();
        let old = h.ctx.memory(|m| m.focused()).unwrap();
        let policy = h.ctx.memory(|m| m.options.input_options.surrender_focus_on);
        open.set(true);
        h.run();
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Popup editor").click();
        h.run();
        assert_ne!(h.ctx.memory(|m| m.focused()), Some(old));
        assert!(h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Popup editor").is_focused());
        assert!(!egui::Popup::is_id_open(&h.ctx, egui::Id::new("format_selection_popup")));
        assert_eq!(h.ctx.memory(|m| m.options.input_options.surrender_focus_on), policy);
        open.set(false);
        egui::Popup::close_all(&h.ctx);
        h.run();
        // Simulate a stale prior ID while a different current owner has focus.
        let current = egui::Id::new("new_focus_owner");
        h.ctx.memory_mut(|m| m.request_focus(current));
        h.state_mut().last_text_focus = Some(old);
        h.state_mut().text_focused = true;
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
        h.hover_at(point);
        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        assert_ne!(h.ctx.memory(|m| m.focused()), Some(old));
        assert_eq!(h.ctx.memory(|m| m.options.input_options.surrender_focus_on), policy);
    }
    #[test]
    fn unrelated_dirty_editor_cannot_be_blurred_by_format_toolbar() {
        let mut text = String::from("unsaved draft");
        let mut h = Harness::builder().with_size(Vec2::new(1200.0, 800.0)).build_ui_state(
            move |ui, app: &mut NodeCraftApp| {
                ui.text_edit_singleline(&mut text).request_focus();
                app.show(ui);
            },
            app(),
        );
        let before = h.state().session.document().clone();
        let ctx = h.ctx.clone();
        assert!(h.state_mut().dispatch_layout(&ctx, &json!({"mode":"grid"})).is_err());
        // Disabled toolbar remains discoverable and does not take text focus on pointer events.
        h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").click();
        h.run();
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Grid").is_none());
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
        assert!(canvas::text_input_focused(&ctx));
    }
}
