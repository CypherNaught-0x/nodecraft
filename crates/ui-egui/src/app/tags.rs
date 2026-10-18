//! Tagging menus and scoped view-only filter host.
use super::*;
use crate::tags::{PALETTE, label};
use nodecraft_engine::tag_filter_cmds::TagFilterContext;
use nodecraft_types::TagId;
use serde_json::Value as Json;
#[derive(Clone, Copy)]
pub(super) enum TagAction {
    Existing(TagId),
    Palette(usize),
    Clear,
}
fn is_command(id: &str) -> bool {
    matches!(id, "canvas.tag_filter_open" | "canvas.tag_filter_close" | "canvas.tag_filter_set" | "canvas.tag_filter_next" | "canvas.tag_filter_previous")
}
pub(super) fn tag_menu(ui: &mut egui::Ui, canvas: &Canvas, selection: &Selection, t: &theme::Tokens, order: &[TagId]) -> Option<TagAction> {
    let mut action = None;
    ui.menu_button("Tag selected nodes", |ui| {
        if ui.add_enabled(!selection.is_empty(), egui::Button::new("Clear tag")).clicked() {
            action = Some(TagAction::Clear);
            ui.close();
        }
        ui.label("Standard colors");
        for (index, (_, name)) in PALETTE.iter().enumerate() {
            if ui
                .add_enabled(!selection.is_empty(), egui::Button::new(egui::RichText::new(*name).color(t.tag_colors.get(index).copied().unwrap_or(t.text))))
                .clicked()
            {
                action = Some(TagAction::Palette(index));
                ui.close();
            }
        }
        ui.separator();
        ui.label("Existing tags");
        let mut counts: BTreeMap<TagId, usize> = BTreeMap::new();
        for id in selection.nodes() {
            if let Some(tag) = canvas.nodes().get(id).and_then(|n| n.tag) {
                *counts.entry(tag).or_default() += 1;
            }
        }
        // Sorted presentation retains IDs and at most 128 characters of each displayed title.

        egui::ScrollArea::vertical().id_salt("assign_tags").max_height(t.tag_panel_height).show_rows(ui, t.tag_row_height, order.len(), |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for (index, id) in order.get(range.clone()).unwrap_or(&[]).iter().enumerate() {
                let Some(tag) = canvas.tags().get(id) else {
                    continue;
                };
                let ordinal = range.start + index + 1;
                let count = counts.get(&tag.id).copied().unwrap_or(0);
                let state = if count == 0 {
                    ""
                } else if count == selection.len() {
                    " ✓"
                } else {
                    " (mixed)"
                };
                let suffix = format!("#{ordinal}");
                if ui
                    .add_enabled(
                        !selection.is_empty(),
                        egui::Button::new(egui::RichText::new(format!("{}{} · {}", suffix, state, label(&tag.title))).color(t.tag_color(tag.color.as_deref())))
                            .truncate(),
                    )
                    .clicked()
                {
                    action = Some(TagAction::Existing(tag.id));
                    ui.close();
                }
            }
        });
    });
    action
}
impl NodeCraftApp {
    fn sync_tag_session(&mut self) {
        let identity = self.session.view_identity();
        if self.tag_session_identity.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &identity)) {
            self.tag_filter = crate::tags::TagFilter::default();
            self.search = crate::search::Search::default();
            self.tag_focus_ids.clear();
            self.tag_focused = false;
            self.tag_focus_pending = false;
            self.tag_session_identity = Some(identity);
        }
    }
    pub(super) fn apply_tag_action(&mut self, action: TagAction) -> Result<(), Error> {
        if self.selection.is_empty() {
            return Err(Error::new(ErrorKind::Conflict, "select nodes before tagging"));
        }
        let mut create = None;
        let tag = match action {
            TagAction::Clear => None,
            TagAction::Existing(id) => Some(id),
            TagAction::Palette(index) => {
                let (color, title) = PALETTE.get(index).copied().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "unknown tag palette entry"))?;
                let canvas = self.session.document().canvases().get(&self.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas unavailable"))?;
                let existing = canvas.tags().values().find(|t| t.color.as_deref() == Some(color)).map(|t| t.id);
                Some(if let Some(id) = existing {
                    id
                } else {
                    let id = TagId::from_bytes(crate::ids::random_id(|bytes| {
                        self.session.document().canvases().values().any(|c| c.tags().contains_key(&TagId::from_bytes(*bytes)))
                    })?);
                    create = Some(json!({"id":id,"title":title,"color":color}));
                    id
                })
            }
        };
        let nodes: Vec<_> = self.selection.nodes().iter().copied().collect();
        let mut params = json!({"canvas":self.canvas,"nodes":nodes,"tag":tag});
        if let Some(create) = create {
            params["create"] = create;
        }
        self.session.dispatch("node.tag_set", &params)?;
        Ok(())
    }
    pub(super) fn tag_owns_focus(&self, ctx: &egui::Context) -> bool {
        self.tag_filter.open && ctx.memory(|m| m.focused()).map_or(self.tag_focused, |id| self.tag_focus_ids.contains(&id))
    }
    pub(super) fn tag_rect(&self) -> Rect {
        let t = self.theme.tokens();
        let available = (self.viewport.width() - 2.0 * t.navigation_margin).max(1.0);
        let left = if self.tag_filter.open && self.comments.open {
            let discussion_width = t.comments_width.min(((available - t.navigation_gap) / 2.0).max(1.0));
            self.viewport.min.x + t.navigation_margin + discussion_width + t.navigation_gap
        } else if self.tag_filter.open {
            self.viewport.min.x + t.navigation_margin
        } else {
            (self.search_rect().left() - t.navigation_gap - t.search_button_width).max(self.viewport.min.x + t.navigation_margin)
        };
        let width =
            if self.tag_filter.open { t.tag_panel_width } else { t.search_button_width }.min((self.viewport.max.x - t.navigation_margin - left).max(1.0));
        let top = if self.search.open { self.search_rect().bottom() + t.navigation_gap } else { self.viewport.min.y + t.navigation_margin };
        let height =
            if self.tag_filter.open { t.tag_panel_height } else { t.navigation_toolbar_height }.min((self.viewport.max.y - top - t.navigation_margin).max(1.0));
        Rect::from_min_size(Pos2::new(left, top), Vec2::new(width, height))
    }
    fn tag_context(&mut self, ctx: &egui::Context) -> TagFilterContext {
        self.sync_tag_session();
        self.tag_filter.sync(self.session.document(), self.canvas, self.session.revision());
        TagFilterContext {
            canvas: self.canvas,
            viewport_available: canvas::valid_rect(self.viewport) && self.viewport.width() > 0.0 && self.viewport.height() > 0.0,
            blocked: egui::Popup::is_any_open(ctx)
                || canvas::text_input_focused(ctx)
                || (self.text_focused && !self.tag_owns_focus(ctx))
                || self.drag.is_some()
                || self.marquee.is_some()
                || self.comments.drag.is_some()
                || self.comments.gesture_cancelled
                || ctx.dragged_id().is_some()
                || self.navigation.pointer_owned
                || self.navigation.drag.is_some()
                || self.palette.is_some()
                || self.turn_into.is_some()
                || self.reordering.is_some()
                || self.wiring.is_some()
                || ctx.input(|i| !i.raw.hovered_files.is_empty() || !i.raw.dropped_files.is_empty()),
            open: self.tag_filter.open,
            active: self.tag_filter.active(),
            scanning: self.tag_filter.scanning,
            matches: self.tag_filter.results.len(),
        }
    }
    pub fn tag_filter_enabled(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<(), Error> {
        if !is_command(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a tag filter command"));
        }
        let context = self.tag_context(ctx);
        let prior = self.session.swap_resource(Some(context));
        let result = self.session.enabled(id, params);
        self.session.swap_resource(prior);
        result
    }
    pub fn dispatch_tag_filter(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<Json, Error> {
        if !is_command(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a tag filter command"));
        }
        let context = self.tag_context(ctx);
        *self.tag_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "tag host busy"))? = None;
        let prior = self.session.swap_resource(Some(context));
        let result = self.session.dispatch(id, params);
        self.session.swap_resource(prior);
        let intent = self.tag_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "tag host busy"))?.take();
        result?;
        let intent = intent.ok_or_else(|| Error::new(ErrorKind::Conflict, "tag host did not acknowledge request"))?;
        match intent.get("action").and_then(Json::as_str) {
            Some("open") => {
                self.tag_filter.open = true;
                self.tag_focus_pending = true;
            }
            Some("close") => {
                let owned = self.tag_owns_focus(ctx);
                self.tag_filter.open = false;
                self.tag_filter.cancel_navigation();
                self.tag_focus_pending = false;
                let id = ctx.memory(|m| m.focused());
                if owned && let Some(id) = id {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
            Some("set") => {
                let tags: BTreeSet<TagId> =
                    serde_json::from_value(intent.get("tags").cloned().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing filter tags"))?)
                        .map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid filter tags"))?;
                self.tag_filter.set(tags, intent.get("untagged").and_then(Json::as_bool).unwrap_or(false));
            }
            Some("next") => {
                self.search.cancel_navigation();
                self.tag_filter.advance(false);
            }
            Some("previous") => {
                self.search.cancel_navigation();
                self.tag_filter.advance(true);
            }
            _ => return Err(Error::new(ErrorKind::InvalidInput, "unknown tag filter intent")),
        }
        self.apply_tag_frame(ctx);
        ctx.request_repaint();
        Ok(json!({"open":self.tag_filter.open,"scanning":self.tag_filter.scanning,"matches":self.tag_filter.results.len(),"current":self.tag_filter.current}))
    }
    fn apply_tag_frame(&mut self, ctx: &egui::Context) {
        if self.tag_filter.open && ctx.input(|i| i.pointer.primary_down() && i.pointer.interact_pos().is_some_and(|p| self.tag_rect().contains(p))) {
            return;
        }
        if self.tag_context(ctx).blocked {
            return;
        }
        if let Some(id) = self.tag_filter.frame_request.take() {
            self.frame_canvas_node(ctx, id, "Tagged node");
        }
    }
    pub(super) fn tag_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        self.sync_tag_session();
        self.tag_filter.tick(self.session.document(), self.canvas, self.session.revision());
        if self.tag_filter.scanning || self.tag_filter.indexing {
            ctx.request_repaint();
        }
        let rect = self.tag_rect();
        let t = self.theme.tokens();
        let allowed = !self.tag_context(&ctx).blocked;
        if !allowed {
            self.tag_focus_pending = false;
        }
        let steps = allowed && self.tag_filter.active() && (self.tag_filter.scanning || !self.tag_filter.results.is_empty());
        let mut action = None;
        let mut filter = None;
        let mut focus_ids = Vec::new();
        let mut control_rects = Vec::new();
        if self.tag_filter.open && self.tag_owns_focus(&ctx) && allowed {
            action = ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                    Some("canvas.tag_filter_close")
                } else if i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter) {
                    Some("canvas.tag_filter_previous")
                } else if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    Some("canvas.tag_filter_next")
                } else {
                    None
                }
            });
        }
        let area =
            egui::Area::new(ui.id().with("tag_filter_overlay")).order(egui::Order::Foreground).fixed_pos(rect.min).movable(false).constrain(false).show(
                &ctx,
                |ui| {
                    crate::comments::checkbox_visuals(ui.visuals_mut(), t);
                    ui.set_min_size(rect.size());
                    ui.set_max_size(rect.size());
                    ui.set_clip_rect(rect);
                    if !self.tag_filter.open {
                        let response = ui.add_enabled(
                            allowed,
                            egui::Button::new(if self.tag_filter.active() {
                                format!("Tags · {} matches", self.tag_filter.results.len())
                            } else {
                                "Tag filter".into()
                            }),
                        );
                        focus_ids.push(response.id);
                        control_rects.push(response.rect);
                        if response.clicked() {
                            action = Some("canvas.tag_filter_open");
                        }
                        return;
                    }
                    ui.painter().rect(rect, t.panel_radius, t.panel_fill, egui::Stroke::new(t.handle_stroke, t.panel_stroke), egui::StrokeKind::Inside);
                    ui.add_space(t.navigation_gap);
                    let header = ui.add_enabled(allowed, egui::Button::new("Tag filter · highlight matches"));
                    focus_ids.push(header.id);
                    control_rects.push(header.rect);
                    if allowed && (self.tag_focus_pending || header.clicked()) {
                        header.request_focus();
                        self.tag_focus_pending = false;
                    }
                    ui.horizontal(|ui| {
                        for (label, id, enabled) in [
                            ("Previous tag", "canvas.tag_filter_previous", steps),
                            ("Next tag", "canvas.tag_filter_next", steps),
                            ("Close tags", "canvas.tag_filter_close", allowed),
                        ] {
                            let r = ui.add_enabled(enabled, egui::Button::new(label));
                            focus_ids.push(r.id);
                            control_rects.push(r.rect);
                            if r.clicked() {
                                action = Some(id);
                            }
                        }
                    });
                    let r = ui.add_enabled(allowed, egui::Button::new("Clear tag filter"));
                    focus_ids.push(r.id);
                    control_rects.push(r.rect);
                    if r.clicked() {
                        filter = Some((BTreeSet::new(), false));
                    }
                    let mut untagged = self.tag_filter.untagged;
                    let r = ui.add_enabled(allowed, egui::Checkbox::new(&mut untagged, "Untagged"));
                    focus_ids.push(r.id);
                    control_rects.push(r.rect);
                    if r.changed() {
                        filter = Some((self.tag_filter.tags.clone(), untagged));
                    }
                    let status = if self.tag_filter.scanning {
                        format!("Scanning… {} matches", self.tag_filter.results.len())
                    } else {
                        format!("{} tag matches", self.tag_filter.results.len())
                    };
                    ui.label(status);
                    if let Some(canvas) = self.session.document().canvases().get(&self.canvas) {
                        egui::ScrollArea::vertical()
                            .id_salt("filter_tags")
                            .max_height((rect.bottom() - ui.cursor().min.y - t.navigation_gap).max(1.0))
                            .show_rows(ui, t.tag_row_height, self.tag_filter.presentation.len(), |ui, range| {
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                for (index, id) in self.tag_filter.presentation.get(range.clone()).unwrap_or(&[]).iter().enumerate() {
                                    let Some(tag) = canvas.tags().get(id) else {
                                        continue;
                                    };
                                    let ordinal = range.start + index + 1;
                                    let mut selected = self.tag_filter.tags.contains(&tag.id);
                                    let suffix = format!("#{ordinal}");
                                    let r = ui.add_enabled(
                                        allowed,
                                        egui::Checkbox::new(
                                            &mut selected,
                                            egui::RichText::new(format!("{} · {}", suffix, label(&tag.title))).color(t.tag_color(tag.color.as_deref())),
                                        ),
                                    );
                                    focus_ids.push(r.id);
                                    control_rects.push(r.rect);
                                    if r.changed() {
                                        let mut tags = self.tag_filter.tags.clone();
                                        if selected {
                                            tags.insert(tag.id);
                                        } else {
                                            tags.remove(&tag.id);
                                        }
                                        filter = Some((tags, untagged));
                                    }
                                }
                            });
                    }
                },
            );
        if !allowed
            && self.text_focused
            && !egui::Popup::is_any_open(&ctx)
            && let Some(owner) = self.last_text_focus
            && ctx.memory(|m| m.focused()).is_none_or(|id| id == owner)
            && ui.input(|i| i.pointer.any_pressed() || i.pointer.any_released())
            && let Some(pointer) = ui.input(|i| i.pointer.interact_pos())
            && (control_rects.iter().any(|r| r.contains(pointer)) || self.tag_filter.open && rect.contains(pointer))
            && ctx.layer_id_at(pointer) == Some(area.response.layer_id)
        {
            ctx.memory_mut(|m| m.request_focus(owner));
            ctx.options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        self.tag_focus_ids = focus_ids;
        if let Some((tags, untagged)) = filter
            && let Err(e) = self.dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":tags,"untagged":untagged}))
        {
            self.notice = Some(e.message);
        }
        if let Some(action) = action
            && let Err(e) = self.dispatch_tag_filter(&ctx, action, &json!({}))
        {
            self.notice = Some(e.message);
        }
        self.apply_tag_frame(&ctx);
        ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| rect.contains(p) && ctx.layer_id_at(p) == Some(area.response.layer_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable};
    fn harness() -> Harness<'static, NodeCraftApp> {
        Harness::builder()
            .with_step_dt(1.0 / 60.0)
            .with_size(Vec2::new(1100.0, 800.0))
            .with_max_steps(160)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap())
    }
    #[test]
    fn focused_tag_header_hands_command_f_to_search() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.run();
        h.get_by_role_and_label(Role::Button, "Tag filter · highlight matches").click();
        h.run();
        assert!(h.state().tag_owns_focus(&ctx));
        h.event(egui::Event::Key { key: egui::Key::F, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND });
        h.run();
        assert!(h.state().search.open, "{:?}", h.state().notice);
        assert_eq!(ctx.memory(|m| m.focused()), Some(h.state().search_input_id()));
    }
    #[test]
    fn palette_ids_are_secure_and_entropy_failure_leaves_document_intact() {
        let mut h = harness();
        let id = *h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().keys().next().unwrap();
        h.state_mut().selection.only(id);
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        crate::ids::FAIL_ENTROPY.with(|v| v.set(true));
        let result = h.state_mut().apply_tag_action(TagAction::Palette(0));
        crate::ids::FAIL_ENTROPY.with(|v| v.set(false));
        assert!(result.is_err());
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), revision);
        assert!(h.state().selection.contains(id));
        h.state_mut().apply_tag_action(TagAction::Palette(0)).unwrap();
        let tag = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().get(&id).unwrap().tag.unwrap();
        assert_eq!(tag.as_bytes().get(6).unwrap() >> 4, 4);
        assert_eq!(tag.as_bytes().get(8).unwrap() >> 6, 2);
    }
    #[test]
    #[ignore = "manual public-safe PR evidence"]
    fn render_tag_review_evidence() {
        let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
        std::fs::create_dir_all(&folder).unwrap();
        let mut h = Harness::builder()
            .wgpu()
            .with_size(Vec2::new(1200.0, 800.0))
            .with_max_steps(160)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), sample());
        theme::apply(&h.ctx, ThemeKind::Dark);
        h.run();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        h.run();
        h.render().unwrap().save(folder.join("before.png")).unwrap();
        let ids: Vec<_> = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().keys().copied().take(2).collect();
        for id in ids {
            h.state_mut().selection.add(id);
        }
        h.state_mut().apply_tag_action(TagAction::Palette(2)).unwrap();
        h.run();
        let tag = *h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().keys().next().unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[tag],"untagged":false})).unwrap();
        h.run();
        h.render().unwrap().save(folder.join("dark.png")).unwrap();
        h.state_mut().theme = ThemeKind::Light;
        theme::apply(&ctx, ThemeKind::Light);
        h.run();
        h.render().unwrap().save(folder.join("light.png")).unwrap();
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().tags().is_empty());
        h.render().unwrap().save(folder.join("undo.png")).unwrap();
    }
    #[test]
    fn bulk_palette_assignment_is_one_undo_and_view_commands_preserve_history() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let original = h.state().session.document().clone();
        let id = *original.canvases().get(&h.state().canvas).unwrap().nodes().keys().next().unwrap();
        h.state_mut().selection.only(id);
        h.state_mut().apply_tag_action(TagAction::Palette(5)).unwrap();
        let revision = h.state().session.revision();
        let tagged = h.state().session.document().clone();
        let tag = tagged.canvases().get(&h.state().canvas).unwrap().nodes().get(&id).unwrap().tag.unwrap();
        h.run();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[tag],"untagged":false})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().tag_filter.current, Some(id));
        assert!(h.state().selection.contains(id));
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().session.document(), &tagged);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().tag_filter.open);
        assert!(h.state().tag_filter.active());
        assert!(h.state().selection.contains(id));
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(h.state().session.document(), &original);
    }
    #[test]
    fn actual_filter_button_opens_and_unrelated_editor_prevents_focus_theft() {
        let mut h = harness();
        h.get_by_role_and_label(Role::Button, "Tag filter").click();
        h.run();
        assert!(h.state().tag_filter.open);
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().tag_filter.open);
        h.get_by_role_and_label(Role::TextInput, "Text").click();
        h.run();
        h.event(egui::Event::Text("dirty draft".into()));
        h.run();
        let original = h.state().session.document().clone();
        let ctx = h.ctx.clone();
        assert!(h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).is_err());
        assert!(canvas::text_input_focused(&ctx));
        assert_eq!(h.state().session.document(), &original);
    }
    #[test]
    fn headless_commands_unsupported_and_host_resources_restore() {
        let mut h = harness();
        assert!(h.state_mut().session.dispatch("canvas.tag_filter_open", &json!({})).is_err());
        let ctx = h.ctx.clone();
        let prior =
            TagFilterContext { canvas: h.state().canvas, viewport_available: false, blocked: true, open: false, active: false, scanning: false, matches: 0 };
        h.state_mut().session.swap_resource(Some(prior));
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        let restored = h.state_mut().session.swap_resource::<TagFilterContext>(None).unwrap();
        assert!(restored.blocked);
        assert!(!restored.viewport_available);
    }
    fn press_at(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, button: egui::PointerButton) {
        h.hover_at(pos);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE });
            h.run();
        }
    }
    fn sample() -> NodeCraftApp {
        let base = NodeCraftApp::headless().unwrap();
        let c = base.canvas;
        let mut p = base.session.document().clone();
        let first = p.canvases().get(&c).unwrap().nodes().values().next().unwrap().clone();
        let mut changes = Vec::new();
        for (n, x) in [(3, -220.0), (4, 220.0)] {
            let mut node = first.clone();
            node.id = NodeId::from_bytes([n; 16]);
            node.pos.x = x;
            node.title = format!("Card {n}");
            node.params.insert("text".into(), Value::Text(format!("Public sample {n}")));
            changes.push(if n == 3 { Change::SetNode { canvas: c, node } } else { Change::AddNode { canvas: c, node } });
        }
        nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
        NodeCraftApp::from_session(Session::new(p).unwrap(), c).unwrap()
    }
    #[test]
    fn real_context_menu_tags_multiple_selected_cards_and_right_click_preserves_selection() {
        let mut h =
            Harness::builder().with_size(Vec2::new(1200.0, 800.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), sample());
        let ids = BTreeSet::from([NodeId::from_bytes([3; 16]), NodeId::from_bytes([4; 16])]);
        h.state_mut().selection.replace(ids.clone());
        h.run();
        let original = h.state().session.document().clone();
        let point = h.state().view.rect_to_screen(h.state().viewport.min, h.state().painted[&NodeId::from_bytes([3; 16])]).left_center() + Vec2::new(4.0, 0.0);
        press_at(&mut h, point, egui::PointerButton::Secondary);
        assert_eq!(h.state().selection.nodes(), &ids);
        h.get_by_label("Tag selected nodes ⏵").click();
        h.run();
        h.get_by_label("Blue").click();
        h.run();
        assert_eq!(h.state().session.revision(), 1);
        let c = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
        let tag = c.nodes().get(&NodeId::from_bytes([3; 16])).unwrap().tag;
        assert!(tag.is_some());
        assert_eq!(c.nodes().get(&NodeId::from_bytes([4; 16])).unwrap().tag, tag);
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(h.state().session.document(), &original);
    }
    #[test]
    fn actual_disabled_button_preserves_dirty_parameter_and_title_and_restores_focus_policy() {
        for title in [false, true] {
            let mut h = harness();
            if title {
                let rect = *h.state().painted.values().next().unwrap();
                let point = h.state().view.rect_to_screen(h.state().viewport.min, rect).left_top() + Vec2::new(50.0, -12.0);
                h.hover_at(point);
                h.step();
                for _ in 0..2 {
                    for pressed in [true, false] {
                        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
                        h.step();
                    }
                }
                h.run();
            } else {
                h.get_by_role_and_label(Role::TextInput, "Text").click();
                h.run();
            }
            h.event(egui::Event::Text("uncommitted private draft".into()));
            h.run();
            let owner = h.ctx.memory(|m| m.focused()).unwrap();
            let policy = h.ctx.options(|o| o.input_options.surrender_focus_on);
            let original = h.state().session.document().clone();
            let point = h.get_by_role_and_label(Role::Button, "Tag filter").rect().center();
            press_at(&mut h, point, egui::PointerButton::Primary);
            assert_eq!(h.ctx.memory(|m| m.focused()), Some(owner));
            assert!(!h.state().tag_filter.open);
            assert_eq!(h.state().session.document(), &original);
            assert_eq!(h.ctx.options(|o| o.input_options.surrender_focus_on), policy);
            // A real context-menu opening also leaves the draft uncommitted and tagging disabled.
            let rect = *h.state().painted.values().next().unwrap();
            let point = h.state().view.rect_to_screen(h.state().viewport.min, rect).left_center() + Vec2::new(4.0, 0.0);
            press_at(&mut h, point, egui::PointerButton::Secondary);
            assert_eq!(h.state().session.document(), &original);
            assert!(!h.state().session.can_undo());
        }
    }
    #[test]
    fn same_id_revision_empty_session_replacement_resets_cached_tags_and_pending_navigation() {
        use nodecraft_graph::Tag;
        let mut h = harness();
        let c = h.state().canvas;
        let mut p = h.state().session.document().clone();
        let ids: Vec<_> = p.canvases().get(&c).unwrap().nodes().keys().copied().collect();
        for id in ids {
            Change::RemoveNode { canvas: c, id }.apply(&mut p).unwrap();
        }
        let t = TagId::from_bytes([8; 16]);
        Change::PutTag { canvas: c, tag: Tag { id: t, title: "Old".into(), color: None } }.apply(&mut p).unwrap();
        h.state_mut().session = Session::new(p.clone()).unwrap();
        h.run();
        assert_eq!(h.state().tag_filter.presentation, vec![t]);
        h.state_mut().tag_filter.set(BTreeSet::from([t]), false);
        h.state_mut().tag_filter.advance(false);
        Change::RemoveTag { canvas: c, id: t }.apply(&mut p).unwrap();
        let new = TagId::from_bytes([9; 16]);
        Change::PutTag { canvas: c, tag: Tag { id: new, title: "New".into(), color: None } }.apply(&mut p).unwrap();
        h.state_mut().session = Session::new(p).unwrap();
        h.run();
        assert_eq!(h.state().tag_filter.presentation, vec![new]);
        assert!(!h.state().tag_filter.active());
        assert_eq!(h.state().tag_filter.frame_request, None);
    }
    #[test]
    fn long_duplicate_labels_use_single_line_virtualized_rows_and_unique_visible_ordinals() {
        let mut h = harness();
        let c = h.state().canvas;
        let mut p = h.state().session.document().clone();
        let changes = (1u128..=100)
            .map(|n| Change::PutTag {
                canvas: c,
                tag: nodecraft_graph::Tag { id: TagId::from_bytes(n.to_be_bytes()), title: ("Long\nTitle ".repeat(300)), color: Some("future-color".into()) },
            })
            .collect();
        nodecraft_graph::Changeset::new(changes).apply(&mut p).unwrap();
        h.state_mut().replace_session(Session::new(p).unwrap(), c).unwrap();
        h.run();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.run();
        let prefix = label(&"Long\nTitle ".repeat(300));
        let first = h.get_by_label(&format!("#1 · {prefix}")).rect();
        let second = h.get_by_label(&format!("#2 · {prefix}")).rect();
        assert!(first.height() <= h.state().theme.tokens().tag_row_height);
        assert!(second.top() > first.top());
        assert!(h.query_by_label(&format!("#100 · {prefix}")).is_none());
    }
    #[test]
    fn expanded_disabled_header_controls_and_background_preserve_dirty_editor() {
        for target in 0..3 {
            let mut h = harness();
            let ctx = h.ctx.clone();
            h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
            h.run();
            h.get_by_role_and_label(Role::TextInput, "Text").click();
            h.run();
            h.event(egui::Event::Text("uncommitted expanded draft".into()));
            h.run();
            let original = h.state().session.document().clone();
            let owner = ctx.memory(|m| m.focused()).unwrap();
            let policy = ctx.options(|o| o.input_options.surrender_focus_on);
            let point = match target {
                0 => h.get_by_role_and_label(Role::Button, "Tag filter · highlight matches").rect().center(),
                1 => h.get_by_role_and_label(Role::Button, "Clear tag filter").rect().center(),
                _ => h.state().tag_rect().left_bottom() + Vec2::new(10.0, -10.0),
            };
            press_at(&mut h, point, egui::PointerButton::Primary);
            assert_eq!(ctx.memory(|m| m.focused()), Some(owner));
            assert_eq!(h.state().session.document(), &original);
            assert_eq!(ctx.options(|o| o.input_options.surrender_focus_on), policy);
            assert!(h.state().drag.is_none());
            assert!(h.state().marquee.is_none());
        }
    }
    #[test]
    fn supported_session_replacement_reinstalls_all_live_services_and_discards_old_mailboxes() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().selection.add(NodeId::from_bytes([3; 16]));
        *h.state().tag_mailbox.borrow_mut() = Some(json!({"action":"next"}));
        h.state_mut().theme = ThemeKind::Light;
        let replacement = sample();
        let c = replacement.canvas;
        h.state_mut().replace_session(Session::new(replacement.session.document().clone()).unwrap(), c).unwrap();
        h.run();
        assert_eq!(h.state().theme, ThemeKind::Light);
        assert!(h.state().selection.is_empty());
        assert!(h.state().tag_mailbox.borrow().is_none());
        assert!(h.state().editors.is_empty() || !h.state().text_focused);
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).unwrap();
        h.run();
        assert!(h.state().tag_filter.current.is_some());
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_close", &json!({})).unwrap();
        h.run();
        h.state_mut().dispatch_search(&ctx, "canvas.search_open", &json!({})).unwrap();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"Public sample"})).unwrap();
        h.run();
        assert_eq!(h.state().search.results.len(), 2);
        h.state_mut().dispatch_search(&ctx, "canvas.search_close", &json!({})).unwrap();
        h.run();
        h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn latest_explicit_search_or_tag_jump_cancels_competing_pending_navigation() {
        let mut h =
            Harness::builder().with_size(Vec2::new(1200.0, 800.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), sample());
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_search(&ctx, "canvas.search_open", &json!({})).unwrap();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"sample 4"})).unwrap();
        assert!(h.state().search.scanning);
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).unwrap();
        h.run();
        let first = NodeId::from_bytes([3; 16]);
        assert!(
            h.state().selection.contains(first),
            "selected {:?}, tag current {:?}, frame {:?}, search current {:?}, notice {:?}",
            h.state().selection.nodes(),
            h.state().tag_filter.current,
            h.state().tag_filter.frame_request,
            h.state().search.current,
            h.state().notice
        );
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_close", &json!({})).unwrap();
        h.run();
        assert!(
            h.state().selection.contains(first),
            "selected {:?}, tag current {:?}, frame {:?}, search current {:?}, notice {:?}",
            h.state().selection.nodes(),
            h.state().tag_filter.current,
            h.state().tag_filter.frame_request,
            h.state().search.current,
            h.state().notice
        );
        h.state_mut().tag_filter.set(BTreeSet::new(), false);
        h.state_mut().tag_filter.set(BTreeSet::new(), true);
        h.state_mut().tag_filter.advance(false);
        h.state_mut().dispatch_search(&ctx, "canvas.search_next", &json!({})).unwrap();
        h.run();
        assert!(h.state().selection.contains(NodeId::from_bytes([4; 16])));
        assert_eq!(h.state().tag_filter.frame_request, None);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn same_frame_clear_cancels_a_just_completed_old_filter_jump() {
        let mut h =
            Harness::builder().with_size(Vec2::new(1200.0, 800.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), sample());
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.run();
        let selected = NodeId::from_bytes([4; 16]);
        h.state_mut().selection.only(selected);
        let point = h.get_by_role_and_label(Role::Button, "Clear tag filter").rect().center();
        h.hover_at(point);
        h.run();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).unwrap();
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        }
        h.run();
        assert!(!h.state().tag_filter.active());
        assert!(
            h.state().selection.contains(selected),
            "selection {:?}; filter current {:?}; frame {:?}",
            h.state().selection.nodes(),
            h.state().tag_filter.current,
            h.state().tag_filter.frame_request
        );
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn narrow_populated_discussions_and_tags_never_cover_framed_cards() {
        let mut h =
            Harness::builder().with_size(Vec2::new(520.0, 900.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), sample());
        let canvas = h.state().canvas;
        h.state_mut().session.dispatch("comment.create",&json!({"canvas":canvas,"id":nodecraft_types::CommentId::from_bytes([80;16]),"text":"A populated discussion panel with a visible thread","pos":{"x":0,"y":0}})).unwrap();
        h.state_mut().comments.open = true;
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        for command in ["canvas.tag_filter_next", "canvas.tag_filter_previous"] {
            h.state_mut().dispatch_tag_filter(&ctx, command, &json!({})).unwrap();
            h.run();
            let app = h.state();
            let id = app.tag_filter.current.unwrap();
            let body = app.navigation.geometry.rects[&id];
            let decorated = crate::navigation::decorated(body, app.theme.tokens());
            let screen = app.view.rect_to_screen(app.viewport.min, decorated);
            assert!(app.viewport.contains_rect(screen), "{screen:?}");
            assert!(!screen.intersects(app.tag_rect()), "card{screen:?}, tags{:?}", app.tag_rect());
            assert!(!screen.intersects(app.discussion_rect()), "card{screen:?}, comments{:?}", app.discussion_rect());
            assert_eq!(app.session.document(), &before);
            assert_eq!(app.session.revision(), revision);
        }
    }
    #[test]
    fn narrow_viewport_tag_jump_uses_actual_card_geometry_and_theme_markers_are_distinct() {
        let mut h =
            Harness::builder().with_size(Vec2::new(520.0, 760.0)).with_max_steps(160).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), sample());
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags":[],"untagged":true})).unwrap();
        h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).unwrap();
        h.run();
        let id = h.state().tag_filter.current.unwrap();
        let body = h.state().navigation.geometry.rects[&id];
        let screen = h.state().view.rect_to_screen(h.state().viewport.min, crate::navigation::decorated(body, h.state().theme.tokens()));
        assert!(!screen.intersects(h.state().tag_rect()));
        assert!(screen.left() >= h.state().viewport.left());
        assert!(screen.right() <= h.state().viewport.right());
        for (color, _) in PALETTE {
            assert_ne!(theme::DARK.tag_color(Some(color)), theme::LIGHT.tag_color(Some(color)));
        }
        assert_eq!(theme::DARK.tag_color(Some("unknown")), theme::DARK.text_muted);
    }
}
