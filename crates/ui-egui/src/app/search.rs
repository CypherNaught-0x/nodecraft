//! Scoped live search host and its exact editor/input ownership.
use super::*;
use crate::search::{Direction, MAX_QUERY_BYTES};
use nodecraft_engine::search_cmds::SearchContext;
use serde_json::Value as Json;
fn search_id(id: &str) -> bool {
    matches!(id, "canvas.search_open" | "canvas.search_query" | "canvas.search_next" | "canvas.search_previous" | "canvas.search_close")
}
impl NodeCraftApp {
    pub(super) fn search_input_id(&self) -> egui::Id {
        egui::Id::new("canvas_search_query").with(self.canvas)
    }
    fn search_rect(&self) -> Rect {
        let t = self.theme.tokens();
        let width = if self.search.open { t.search_width } else { t.search_button_width }.min((self.viewport.width() - 2.0 * t.navigation_margin).max(1.0));
        let height = if self.search.open { t.search_height } else { t.navigation_toolbar_height };
        Rect::from_min_size(Pos2::new(self.viewport.max.x - width - t.navigation_margin, self.viewport.min.y + t.navigation_margin), Vec2::new(width, height))
    }
    fn search_context(&mut self, ctx: &egui::Context) -> SearchContext {
        self.search.bind_session(&mut self.session);
        self.search.sync(self.session.document(), self.canvas, self.session.revision());
        let own_focus = ctx.memory(|m| m.focused()) == Some(self.search_input_id());
        let focused = canvas::text_input_focused(ctx);
        SearchContext {
            canvas: self.canvas,
            viewport_available: canvas::valid_rect(self.viewport) && self.viewport.width() > 0.0 && self.viewport.height() > 0.0,
            blocked: egui::Popup::is_any_open(ctx)
                || (focused && !own_focus)
                || (self.text_focused && !own_focus && !self.search_focused)
                || self.drag.is_some()
                || self.marquee.is_some()
                || ctx.dragged_id().is_some()
                || self.navigation.pointer_owned,
            open: self.search.open,
            has_query: !self.search.query.is_empty()
                && self.search.draft.len() <= MAX_QUERY_BYTES
                && self.search.draft.trim() == self.search.query
                && (self.search.scanning || !self.search.results.is_empty()),
        }
    }
    /// Fresh enabled query for the live search host. Raw/headless Session calls are Unsupported.
    pub fn search_enabled(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<(), Error> {
        if !search_id(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a canvas search command"));
        }
        let context = self.search_context(ctx);
        let previous = self.session.swap_resource(Some(context));
        let result = self.session.enabled(id, params);
        self.session.swap_resource(previous);
        result
    }
    /// Apply search state immediately through the shared service path. Scans progress once per
    /// rendered frame; an unfinished scan queues one generation-specific navigation request.
    pub fn dispatch_search(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<Json, Error> {
        if !search_id(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a canvas search command"));
        }
        let context = self.search_context(ctx);
        *self.search_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "search host is busy"))? = None;
        let previous = self.session.swap_resource(Some(context));
        let result = self.session.dispatch(id, params);
        self.session.swap_resource(previous);
        let intent = self.search_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "search host is busy"))?.take();
        result?;
        let intent = intent.ok_or_else(|| Error::new(ErrorKind::Conflict, "search host did not acknowledge the command"))?;
        match intent.get("action").and_then(Json::as_str) {
            Some("open") => {
                self.search.open = true;
                ctx.memory_mut(|m| m.request_focus(self.search_input_id()));
            }
            Some("close") => {
                self.search.close();
                ctx.memory_mut(|m| m.surrender_focus(self.search_input_id()));
            }
            Some("query") => self
                .search
                .set_query(intent.get("query").and_then(Json::as_str).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "search query is unavailable"))?)?,
            Some("next") => self.search.advance(Direction::Next),
            Some("previous") => self.search.advance(Direction::Previous),
            _ => return Err(Error::new(ErrorKind::InvalidInput, "unknown search intent")),
        }
        self.apply_search_frame(ctx);
        ctx.request_repaint();
        Ok(json!({"open":self.search.open,"scanning":self.search.scanning,"matches":self.search.results.len(),"current":self.search.current}))
    }
    fn apply_search_frame(&mut self, ctx: &egui::Context) {
        if !self.search.open
            || self.search.frame_request.is_none()
            || self.search_context(ctx).blocked
            || self.search.draft.len() > MAX_QUERY_BYTES
            || self.search.draft.trim() != self.search.query
        {
            return;
        }
        let Some(id) = self.search.frame_request.take() else {
            return;
        };
        self.selection.only(id);
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        let Some(body) = self.navigation.geometry.rects.get(&id).copied() else {
            self.notice = Some("Search result selected; its instance geometry is unavailable.".into());
            ctx.request_repaint();
            return;
        };
        let t = self.theme.tokens();
        let mut usable = self.navigation.usable(self.viewport, t);
        if self.search.open {
            usable.min.y = self.search_rect().bottom() + t.navigation_gap;
        }
        let mut next = self.view;
        let bounds = crate::navigation::decorated(body, t);
        let padding = t.fit_padding + t.title_min_screen_px + t.handle_min_screen_radius;
        if let Some(result) = next.fit(usable, bounds, padding) {
            // CanvasView::fit uses its viewport's top-left as origin; the real canvas origin
            // stays unchanged when reserving a top strip for the search overlay.
            next.pan += usable.min - self.viewport.min;
            if self.view.zoom.is_finite() && self.view.zoom >= canvas::MIN_ZOOM && self.view.zoom <= next.zoom {
                // A fitting card only needs the smallest translation into the unobscured area.
                let screen = self.view.rect_to_screen(self.viewport.min, bounds);
                let target = usable.shrink(padding);
                let shift = |min: f32, max: f32, low: f32, high: f32| {
                    if min < low {
                        low - min
                    } else if max > high {
                        high - max
                    } else {
                        0.0
                    }
                };
                next = self.view;
                next.pan +=
                    Vec2::new(shift(screen.min.x, screen.max.x, target.min.x, target.max.x), shift(screen.min.y, screen.max.y, target.min.y, target.max.y));
            }
            if next.pan.is_finite() {
                self.view = next;
                self.notice = result.clamped.then(|| "Search result centered at minimum zoom; complete containment is unavailable.".into());
            }
        } else {
            self.notice = Some("Search result selected; the canvas is too small to frame it.".into());
        }
        ctx.request_repaint();
    }
    pub(super) fn search_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        if !self.search_context(&ctx).blocked {
            let open = ui.input_mut(|i| consume_shortcut(i, "Command+F") || consume_shortcut(i, "Ctrl+F"));
            if open && let Err(error) = self.dispatch_search(&ctx, "canvas.search_open", &json!({})) {
                self.notice = Some(error.message);
            }
            if self.search.open
                && !self.search_context(&ctx).blocked
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
                && let Err(error) = self.dispatch_search(&ctx, "canvas.search_close", &json!({}))
            {
                self.notice = Some(error.message);
            }
        }
        let rect = self.search_rect();
        let open_enabled = self.search_enabled(&ctx, "canvas.search_open", &json!({})).is_ok();
        let step_enabled = self.search_enabled(&ctx, "canvas.search_next", &json!({})).is_ok();
        let close_enabled = self.search_enabled(&ctx, "canvas.search_close", &json!({})).is_ok();
        let t = self.theme.tokens();
        let mut action = None;
        let mut changed = false;
        let mut enter = None;
        let id = self.search_input_id();
        egui::Area::new(ui.id().with("canvas_search_overlay")).order(egui::Order::Foreground).movable(false).fixed_pos(rect.min).constrain(false).show(
            &ctx,
            |ui| {
                ui.set_min_size(rect.size());
                if !self.search.open {
                    if ui
                        .add_enabled_ui(open_enabled, |ui| {
                            ui.put(rect, egui::Button::new(if cfg!(target_os = "macos") { "Search (⌘F)" } else { "Search (Ctrl+F)" }))
                        })
                        .inner
                        .on_hover_text("Finish any active node editor or gesture before opening canvas search.")
                        .clicked()
                    {
                        action = Some("canvas.search_open");
                    }
                    return;
                }
                ui.painter().rect(rect, t.panel_radius, t.panel_fill, egui::Stroke::new(t.handle_stroke, t.panel_stroke), egui::StrokeKind::Inside);
                let inner = rect.shrink(t.navigation_gap);
                let label = ui.put(Rect::from_min_size(inner.min, Vec2::new(inner.width(), t.body_size + t.control_gap)), egui::Label::new("Search canvas"));
                let input =
                    Rect::from_min_size(Pos2::new(inner.min.x, label.rect.bottom() + t.navigation_gap), Vec2::new(inner.width(), t.navigation_toolbar_height));
                let response = ui
                    .put(
                        input,
                        egui::TextEdit::singleline(&mut self.search.draft)
                            .id(id)
                            .char_limit(MAX_QUERY_BYTES.saturating_add(1))
                            .hint_text("Title, prompt, stored model or tag"),
                    )
                    .labelled_by(label.id);
                changed = response.changed();
                if response.has_focus() || response.lost_focus() {
                    let direction = ui.input_mut(|i| {
                        if i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter) {
                            Some(Direction::Previous)
                        } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                            Some(Direction::Next)
                        } else {
                            None
                        }
                    });
                    if direction.is_some() {
                        response.request_focus();
                        enter = direction;
                    }
                }
                let row = Rect::from_min_size(Pos2::new(inner.min.x, input.bottom() + t.navigation_gap), Vec2::new(inner.width(), t.navigation_toolbar_height));
                let width = row.width() / 3.0;
                for (index, (label, id, enabled)) in [
                    ("Previous", "canvas.search_previous", step_enabled),
                    ("Next", "canvas.search_next", step_enabled),
                    ("Close search", "canvas.search_close", close_enabled),
                ]
                .into_iter()
                .enumerate()
                {
                    let button = Rect::from_min_size(row.min + Vec2::new(index as f32 * width, 0.0), Vec2::new(width, row.height()));
                    if ui.add_enabled_ui(enabled, |ui| ui.put(button, egui::Button::new(label))).inner.clicked() {
                        action = Some(id);
                    }
                }
                let status = if self.search.draft.len() > MAX_QUERY_BYTES {
                    "Query exceeds 1024 UTF-8 bytes".to_owned()
                } else if self.search.scanning {
                    format!("Searching… {} nodes checked, {} matches", self.search.scanned_nodes, self.search.results.len())
                } else if let Some(index) = self.search.current.and_then(|id| self.search.results.iter().position(|candidate| *candidate == id)) {
                    format!("{} of {} matches", index + 1, self.search.results.len())
                } else {
                    format!("{} matches", self.search.results.len())
                };
                let status_rect =
                    Rect::from_min_size(Pos2::new(inner.min.x, row.bottom() + t.navigation_gap), Vec2::new(inner.width(), t.body_size + t.control_gap));
                let response =
                    ui.put(status_rect, egui::Label::new(egui::RichText::new(status).size(t.body_size).color(t.text_muted)).halign(egui::Align::LEFT));
                ctx.accesskit_node_builder(response.id, |node| {
                    node.set_live(egui::accesskit::Live::Polite);
                    node.set_live_atomic();
                });
            },
        );
        let mut query_ok = true;
        if changed {
            let query = self.search.draft.clone();
            if let Err(error) = self.dispatch_search(&ctx, "canvas.search_query", &json!({"query":query})) {
                self.notice = Some(error.message);
                query_ok = false;
            }
        }
        if query_ok && let Some(direction) = enter {
            action = Some(if direction == Direction::Previous { "canvas.search_previous" } else { "canvas.search_next" });
        }
        if let Some(action) = action
            && let Err(error) = self.dispatch_search(&ctx, action, &json!({}))
        {
            self.notice = Some(error.message);
        }
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        let was_scanning = self.search.scanning;
        self.search.tick(self.session.document(), self.canvas, self.session.revision(), &self.navigation.geometry.labels);
        self.apply_search_frame(&ctx);
        if self.search.open && (was_scanning || self.search.scanning) {
            ctx.request_repaint();
        }
        let gesture = self.drag.is_some() || self.marquee.is_some() || ctx.dragged_id().is_some();
        !gesture && ui.input(|i| i.pointer.interact_pos().is_some_and(|p| rect.contains(p)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };
    fn app() -> NodeCraftApp {
        let base = NodeCraftApp::headless().unwrap();
        let canvas = base.canvas;
        let mut document = base.session.document().clone();
        let original = document.canvases().get(&canvas).unwrap().nodes().values().next().unwrap().clone();
        for (n, text, x) in [(3, "alpha", -2000.0), (4, "needle", 2000.0)] {
            let mut node = original.clone();
            node.id = NodeId::from_bytes([n; 16]);
            node.title = format!("Card {n}");
            node.pos.x = x;
            node.params.insert("text".into(), Value::Text(text.into()));
            let change = if n == 3 { Change::SetNode { canvas, node } } else { Change::AddNode { canvas, node } };
            change.apply(&mut document).unwrap();
        }
        NodeCraftApp::from_session(Session::new(document).unwrap(), canvas).unwrap()
    }
    fn harness() -> Harness<'static, NodeCraftApp> {
        Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app())
    }
    fn open(h: &mut Harness<'static, NodeCraftApp>) {
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        h.run();
        assert!(h.state().search.open);
    }
    #[test]
    fn typing_and_enter_same_frame_uses_new_query_and_escape_keeps_selection_and_history() {
        let mut h = harness();
        let original = h.state().session.document().clone();
        open(&mut h);
        h.event(egui::Event::Text("alpha".into()));
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().search.current, Some(NodeId::from_bytes([3; 16])));
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("needle".into()));
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().search.query, "needle");
        assert_eq!(h.state().search.current, Some(NodeId::from_bytes([4; 16])));
        assert!(h.state().selection.contains(NodeId::from_bytes([4; 16])));
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"Card"})).unwrap();
        h.run();
        let status = h.get_by_label("1 of 2 matches").rect();
        assert!(h.state().search_rect().contains_rect(status), "accessible match count stays inside its panel");
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Enter);
        h.run();
        assert_eq!(h.state().search.current, Some(NodeId::from_bytes([4; 16])));
        h.get_by_role_and_label(Role::Button, "Next").click();
        h.run();
        assert_eq!(h.state().search.current, Some(NodeId::from_bytes([3; 16])));
        // Escape must belong to search even after Next moved focus to its button.
        let selection = h.state().selection.clone();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().search.open);
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.document(), &original);
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn unrelated_dirty_editor_keeps_focus_and_draft_when_search_shortcut_is_pressed() {
        let mut h =
            Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        let original = h.state().session.document().clone();
        h.get_by_role_and_label(Role::TextInput, "Text").click();
        h.run();
        h.event(egui::Event::Text("dirty uncommitted".into()));
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        h.run();
        assert!(!h.state().search.open);
        assert!(canvas::text_input_focused(&h.ctx));
        assert_eq!(h.state().session.document(), &original);
        assert!(!h.state().session.can_undo());
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(h.state().session.document(), &original);
    }
    #[test]
    fn scoped_search_resource_restores_on_errors_and_preserves_unrelated_state() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        assert!(h.state_mut().session.dispatch("canvas.search_open", &json!({})).is_err());
        let prior = SearchContext { canvas: h.state().canvas, viewport_available: false, blocked: true, open: false, has_query: false };
        h.state_mut().session.swap_resource(Some(prior));
        h.state_mut().session.swap_resource(Some(99_u32));
        open(&mut h);
        assert!(h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"é".repeat(513)})).is_err());
        let restored = h.state_mut().session.swap_resource::<SearchContext>(None).unwrap();
        assert!(restored.blocked);
        assert!(!restored.viewport_available);
        assert_eq!(h.state_mut().session.swap_resource::<u32>(None), Some(99));
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn popup_and_existing_box_gesture_exclude_search_opening() {
        let mut h = harness();
        let from = Pos2::new(40.0, 40.0);
        h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
        h.run();
        h.hover_at(Pos2::new(100.0, 100.0));
        h.run();
        assert!(h.state().marquee.is_some());
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        h.run();
        assert!(!h.state().search.open);
        assert!(h.state().marquee.is_some());
        h.event(egui::Event::PointerButton {
            pos: Pos2::new(100.0, 100.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        egui::Popup::open_id(&h.ctx, egui::Id::new("search_popup_test"));
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F);
        h.run();
        assert!(!h.state().search.open);
    }
    #[test]
    fn full_prompt_tail_is_found_incrementally_and_close_cancels_pending_frame() {
        let mut base = app();
        let canvas = base.canvas;
        let mut document = base.session.document().clone();
        let mut node = document.canvases().get(&canvas).unwrap().nodes().get(&NodeId::from_bytes([4; 16])).unwrap().clone();
        node.params.insert("text".into(), Value::Text("x".repeat(1_048_550) + " needle-tail"));
        Change::SetNode { canvas, node }.apply(&mut document).unwrap();
        base = NodeCraftApp::from_session(Session::new(document).unwrap(), canvas).unwrap();
        let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), base);
        let original = h.state().session.document().clone();
        open(&mut h);
        h.event(egui::Event::Text("needle-tail".into()));
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(!h.state().search.scanning);
        assert_eq!(h.state().search.current, Some(NodeId::from_bytes([4; 16])));
        assert_eq!(h.state().session.document(), &original);
        assert_eq!(h.state().session.revision(), 0);
        let ctx = h.ctx.clone();
        h.state_mut().selection.only(NodeId::from_bytes([3; 16]));
        let selection = h.state().selection.clone();
        // Harness processes each modifier/key event as a frame. Select the draft first,
        // then paste during the newly queued scan rather than after those input frames.
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.run();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"needle"})).unwrap();
        h.state_mut().dispatch_search(&ctx, "canvas.search_next", &json!({})).unwrap();
        assert_eq!(h.state().selection, selection, "pending command must not select before a scan");
        assert!(h.state().search.scanning);
        h.event(egui::Event::Text("é".repeat(513)));
        h.run();
        assert!(h.state().search.draft.len() > MAX_QUERY_BYTES);
        assert_eq!(h.state().search.query, "needle");
        assert_eq!(h.state().selection, selection, "invalid visible draft must not frame accepted old-query results");
        assert!(!h.state().search.scanning);
        h.state_mut().dispatch_search(&ctx, "canvas.search_close", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().selection, selection);
        assert!(h.state().search.frame_request.is_none());
    }
    #[test]
    fn dynamic_offscreen_fallback_label_frames_actual_shape_and_unknown_text_still_matches() {
        use nodecraft_nodes::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx};
        use nodecraft_types::{ParamControl, ParamSchema, ParamSpec, PortType};
        struct Kind(NodeDescriptor, NodeDescriptor);
        impl NodeKind for Kind {
            fn descriptor(&self) -> &NodeDescriptor {
                &self.0
            }
            fn instance<'a>(&'a self, _: &nodecraft_graph::Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
                Ok(Cow::Borrowed(&self.1))
            }
            fn mode(&self, _: &[PortType]) -> Option<ModeId> {
                None
            }
            fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
                Ok(vec![])
            }
        }
        let short = NodeDescriptor {
            id: "test.dynamic".into(),
            label: "Static".into(),
            category: Category::Source,
            inputs: vec![],
            outputs: vec![],
            params: ParamSchema { params: vec![] },
            quick_add: None,
        };
        let tall = NodeDescriptor {
            label: "Dynamic needle".into(),
            params: ParamSchema {
                params: (0..8)
                    .map(|n| ParamSpec {
                        name: format!("p{n}"),
                        label: format!("P{n}"),
                        description: None,
                        control: ParamControl::Toggle,
                        required: false,
                        default: Some(Value::Bool(false)),
                        visible_if: None,
                    })
                    .collect(),
            },
            ..short.clone()
        };
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut document = Project::new(ProjectId::from_bytes([2; 16]), "Search shapes").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let mut node = nodecraft_graph::Node::new(NodeId::from_bytes([3; 16]), "test.dynamic");
        node.pos.x = 100_000.0;
        Change::AddNode { canvas, node: node.clone() }.apply(&mut document).unwrap();
        let mut unknown = nodecraft_graph::Node::new(NodeId::from_bytes([4; 16]), "test.unavailable");
        unknown.title = "Unavailable card".into();
        unknown.pos.x = -100_000.0;
        unknown.config.insert("provider.model".into(), Value::Text("unknown model tail".into()));
        Change::AddNode { canvas, node: unknown }.apply(&mut document).unwrap();
        let mut kinds = nodecraft_nodes::Registry::default();
        kinds.register(Box::new(Kind(short.clone(), tall))).unwrap();
        let session = Session::with_registries(document, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap();
        let mut h = Harness::builder()
            .with_size(Vec2::new(1000.0, 700.0))
            .with_max_steps(160)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::from_session(session, canvas).unwrap());
        open(&mut h);
        h.event(egui::Event::Text("dynamic needle".into()));
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(h.state().search.current, Some(node.id));
        let actual = h.state().navigation.geometry.rects[&node.id];
        assert!(actual.height() > card::world_rect(&node, Some(&short), h.state().theme.tokens()).height());
        let screen = h.state().view.rect_to_screen(h.state().viewport.min, crate::navigation::decorated(actual, h.state().theme.tokens()));
        assert!(screen.top() > h.state().search_rect().bottom());
        assert!(screen.bottom() < h.state().navigation.usable(h.state().viewport, h.state().theme.tokens()).bottom());
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"unknown model tail"})).unwrap();
        h.run();
        assert_eq!(h.state().search.current, Some(NodeId::from_bytes([4; 16])));
        assert!(h.state().navigation.geometry.rects.contains_key(&NodeId::from_bytes([4; 16])));
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn query_limit_accepts_exact_bytes_but_ascii_and_multibyte_overflow_remain_invalid() {
        let mut h = harness();
        open(&mut h);
        h.event(egui::Event::Text("x".repeat(MAX_QUERY_BYTES)));
        h.run();
        assert_eq!(h.state().search.query.len(), MAX_QUERY_BYTES);
        for invalid in ["x".repeat(MAX_QUERY_BYTES + 1), "é".repeat(513)] {
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
            h.event(egui::Event::Text(invalid));
            h.run();
            assert!(h.state().search.draft.len() > MAX_QUERY_BYTES);
            assert_eq!(h.state().search.query, "x".repeat(MAX_QUERY_BYTES));
            let ctx = h.ctx.clone();
            assert!(h.state_mut().search_enabled(&ctx, "canvas.search_next", &json!({})).is_err());
        }
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn passive_title_edits_do_not_change_selection_or_camera() {
        let mut h = harness();
        open(&mut h);
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"Card 4"})).unwrap();
        h.run();
        h.state_mut().selection.only(NodeId::from_bytes([3; 16]));
        let selection = h.state().selection.clone();
        let view = h.state().view;
        let canvas = h.state().canvas;
        for (id, title) in [(4, "absent"), (3, "Card 4"), (3, "still Card 4")] {
            h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":NodeId::from_bytes([id;16]),"title":title})).unwrap();
            let document = h.state().session.document().clone();
            let revision = h.state().session.revision();
            h.run();
            assert_eq!(h.state().view, view);
            assert_eq!(h.state().selection, selection);
            assert!(h.state().search.frame_request.is_none());
            assert!(h.state().search.current.is_none());
            assert_eq!(h.state().session.document(), &document);
            assert_eq!(h.state().session.revision(), revision);
            assert!(h.state().session.can_undo());
        }
    }
    #[test]
    fn framing_preserves_zoom_and_only_pans_when_needed() {
        let mut h = harness();
        open(&mut h);
        let ctx = h.ctx.clone();
        h.state_mut().view.zoom = 0.25;
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"needle"})).unwrap();
        h.run();
        assert_eq!(h.state().view.zoom, 0.25);
        let view = h.state().view;
        h.state_mut().dispatch_search(&ctx, "canvas.search_next", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().view, view);
        assert_eq!(h.state().session.revision(), 0);
        assert!(!h.state().session.can_undo());
    }
    #[test]
    fn blocked_search_does_not_consume_shortcut_or_add_notice() {
        for owner in 0..3 {
            let retained = Rc::new(RefCell::new(false));
            let observed = Rc::clone(&retained);
            let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(
                move |ui, app: &mut NodeCraftApp| {
                    match owner {
                        0 => egui::Popup::open_id(ui.ctx(), egui::Id::new("search_shortcut_owner")),
                        1 => app.marquee = Some(Marquee { start: Pos2::ZERO, base: BTreeSet::new(), cancelled: false }),
                        _ => app.text_focused = true,
                    }
                    app.viewport = ui.max_rect();
                    app.search_overlay(ui);
                    *observed.borrow_mut() |=
                        ui.input(|input| input.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::F, pressed: true, .. })));
                },
                app(),
            );
            let document = h.state().session.document().clone();
            let view = h.state().view;
            let selection = h.state().selection.clone();
            h.event(egui::Event::Key {
                key: egui::Key::F,
                physical_key: Some(egui::Key::F),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            });
            h.run();
            assert!(*retained.borrow());
            assert!(!h.state().search.open);
            assert!(h.state().notice.is_none());
            assert_eq!(h.state().view, view);
            assert_eq!(h.state().selection, selection);
            assert_eq!(h.state().session.document(), &document);
            assert_eq!(h.state().session.revision(), 0);
            assert!(!h.state().session.can_undo());
        }
    }
    #[test]
    fn result_status_is_polite_and_stable_across_unchanged_frames() {
        let mut h = harness();
        open(&mut h);
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"needle"})).unwrap();
        h.run();
        let status = h.get_by_label("1 of 1 matches");
        assert_eq!(status.accesskit_node().live(), egui::accesskit::Live::Polite);
        assert!(status.accesskit_node().is_live_atomic());
        let id = status.accesskit_node().id();
        h.run();
        assert_eq!(h.get_by_label("1 of 1 matches").accesskit_node().id(), id);
    }
    #[test]
    fn passive_deletion_and_edit_during_partial_scan_leave_view_and_history_alone() {
        let mut h = harness();
        open(&mut h);
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"Card 4"})).unwrap();
        h.run();
        h.state_mut().selection.only(NodeId::from_bytes([3; 16]));
        let selection = h.state().selection.clone();
        let view = h.state().view;
        let canvas = h.state().canvas;
        h.state_mut().session.dispatch("node.remove", &json!({"canvas":canvas,"id":NodeId::from_bytes([4;16])})).unwrap();
        let document = h.state().session.document().clone();
        h.run();
        assert!(h.state().search.current.is_none());
        assert_eq!(h.state().view, view);
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), 1);
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas":canvas,"id":NodeId::from_bytes([3;16]),"name":"text","value":Value::Text("x".repeat(900_000))}))
            .unwrap();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"absent"})).unwrap();
        h.step();
        assert!(h.state().search.scanning);
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":NodeId::from_bytes([3;16]),"title":"absent"})).unwrap();
        let document = h.state().session.document().clone();
        let revision = h.state().session.revision();
        h.run();
        assert!(h.state().search.current.is_none());
        assert_eq!(h.state().view, view);
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.document(), &document);
        assert_eq!(h.state().session.revision(), revision);
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(h.state().session.document().canvases()[&canvas].nodes()[&NodeId::from_bytes([3; 16])].title, "Card 3");
    }
}
