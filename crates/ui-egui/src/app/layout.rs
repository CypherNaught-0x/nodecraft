//! Live geometry capability for one atomic engine layout command. No graph edits in the UI.
use super::*;
use nodecraft_engine::layout_cmds::LayoutContext;
use nodecraft_graph::layout::Extent;
use serde_json::Value as Json;
// Consume a disabled toolbar's pointer input before *any* widgets are built, including
// editors supplied by a host before app.show. The capability lasts just one pass and is
// rearmed by the visible owner; it never restores a prior focus or changes focus policy.
#[derive(Clone)]
struct PassiveToolbar {
    rect: Rect,
    layer: egui::LayerId,
    focus: egui::Id,
    viewport: Rect,
}
fn passive_toolbar(ctx: &egui::Context) {
    let installed = egui::Id::new("layout_passive_hook");
    if ctx.data_mut(|d| d.get_temp::<bool>(installed).unwrap_or(false)) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(installed, true));
    ctx.on_begin_pass(
        "layout passive toolbar",
        std::sync::Arc::new(|ctx| {
            let owner = ctx.data_mut(|d| {
                let id = egui::Id::new("layout_passive_owner");
                let owner = d.get_temp::<PassiveToolbar>(id);
                d.remove::<PassiveToolbar>(id);
                owner
            });
            let Some(owner) = owner else {
                return;
            };
            if egui::Popup::is_any_open(ctx) || ctx.memory(|m| m.focused()) != Some(owner.focus) || ctx.viewport_rect() != owner.viewport {
                return;
            }
            if ctx.input(|i| {
                let other = [egui::PointerButton::Secondary, egui::PointerButton::Middle, egui::PointerButton::Extra1, egui::PointerButton::Extra2];
                (i.pointer.primary_pressed() || i.pointer.primary_released())
                    && !other.into_iter().any(|button| i.pointer.button_down(button))
                    && !i.events.iter().any(|event| matches!(event, egui::Event::PointerButton { button, .. } if *button != egui::PointerButton::Primary))
            }) && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| owner.rect.contains(p) && ctx.layer_id_at(p) == Some(owner.layer))
            {
                ctx.input_mut(|i| {
                    i.pointer = egui::PointerState::default();
                    i.events.retain(|e| !matches!(e, egui::Event::PointerButton { button: egui::PointerButton::Primary, .. }));
                });
            }
        }),
    );
}
impl NodeCraftApp {
    fn layout_context(&mut self, ctx: &egui::Context, owner: Option<egui::Id>) -> LayoutContext {
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
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
                    let (left, top, right, bottom) = card::outer_insets(t, 1.0, !node.outputs.is_empty());
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
    /// Fresh actual-instance geometry at reference zoom 1.0; raw headless Session requires its own trusted
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
            || self.wiring.is_some()
            || self.palette.is_some()
            || self.turn_into.is_some()
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
        passive_toolbar(&ctx);
        if self.selection.len() < 2 {
            return false;
        }
        let popup = egui::Id::new("format_selection_popup");
        let owned_before = egui::Popup::is_id_open(&ctx, popup);
        let host = self.layout_context(&ctx, Some(popup));
        let can_open = !host.blocked && host.selection.len() >= 2 && host.extents.len() == host.selection.len();
        let mut mode = None;
        let t = self.theme.tokens();
        let area = egui::Area::new(egui::Id::new("selection_format_toolbar"))
            .order(egui::Order::Foreground)
            .movable(false)
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
        if !can_open
            && !egui::Popup::is_any_open(&ctx)
            && let Some(focus) = ctx.memory(|m| m.focused())
            && canvas::text_input_focused(&ctx)
        {
            let viewport = ctx.viewport_rect();
            ctx.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new("layout_passive_owner"),
                    PassiveToolbar { rect: area.response.rect, layer: area.response.layer_id, focus, viewport },
                )
            });
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
    fn primary_click(h: &mut Harness<'static, NodeCraftApp>, point: Pos2) {
        h.hover_at(point);
        h.run();
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.run();
        }
    }
    #[test]
    fn zoom_independent_ui_and_explicit_headless_geometry_agree_and_reapply_is_noop() {
        for mode in ["grid", "layered"] {
            let mut expected = None;
            for zoom in [0.1, 1.0, 4.0] {
                let mut app = app();
                app.view.zoom = zoom;
                let ctx = egui::Context::default();
                let host = app.layout_context(&ctx, None);
                let mut headless = Session::new(app.session.document().clone()).unwrap();
                // An explicit headless host supplies the same reference-world metrics.
                headless.swap_resource(Some(host));
                headless.dispatch("canvas.auto_format", &json!({"mode":mode})).unwrap();
                app.dispatch_layout(&ctx, &json!({"mode":mode})).unwrap();
                assert_eq!(app.session.document(), headless.document());
                if let Some(expected) = &expected {
                    assert_eq!(app.session.document(), expected);
                }
                expected = Some(app.session.document().clone());
                for next in [4.0, 0.1, 1.0] {
                    app.view.zoom = next;
                    assert_eq!(app.dispatch_layout(&ctx, &json!({"mode":mode})).unwrap(), json!({"moved":0}));
                    assert_eq!(app.session.revision(), 1);
                }
                app.session.dispatch("edit.undo", &json!({})).unwrap();
                assert!(!app.session.can_undo());
            }
        }
    }
    #[test]
    fn navigation_placeholders_remain_fittable_but_do_not_authorize_layout() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Unavailable kinds").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        for n in [3, 4] {
            let mut node = nodecraft_graph::Node::new(id(n), "missing.plugin");
            node.pos = nodecraft_graph::Position { x: f32::from(n) * 100.0, y: f32::from(n) * -50.0 };
            Change::AddNode { canvas, node }.apply(&mut document).unwrap();
        }
        let mut app = NodeCraftApp::from_session(Session::new(document).unwrap(), canvas).unwrap();
        app.selection.only(id(3));
        app.selection.add(id(4));
        let ctx = egui::Context::default();
        let before = app.session.document().clone();
        assert!(app.dispatch_layout(&ctx, &json!({"mode":"grid"})).is_err());
        for n in [3, 4] {
            let node = before.canvases()[&canvas].nodes().get(&id(n)).unwrap();
            assert_eq!(app.navigation.geometry.rects[&id(n)], card::world_rect(node, None, app.theme.tokens()));
            assert!(!app.navigation.geometry.sizes.contains_key(&id(n)));
        }
        assert!(app.navigation.geometry.bounds(None, app.theme.tokens()).is_some());
        assert_eq!(app.session.document(), &before);
        assert_eq!(app.session.revision(), 0);
    }
    #[test]
    fn passive_toolbar_preserves_simultaneous_nonprimary_buttons_and_releases() {
        use std::sync::{Arc, Mutex};
        for (other, pressed) in [
            (egui::PointerButton::Secondary, false),
            (egui::PointerButton::Middle, false),
            (egui::PointerButton::Extra1, true),
            (egui::PointerButton::Extra2, false),
        ] {
            let mut h = harness();
            h.get_all_by_role_and_label(egui::accesskit::Role::TextInput, "Text").next().unwrap().click();
            h.run();
            let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
            h.hover_at(point);
            h.run();
            let observations = Arc::new(Mutex::new(Vec::new()));
            let captured = observations.clone();
            h.ctx.on_begin_pass(
                "observe nonprimary preservation",
                Arc::new(move |ctx| {
                    let observation = ctx.input(|i| {
                        (
                            i.pointer.primary_pressed(),
                            i.events
                                .iter()
                                .any(|e| matches!(e, egui::Event::PointerButton { button, pressed: actual, .. } if *button == other && *actual == pressed)),
                            i.pointer.button_down(other),
                            i.pointer.button_released(other),
                        )
                    });
                    captured.lock().unwrap().push(observation);
                }),
            );
            if !pressed {
                h.input_mut().events.push(egui::Event::PointerButton { pos: point, button: other, pressed: true, modifiers: egui::Modifiers::NONE });
            }
            for (button, down) in [(other, pressed), (egui::PointerButton::Primary, true)] {
                h.input_mut().events.push(egui::Event::PointerButton { pos: point, button, pressed: down, modifiers: egui::Modifiers::NONE });
            }
            h.step();
            let observations = observations.lock().unwrap().clone();
            assert!(
                observations.iter().any(|(primary, event, down, released)| *primary && *event && if pressed { *down } else { *released }),
                "{other:?} {pressed}: {observations:?}"
            );
        }
    }
    #[test]
    fn changed_viewport_expires_passive_geometry_before_pointer_input() {
        use std::sync::{Arc, Mutex};
        let mut h = harness();
        h.get_all_by_role_and_label(egui::accesskit::Role::TextInput, "Text").next().unwrap().click();
        h.run();
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
        h.hover_at(point);
        h.run();
        let observed = Arc::new(Mutex::new(false));
        let captured = observed.clone();
        h.ctx.on_begin_pass(
            "observe resize pointer",
            Arc::new(move |ctx| {
                *captured.lock().unwrap() |= ctx.input(|i| i.pointer.primary_pressed());
            }),
        );
        h.set_size(Vec2::new(1000.0, 700.0));
        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        assert!(*observed.lock().unwrap());
    }
    #[test]
    fn geometry_replacement_same_document_and_revision_cannot_reuse_old_session_cache() {
        let mut app = app();
        let ctx = egui::Context::default();
        app.layout_context(&ctx, None);
        let before = app.navigation.geometry.rects[&id(3)];
        let mut document = app.session.document().clone();
        Change::MoveNode { canvas: app.canvas, id: id(3), pos: nodecraft_graph::Position { x: 90_000.0, y: 50_000.0 } }.apply(&mut document).unwrap();
        app.session = Session::new(document).unwrap();
        app.layout_context(&ctx, None);
        assert_ne!(app.navigation.geometry.rects[&id(3)], before);
        assert_eq!(app.navigation.geometry.rects[&id(3)].min.x, 90_000.0);
    }
    #[test]
    fn hidden_toolbar_clears_passive_owner_and_primary_lifecycle_does_not_stick() {
        let mut h = harness();
        h.get_all_by_role_and_label(egui::accesskit::Role::TextInput, "Text").next().unwrap().click();
        h.run();
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
        primary_click(&mut h, point);
        assert!(canvas::text_input_focused(&h.ctx));
        assert!(!h.ctx.input(|i| i.pointer.any_down()));
        h.state_mut().selection.clear();
        h.run();
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Format selection").is_none());
        h.run();
        assert!(h.ctx.data(|d| d.get_temp::<PassiveToolbar>(egui::Id::new("layout_passive_owner"))).is_none());
        primary_click(&mut h, point);
        assert!(!canvas::text_input_focused(&h.ctx));
        assert!(!h.ctx.input(|i| i.pointer.any_down()));
    }
    #[test]
    fn unknown_selected_instance_never_uses_guessed_metrics_or_partially_moves() {
        let mut app = app();
        let mut document = app.session.document().clone();
        let unknown = nodecraft_graph::Node::new(id(6), "unknown.layout");
        Change::AddNode { canvas: app.canvas, node: unknown }.apply(&mut document).unwrap();
        app.session = Session::new(document.clone()).unwrap();
        app.selection.only(id(3));
        app.selection.add(id(6));
        let ctx = egui::Context::default();
        assert!(app.layout_enabled(&ctx, &json!({"mode":"grid"})).is_err());
        assert!(app.dispatch_layout(&ctx, &json!({"mode":"grid"})).is_err());
        assert_eq!(app.session.document(), &document);
        assert_eq!(app.session.revision(), 0);
        assert!(!app.navigation.geometry.sizes.contains_key(&id(6)));
    }
    #[test]
    fn real_format_menu_both_modes_one_undo_and_preserved_selection_view_unselected() {
        for label in ["Grid", "Dependency order"] {
            let mut h = harness();
            let before = h.state().session.document().clone();
            let selection = h.state().selection.clone();
            let view = h.state().view;
            let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
            primary_click(&mut h, point);
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
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
        primary_click(&mut h, point);
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
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
        primary_click(&mut h, point);
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
                let response = ui.text_edit_singleline(&mut text);
                if ui.ctx().cumulative_frame_nr() == 0 {
                    response.request_focus();
                }
                app.show(ui);
            },
            app(),
        );
        let before = h.state().session.document().clone();
        let ctx = h.ctx.clone();
        assert!(h.state_mut().dispatch_layout(&ctx, &json!({"mode":"grid"})).is_err());
        // Disabled toolbar remains discoverable and does not take text focus on pointer events.
        let point = h.get_by_role_and_label(egui::accesskit::Role::Button, "Format selection").rect().center();
        primary_click(&mut h, point);
        h.run();
        assert!(h.query_by_role_and_label(egui::accesskit::Role::Button, "Grid").is_none());
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
        assert!(canvas::text_input_focused(&ctx));
    }
    fn assert_retained_owner_refuses(app: &mut NodeCraftApp, ctx: &egui::Context, mode: &str) {
        assert!(ctx.dragged_id().is_none());
        assert!(!canvas::text_input_focused(ctx));
        assert!(!app.text_focused);
        assert!(app.drag.is_none() && app.marquee.is_none());
        assert!(!app.navigation.pointer_owned && app.navigation.drag.is_none());
        let mut prior = app.layout_context(ctx, None);
        prior.gap = 80.0;
        prior.blocked = false;
        app.session.swap_resource(Some(prior.clone()));
        let document = app.session.document().clone();
        let revision = app.session.revision();
        let events = app.session.events_since(0).unwrap();
        let history = (app.session.can_undo(), app.session.can_redo());
        let status = (app.session.file_status(), app.session.status_revision());
        let selection = app.selection.clone();
        let view = app.view;
        let params = json!({"mode":mode});
        // Evaluate both public routes before asserting, so the before-fix test exercises dispatch too.
        let enabled = app.layout_enabled(ctx, &params);
        let dispatched = app.dispatch_layout(ctx, &params);
        assert_eq!(enabled.unwrap_err().kind, nodecraft_types::ErrorKind::Conflict);
        assert_eq!(dispatched.unwrap_err().kind, nodecraft_types::ErrorKind::Conflict);
        assert_eq!(app.session.document(), &document);
        assert_eq!(app.session.revision(), revision);
        assert_eq!(app.session.events_since(0).unwrap(), events);
        assert_eq!((app.session.can_undo(), app.session.can_redo()), history);
        assert_eq!((app.session.file_status(), app.session.status_revision()), status);
        assert_eq!(app.selection, selection);
        assert_eq!(app.view, view);
        let restored = app.session.swap_resource::<LayoutContext>(None).unwrap();
        assert_eq!(format!("{restored:?}"), format!("{prior:?}"));
    }
    #[test]
    fn retained_wiring_without_egui_drag_blocks_public_layout_until_released() {
        for mode in ["grid", "layered"] {
            let mut app = app();
            let ctx = egui::Context::default();
            app.wiring = Some(Wiring {
                from: nodecraft_graph::Endpoint { node: id(3), port: "out".into() },
                port_type: nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: false },
                moving: None,
            });
            assert_retained_owner_refuses(&mut app, &ctx, mode);
            assert!(app.wiring.is_some());
            app.wiring = None;
            assert!(app.layout_enabled(&ctx, &json!({"mode":mode})).is_ok());
            app.dispatch_layout(&ctx, &json!({"mode":mode})).unwrap();
            assert_eq!(app.session.revision(), 1);
        }
    }
    #[test]
    fn retained_palette_without_egui_focus_blocks_public_layout_until_released() {
        for mode in ["grid", "layered"] {
            let mut app = app();
            let ctx = egui::Context::default();
            app.palette = Some(Palette::new(Pos2::ZERO, Pos2::new(600.0, 650.0)));
            assert_retained_owner_refuses(&mut app, &ctx, mode);
            assert!(app.palette.is_some());
            app.palette = None;
            assert!(app.layout_enabled(&ctx, &json!({"mode":mode})).is_ok());
            app.dispatch_layout(&ctx, &json!({"mode":mode})).unwrap();
            assert_eq!(app.session.revision(), 1);
        }
    }
    #[test]
    fn real_palette_double_click_tab_and_escape_transition_guards_layout() {
        let mut h =
            Harness::builder().with_step_dt(1.0 / 60.0).with_size(Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
        h.state_mut().selection.clear();
        h.state_mut().view.pan += Vec2::new(-5000.0, -5000.0);
        h.run();
        let point = Pos2::new(560.0, 650.0);
        h.hover_at(point);
        h.step();
        for _ in 0..2 {
            for pressed in [true, false] {
                h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                h.step();
            }
        }
        h.run();
        assert!(h.state().palette.is_some());
        h.key_press(egui::Key::Tab);
        h.run();
        h.state_mut().selection.only(id(3));
        h.state_mut().selection.add(id(4));
        let ctx = h.ctx.clone();
        assert_retained_owner_refuses(h.state_mut(), &ctx, "grid");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().palette.is_none());
        h.state_mut().dispatch_layout(&ctx, &json!({"mode":"grid"})).unwrap();
        assert_eq!(h.state().session.revision(), 1);
    }
    #[test]
    fn integration_retained_turn_into_blocks_layout_after_pointer_gesture_until_escape() {
        for mode in ["grid", "layered"] {
            let mut h = super::super::tests::chain_harness();
            let from = super::super::tests::port_point(&h, 1, noodle::Side::Output);
            super::super::tests::wire_drag(&mut h, from, Pos2::new(520.0, 380.0));
            h.run();
            assert!(h.state().turn_into.is_some());
            h.state_mut().selection.only(NodeId::from_bytes([1; 16]));
            h.state_mut().selection.add(NodeId::from_bytes([4; 16]));
            // Fresh egui context has no transient focus/drag/popup; only the retained menu remains.
            let ctx = egui::Context::default();
            assert_retained_owner_refuses(h.state_mut(), &ctx, mode);
            assert!(h.state().turn_into.is_some());
            h.key_press(egui::Key::Escape);
            h.run();
            assert!(h.state().turn_into.is_none());
            h.state_mut().dispatch_layout(&ctx, &json!({"mode":mode})).unwrap();
        }
    }
    #[test]
    fn integration_history_metrics_refresh_and_both_layout_modes_reapply_without_overlap() {
        for mode in ["grid", "layered"] {
            let mut app = app();
            let ctx = egui::Context::default();
            let plain = app.layout_context(&ctx, None).extents;
            let canvas = app.canvas;
            app.session
                .dispatch(
                    "run.record",
                    &json!({"canvas":canvas,"results":[{"node":id(3),"generation":{
                "id":"29292929-2929-4929-8929-292929292929","outputs":[{"type":"text","value":"Public layout history"}]}}]}),
                )
                .unwrap();
            let history = app.layout_context(&ctx, None).extents;
            let before = plain.iter().find(|e| e.id == id(3)).unwrap();
            let after = history.iter().find(|e| e.id == id(3)).unwrap();
            assert!(after.height > before.height);
            let node = app.session.document().canvases().get(&canvas).unwrap().nodes().get(&id(3)).unwrap();
            let descriptor = app.session.kinds().get(&node.kind).unwrap().instance(node).unwrap();
            let size = card::world_size(node, Some(&descriptor), app.theme.tokens());
            assert_eq!(f64::from(size.y), after.height);
            assert_eq!(card::world_rect(node, Some(&descriptor), app.theme.tokens()).height(), size.y);
            let mut far = node.clone();
            far.pos = nodecraft_graph::Position { x: 9_000_000.0, y: 9_000_000.0 };
            assert_eq!(card::world_size(&far, Some(&descriptor), app.theme.tokens()), size);
            let request = if mode == "grid" { json!({"mode":mode,"columns":1}) } else { json!({"mode":mode}) };
            app.dispatch_layout(&ctx, &request).unwrap();
            let scene = app.session.document().canvases().get(&canvas).unwrap();
            let a = scene.nodes().get(&id(3)).unwrap();
            let b = scene.nodes().get(&id(4)).unwrap();
            assert!(f64::from(a.pos.y) + after.height + after.bottom <= f64::from(b.pos.y) - history.iter().find(|e| e.id == id(4)).unwrap().top);
            let revision = app.session.revision();
            assert_eq!(app.dispatch_layout(&ctx, &request).unwrap(), json!({"moved":0}));
            assert_eq!(app.session.revision(), revision);
            app.session.dispatch("history.clear", &json!({"canvas":canvas,"id":id(3)})).unwrap();
            let cleared = app.layout_context(&ctx, None).extents;
            assert_eq!(cleared.iter().find(|e| e.id == id(3)).unwrap().height, before.height);
        }
    }
}
