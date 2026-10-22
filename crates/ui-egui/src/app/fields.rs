//! Fields on the canvas (#66): named, coloured zones drawn under the cards. Membership is
//! explicit, never inferred from overlap. Dragging a header moves the zone and every member in one
//! `field.move`; the lower-right handle resizes the zone alone; a collapsed field hides its
//! members' cards (their noodles end at its header). Everything here is view state except the
//! commands it dispatches.
use super::*;
use nodecraft_graph::Field;
use nodecraft_types::FieldId;

/// Size of a field quick-added with nothing selected, in world points.
const EMPTY_FIELD: Vec2 = Vec2::new(320.0, 240.0);

/// Field view state, reset when the session or canvas changes.
#[derive(Default)]
pub(super) struct Fields {
    /// The field whose header was clicked last; cleared when nodes are selected.
    pub(super) selected: Option<FieldId>,
    pub(super) drag: Option<FieldDrag>,
    /// The field being renamed and its draft title.
    pub(super) rename: Option<(FieldId, String)>,
    owner: Option<(CanvasId, Arc<()>)>,
    membership: Option<((CanvasId, u64), Arc<Membership>)>,
}

/// Who is in which field, worked out once per document revision (#709).
#[derive(Default)]
pub(super) struct Membership {
    /// Nodes in a collapsed field: their cards are not drawn and can't be selected.
    pub(super) hidden: BTreeSet<NodeId>,
    /// Member count per field.
    pub(super) counts: BTreeMap<FieldId, usize>,
}

/// A header (move) or corner (resize) drag, previewed locally and dispatched on release.
pub(super) struct FieldDrag {
    id: FieldId,
    /// A document change mid-drag (undo, a remote edit) cancels the drag.
    revision: u64,
    /// Screen point the press started at.
    start: Pos2,
    /// World offset so far; for a resize, the growth of the lower-right corner.
    pub(super) delta: Vec2,
    resize: bool,
    /// The field's world rect when the drag started.
    base: Rect,
}

/// The header band in world points: never thinner on screen than a readable title.
fn header_height(t: &Tokens, zoom: f32) -> f32 {
    t.field_header_height.max((t.title_min_screen_px + 2.0 * t.control_gap) / zoom.max(f32::MIN_POSITIVE))
}

/// A field's world rect as drawn: a collapsed field shows only its header.
pub(super) fn world_rect(field: &Field, t: &Tokens, zoom: f32) -> Rect {
    let header = header_height(t, zoom);
    let height = if field.collapsed { header } else { field.rect.height.max(header) };
    Rect::from_min_size(Pos2::new(field.rect.pos.x, field.rect.pos.y), Vec2::new(field.rect.width, height))
}

impl Membership {
    fn of(canvas: &Canvas) -> Self {
        let mut membership = Self::default();
        for node in canvas.nodes().values() {
            let Some(field) = node.field.and_then(|f| canvas.fields().get(&f)) else { continue };
            let count = membership.counts.entry(field.id).or_default();
            *count = count.saturating_add(1);
            if field.collapsed {
                membership.hidden.insert(node.id);
            }
        }
        membership
    }
}

/// The world rect around `nodes`' cards, with room for every one of them: measured geometry
/// where there is some, else the stored position, so a card without geometry yet (just added,
/// or one that failed to measure) still ends up inside the zone (#709).
fn members_rect(canvas: &Canvas, rects: &BTreeMap<NodeId, Rect>, nodes: impl Iterator<Item = NodeId>) -> Option<Rect> {
    nodes
        .filter_map(|id| {
            rects.get(&id).copied().or_else(|| canvas.nodes().get(&id).map(|node| Rect::from_min_size(Pos2::new(node.pos.x, node.pos.y), Vec2::ZERO)))
        })
        .filter(|rect| rect.is_finite())
        .reduce(|a, b| a.union(b))
}

/// Where noodles to hidden nodes end: their collapsed field's header, following it while it is
/// dragged (`shift`).
pub(super) fn stand_ins<'a>(
    canvas: &'a Canvas,
    hidden: &'a BTreeSet<NodeId>,
    t: &'a Tokens,
    zoom: f32,
    shift: Option<(FieldId, Vec2)>,
) -> impl Iterator<Item = (NodeId, Rect)> + 'a {
    hidden.iter().filter_map(move |id| {
        let field = canvas.nodes().get(id)?.field.and_then(|f| canvas.fields().get(&f))?;
        let offset = shift.filter(|(moving, _)| *moving == field.id).map_or(Vec2::ZERO, |(_, delta)| delta);
        Some((*id, world_rect(field, t, zoom).translate(offset)))
    })
}

/// Whether a wire runs inside one collapsed field: both ends hidden in it, so it isn't drawn.
pub(super) fn inside_collapsed(canvas: &Canvas, hidden: &BTreeSet<NodeId>, from: NodeId, to: NodeId) -> bool {
    let field = |id: NodeId| canvas.nodes().get(&id).and_then(|node| node.field);
    hidden.contains(&from) && hidden.contains(&to) && field(from).is_some() && field(from) == field(to)
}

/// A field title as shown: one bounded line, never blank.
fn label(title: &str) -> String {
    if title.trim().is_empty() { "Untitled field".into() } else { crate::tags::label(title) }
}

/// `field.update`'s rect for a world rect, held to the smallest resizable zone.
fn rect_json(rect: Rect, t: &Tokens) -> serde_json::Value {
    json!({"pos": {"x": rect.min.x, "y": rect.min.y}, "width": rect.width().max(t.field_min_width), "height": rect.height().max(t.field_min_height)})
}

/// A zone around member card rects: padded, with room for the header above them.
fn zone(members: Rect, t: &Tokens) -> Rect {
    let mut zone = members.expand(t.field_padding);
    zone.min.y -= t.field_header_height;
    zone
}

/// What a card's context menu asked of fields.
pub(super) enum FieldAction {
    Assign(FieldId),
    Clear,
}

/// The card menu's field entries: put the selected nodes in a field, or take them out.
pub(super) fn card_menu(ui: &mut egui::Ui, canvas: &Canvas, selection: &Selection, t: &Tokens) -> Option<FieldAction> {
    let mut action = None;
    ui.menu_button("Field", |ui| {
        let any = !selection.nodes().is_empty();
        let member = selection.nodes().iter().any(|id| canvas.nodes().get(id).is_some_and(|n| n.field.is_some()));
        if ui.add_enabled(any && member, egui::Button::new("Remove from field")).clicked() {
            action = Some(FieldAction::Clear);
            ui.close();
        }
        if canvas.fields().is_empty() {
            ui.label(egui::RichText::new("No fields yet: press F to make one").color(t.text_muted));
        }
        let fields: Vec<&Field> = canvas.fields().values().collect();
        egui::ScrollArea::vertical().id_salt("assign_fields").max_height(t.tag_panel_height).show_rows(ui, t.tag_row_height, fields.len(), |ui, range| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for field in fields.get(range).unwrap_or(&[]) {
                let text = egui::RichText::new(format!("Move into {}", label(&field.title))).color(t.tag_color(field.color.as_deref()));
                if ui.add_enabled(any, egui::Button::new(text).truncate()).clicked() {
                    action = Some(FieldAction::Assign(field.id));
                    ui.close();
                }
            }
        });
    });
    action
}

impl NodeCraftApp {
    /// Drop field view state that no longer applies: another session or canvas, a field that
    /// went away, a drag the document moved under, and selected nodes a collapse hid.
    pub(super) fn fields_sync(&mut self) {
        let identity = self.session.view_identity();
        if self.fields.owner.as_ref().is_none_or(|(canvas, old)| *canvas != self.canvas || !Arc::ptr_eq(old, &identity)) {
            self.fields = Fields { owner: Some((self.canvas, identity)), ..Fields::default() };
        }
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return };
        let exists = |id: &FieldId| canvas.fields().contains_key(id);
        if !self.fields.selected.as_ref().is_none_or(exists) {
            self.fields.selected = None;
        }
        if !self.fields.rename.as_ref().is_none_or(|(id, _)| exists(id)) {
            self.fields.rename = None;
        }
        if self.fields.drag.as_ref().is_some_and(|drag| drag.revision != self.session.revision() || !exists(&drag.id)) {
            self.fields.drag = None;
        }
        let membership = self.membership();
        if !membership.hidden.is_empty() {
            self.selection.remove_nodes(&membership.hidden);
        }
        if !self.selection.is_empty() {
            self.fields.selected = None;
        }
    }

    /// This revision's field membership, recomputed only when the document changes. The field
    /// view state, and so this cache, resets with the session or canvas.
    pub(super) fn membership(&mut self) -> Arc<Membership> {
        let key = (self.canvas, self.session.revision());
        if let Some((cached, membership)) = &self.fields.membership
            && *cached == key
        {
            return Arc::clone(membership);
        }
        let membership = Arc::new(self.session.document().canvases().get(&self.canvas).map(Membership::of).unwrap_or_default());
        self.fields.membership = Some((key, Arc::clone(&membership)));
        membership
    }

    /// The cards collapsed fields hide, and those fields' header rects in world space, moved
    /// with a field drag.
    pub(super) fn minimap_fields(&mut self) -> (Arc<Membership>, Vec<Rect>) {
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return Default::default() };
        let (t, zoom, shift) = (self.theme.tokens(), self.view.zoom, self.field_shift());
        let headers = canvas
            .fields()
            .values()
            .filter(|field| field.collapsed)
            .map(|field| {
                let header = world_rect(field, t, zoom);
                shift.filter(|(id, _)| *id == field.id).map_or(header, |(_, delta)| header.translate(delta))
            })
            .collect();
        (self.membership(), headers)
    }

    /// Where a field drag shows a member card this frame.
    pub(super) fn field_preview(drag: Option<(FieldId, Vec2)>, node: &Node) -> Option<nodecraft_graph::Position> {
        let (_, delta) = drag.filter(|(id, _)| node.field == Some(*id))?;
        Some(nodecraft_graph::Position { x: node.pos.x + delta.x, y: node.pos.y + delta.y })
    }

    /// The moving field and its offset, for previewing members.
    pub(super) fn field_shift(&self) -> Option<(FieldId, Vec2)> {
        self.fields.drag.as_ref().filter(|drag| !drag.resize).map(|drag| (drag.id, drag.delta))
    }

    fn field_dispatch(&mut self, command: &str, params: serde_json::Value) {
        match self.session.dispatch(command, &params) {
            Ok(_) => self.notice = None,
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Runs a card menu's field choice on the selected nodes, in one undo step.
    pub(super) fn field_card_action(&mut self, action: FieldAction) {
        let field = match action {
            FieldAction::Assign(id) => json!(id),
            FieldAction::Clear => serde_json::Value::Null,
        };
        let nodes: Vec<NodeId> = self.selection.nodes().iter().copied().collect();
        self.field_dispatch("node.field_set", json!({"canvas": self.canvas, "nodes": nodes, "field": field}));
    }

    /// `F`: a field around the selected cards, or an empty one at the pointer. One undo step.
    pub(super) fn quick_field(&mut self, pointer: Option<Pos2>) {
        let t = self.theme.tokens();
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        let members = self
            .session
            .document()
            .canvases()
            .get(&self.canvas)
            .and_then(|canvas| members_rect(canvas, &self.navigation.geometry.rects, self.selection.nodes().iter().copied()));
        let rect = match members {
            Some(members) => zone(members, t),
            None => {
                let at = pointer.filter(|p| self.viewport.contains(*p)).unwrap_or(self.viewport.center());
                Rect::from_min_size(self.view.to_world(self.viewport.min, at), EMPTY_FIELD)
            }
        };
        let seed = ("field", self.session.revision(), rect.min.x.to_bits(), rect.min.y.to_bits());
        let Some(id) = ids::field_id(self.session.document(), seed) else {
            self.notice = Some("Couldn't allocate a field id; try again.".into());
            return;
        };
        let nodes: Vec<NodeId> = self.selection.nodes().iter().copied().collect();
        let field = json!({"id": id, "title": "Field", "rect": rect_json(rect, t), "color": "blue"});
        match self.session.dispatch("field.create", &json!({"canvas": self.canvas, "field": field, "nodes": nodes})) {
            Ok(_) => {
                self.notice = None;
                self.selection.clear();
                self.fields.selected = Some(id);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// Paints the fields under the cards and runs their header and resize gestures. `blocked`
    /// is the canvas's input gate; returns the screen pan asked for by a middle or right drag.
    pub(super) fn fields_show(&mut self, ui: &mut egui::Ui, viewport: Rect, blocked: bool) -> Vec2 {
        let t = self.theme.tokens();
        let zoom = self.view.zoom;
        let mut pan = Vec2::ZERO;
        let mut action: Option<(&str, serde_json::Value)> = None;
        let mut rename = None;
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let typing = self.text_focused || canvas::text_input_focused(ui.ctx());
        let sense = if blocked || typing { Sense::hover() } else { Sense::click_and_drag() };
        // Shift-drag on a header or a resize corner draws a marquee, as it does everywhere else.
        let header_sense = if sense.senses_drag() && ui.input(|i| i.modifiers.shift) { Sense::click() } else { sense };
        // No geometry refresh here: Fit to members falls back to a card's stored position, and
        // a second full refresh in a frame that took remote edits blows the frame budget.
        let membership = self.membership();
        let Some(canvas) = self.session.document().canvases().get(&self.canvas) else { return pan };
        let painter = ui.painter_at(viewport);
        for field in canvas.fields().values() {
            let mut world = world_rect(field, t, zoom);
            if let Some(drag) = self.fields.drag.as_ref().filter(|drag| drag.id == field.id) {
                world = if drag.resize { Rect::from_min_size(drag.base.min, drag.base.size() + drag.delta) } else { drag.base.translate(drag.delta) };
            }
            let screen = self.view.rect_to_screen(viewport.min, world);
            if !screen.is_finite() || !screen.intersects(viewport) {
                continue;
            }
            let color = t.tag_color(field.color.as_deref());
            let selected = self.fields.selected == Some(field.id);
            let stroke = egui::Stroke::new(if selected { t.handle_stroke * 2.0 } else { t.handle_stroke }, if selected { t.accent } else { color });
            painter.rect(screen, t.panel_radius, color.gamma_multiply(t.field_fill_opacity), stroke, egui::StrokeKind::Inside);
            let header = Rect::from_min_size(screen.min, Vec2::new(screen.width(), (header_height(t, zoom) * zoom).min(screen.height())));
            painter.rect_filled(header, t.panel_radius, color.gamma_multiply(t.field_fill_opacity * 2.0));
            let name = label(&field.title);
            let members = membership.counts.get(&field.id).copied().unwrap_or(0);
            let marker = if field.collapsed { "+" } else { "−" };
            let mut job = egui::text::LayoutJob::simple_singleline(
                format!("{marker}  {name} · {members}"),
                egui::FontId::proportional((t.body_size * zoom).clamp(t.title_min_screen_px, t.body_size * 2.0)),
                t.text,
            );
            job.wrap = egui::text::TextWrapping {
                max_width: (header.width() - 2.0 * t.control_gap).max(1.0),
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
            let text_at = Pos2::new(header.min.x + t.control_gap, header.center().y - galley.size().y * 0.5);
            painter.with_clip_rect(header.intersect(viewport)).galley(text_at, galley, t.text);

            let response = ui.interact(header, ui.id().with(("field", field.id)), header_sense);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !blocked, format!("Field {name}")));
            if response.dragged_by(egui::PointerButton::Middle) || response.dragged_by(egui::PointerButton::Secondary) {
                pan += response.drag_delta();
            }
            if response.clicked() {
                self.selection.clear();
                self.fields.selected = Some(field.id);
            }
            if response.double_clicked() {
                action = Some(("field.update", json!({"canvas": self.canvas, "field": field.id, "collapsed": !field.collapsed})));
            }
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(start) = ui.input(|i| i.pointer.press_origin()).or(pointer)
            {
                self.selection.clear();
                self.fields.selected = Some(field.id);
                let base = world_rect(field, t, zoom);
                self.fields.drag = Some(FieldDrag { id: field.id, revision: self.session.revision(), start, delta: Vec2::ZERO, resize: false, base });
            }
            if !blocked || egui::Popup::is_any_open(ui.ctx()) {
                response.context_menu(|ui| {
                    if ui.button("Rename field…").clicked() {
                        rename = Some((field.id, field.title.clone()));
                        ui.close();
                    }
                    if ui.button(if field.collapsed { "Expand field" } else { "Collapse field" }).clicked() {
                        action = Some(("field.update", json!({"canvas": self.canvas, "field": field.id, "collapsed": !field.collapsed})));
                        ui.close();
                    }
                    let members =
                        members_rect(canvas, &self.navigation.geometry.rects, canvas.nodes().values().filter(|n| n.field == Some(field.id)).map(|n| n.id));
                    if ui.add_enabled(members.is_some(), egui::Button::new("Fit to members")).on_disabled_hover_text("The field has no members").clicked()
                        && let Some(members) = members
                    {
                        action = Some(("field.update", json!({"canvas": self.canvas, "field": field.id, "rect": rect_json(zone(members, t), t)})));
                        ui.close();
                    }
                    ui.menu_button("Field colour", |ui| {
                        for (index, (id, name)) in crate::tags::PALETTE.iter().enumerate() {
                            let text = egui::RichText::new(*name).color(t.tag_colors.get(index).copied().unwrap_or(t.text));
                            if ui.button(text).clicked() {
                                action = Some(("field.update", json!({"canvas": self.canvas, "field": field.id, "color": id})));
                                ui.close();
                            }
                        }
                    });
                    if ui.button("Remove field (keep nodes)").clicked() {
                        action = Some(("field.remove", json!({"canvas": self.canvas, "field": field.id})));
                        ui.close();
                    }
                });
            }
            if !field.collapsed {
                let side = (t.field_header_height * 0.5 * zoom).min(screen.width()).min(screen.height() - header.height()).max(0.0);
                let handle = Rect::from_min_size(screen.max - Vec2::splat(side), Vec2::splat(side));
                let corner = ui.interact(handle, ui.id().with(("field_resize", field.id)), header_sense);
                corner.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !blocked, format!("Resize field {name}")));
                painter.line_segment([handle.left_bottom(), handle.right_top()], egui::Stroke::new(t.handle_stroke, color));
                if corner.hovered() || corner.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                }
                if corner.drag_started_by(egui::PointerButton::Primary)
                    && let Some(start) = ui.input(|i| i.pointer.press_origin()).or(pointer)
                {
                    // The stored rect, not the drawn one: at low zoom the header is drawn taller
                    // than a short field, and a width-only resize must keep its height.
                    let base = Rect::from_min_size(Pos2::new(field.rect.pos.x, field.rect.pos.y), Vec2::new(field.rect.width, field.rect.height));
                    self.fields.drag = Some(FieldDrag { id: field.id, revision: self.session.revision(), start, delta: Vec2::ZERO, resize: true, base });
                }
            }
        }
        if let Some(drag) = &mut self.fields.drag {
            if blocked || typing {
                self.fields.drag = None;
            } else if let Some(p) = pointer {
                let delta = (p - drag.start) / zoom.max(f32::MIN_POSITIVE);
                drag.delta = if delta.is_finite() { delta } else { Vec2::ZERO };
                if drag.resize {
                    let floor = Vec2::new(t.field_min_width, t.field_min_height);
                    drag.delta = drag.delta.max(floor - drag.base.size());
                }
            }
        }
        if !ui.input(|i| i.pointer.primary_down())
            && let Some(drag) = self.fields.drag.take()
            && drag.delta != Vec2::ZERO
        {
            action = Some(if drag.resize {
                let rect = Rect::from_min_size(drag.base.min, drag.base.size() + drag.delta);
                ("field.update", json!({"canvas": self.canvas, "field": drag.id, "rect": rect_json(rect, t)}))
            } else {
                ("field.move", json!({"canvas": self.canvas, "field": drag.id, "delta": {"x": drag.delta.x, "y": drag.delta.y}}))
            });
        }
        if let Some((command, params)) = action {
            self.field_dispatch(command, params);
            self.fields_sync();
        }
        if rename.is_some() {
            self.fields.rename = rename;
        }
        self.rename_window(ui.ctx());
        pan
    }

    /// The rename dialog: Enter or Save applies the title, Escape or Cancel keeps it.
    fn rename_window(&mut self, ctx: &egui::Context) {
        let Some((id, mut title)) = self.fields.rename.take() else { return };
        let mut keep = true;
        let mut save = false;
        egui::Window::new("Rename field").collapsible(false).resizable(false).show(ctx, |ui| {
            let label = ui.label("Field name");
            let edit = ui.add(egui::TextEdit::singleline(&mut title).char_limit(4096)).labelled_by(label.id);
            if !edit.has_focus() && !edit.lost_focus() && ctx.memory(|m| m.focused().is_none()) {
                edit.request_focus();
            }
            ui.horizontal(|ui| {
                save = ui.button("Save field name").clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if ui.button("Cancel rename").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    keep = false;
                }
            });
        });
        if save && keep {
            if title.len() > 4096 {
                self.notice = Some("A field name is at most 4096 bytes.".into());
            } else {
                match self.session.dispatch("field.update", &json!({"canvas": self.canvas, "field": id, "title": title})) {
                    Ok(_) => {
                        self.notice = None;
                        keep = false;
                    }
                    Err(error) => self.notice = Some(error.message),
                }
            }
        }
        if keep {
            self.fields.rename = Some((id, title));
        }
    }
}
