//! Discussion overlay and live view bridge. Only engine commands publish accepted text.
use super::*;
use crate::comments::{Draft, MAX_DRAFT_BYTES, MAX_DRAFTS, MESSAGE_PAGE, PinDrag, Target};
use nodecraft_engine::comment_cmds::CommentContext;
use nodecraft_graph::Position;
use nodecraft_types::CommentId;
use serde_json::Value as Json;

/// Prefer the ordinary right-side placement, but keep a projected pin reachable when
/// a card reaches a viewport edge. Never clamp a foreground Area across a port hit cell.
fn projected_marker(anchor: Pos2, body: Rect, decorated: Rect, viewport: Rect, t: &Tokens) -> Pos2 {
    let size = t.comment_marker_size;
    let clearance = t.comment_marker_gap + t.comment_marker_ring;
    // Subtract first, then order the endpoints. (top + size) - size can round below
    // top (e.g. -13.97 with size 30), which makes f32::clamp panic at overview zoom.
    let y = anchor.y.clamp(body.top(), (body.bottom() - size).max(body.top()));
    let right = Pos2::new(decorated.right() + clearance, y);
    let shown = |at| Rect::from_min_size(at, Vec2::splat(size)).expand(t.comment_marker_ring);
    // Reachability is for visible cards, not an offscreen-node indicator. Keep the
    // ordinary projection and natural culling when the entire decorated card is gone.
    if !decorated.intersects(viewport) || viewport.contains_rect(shown(right)) {
        return right;
    }
    let x =
        anchor.x.clamp(viewport.left() + t.comment_marker_ring, (viewport.right() - size - t.comment_marker_ring).max(viewport.left() + t.comment_marker_ring));
    let y = y.clamp(viewport.top() + t.comment_marker_ring, (viewport.bottom() - size - t.comment_marker_ring).max(viewport.top() + t.comment_marker_ring));
    for candidate in [
        Pos2::new(right.x, y),
        Pos2::new(decorated.left() - clearance - size, y),
        Pos2::new(x, decorated.top() - clearance - size),
        Pos2::new(x, decorated.bottom() + clearance),
    ] {
        if viewport.contains_rect(shown(candidate)) && !decorated.intersects(shown(candidate)) {
            return candidate;
        }
    }
    // A zoomed-in card can fill the entire viewport: no exterior position exists.
    // Prefer a visible interior pin away from BOTH side hit strips in that case.
    let side = (decorated.right() - body.right()).max(body.left() - decorated.left());
    let interior = body.shrink(side + clearance).intersect(viewport.shrink(t.comment_marker_ring));
    if interior.width() >= size && interior.height() >= size {
        return Pos2::new(
            x.clamp(interior.left(), (interior.right() - size).max(interior.left())),
            y.clamp(interior.top(), (interior.bottom() - size).max(interior.top())),
        );
    }
    right
}

enum Action {
    Open(CommentId),
    Compose(Option<NodeId>),
    Note,
    Edit(Target),
    Save,
    Cancel,
    Close,
    Reload,
    SaveNew,
    Remove,
    Resolve(bool),
    RemoveReply(CommentId),
    Detach,
    Attach(NodeId),
    Discard(u64),
    Resume(u64),
}
pub(super) fn view_command(id: &str) -> bool {
    matches!(id, "comment.compose" | "comment.open" | "canvas.comments_toggle")
}
impl NodeCraftApp {
    pub(super) fn discussion_rect(&self) -> Rect {
        let t = self.theme.tokens();
        let available_width = (self.viewport.width() - 2.0 * t.navigation_margin).max(1.0);
        let width = t.comments_width.min(if self.tag_filter.open { ((available_width - t.navigation_gap) / 2.0).max(1.0) } else { available_width });
        let top = self.comments_toolbar_rect().bottom() + t.navigation_gap;
        let map_top = self.navigation.usable(self.viewport, t).bottom();
        // Prefer the space above navigation. A short viewport must still leave a usable
        // scrolling discussion panel; foreground hit ownership then protects the map below it.
        let bottom = if map_top - top >= 2.0 * t.card_padding + t.navigation_toolbar_height { map_top } else { self.viewport.bottom() - t.navigation_margin };
        let height = t.comments_height.min((bottom - top).max(1.0));
        Rect::from_min_size(Pos2::new(self.viewport.right() - t.navigation_margin - width, top), Vec2::new(width, height))
    }
    pub(super) fn comments_toolbar_rect(&self) -> Rect {
        let t = self.theme.tokens();
        let width = t.comments_button_width.min((self.viewport.width() - 2.0 * t.navigation_margin).max(1.0));
        Rect::from_min_size(
            Pos2::new(self.viewport.right() - t.navigation_margin - width, self.search_rect().bottom() + t.navigation_gap),
            Vec2::new(width, t.navigation_toolbar_height),
        )
    }
    fn discussion_context(&self, ctx: &egui::Context, owner: Option<egui::Id>) -> CommentContext {
        let focused = ctx.memory(|m| m.focused());
        let own = focused.map_or_else(
            || self.comments.last_focus.is_some() && self.last_text_focus == self.comments.last_focus,
            |id| {
                self.comments.drafts.keys().any(|key| id == egui::Id::new("discussion_edit").with(key))
                    || self.comments.marker_ids.contains(&id)
                    || ctx.read_response(id).is_some_and(|response| {
                        response.layer_id == egui::LayerId::new(egui::Order::Foreground, egui::Id::new("discussions_panel"))
                            || response.layer_id == egui::LayerId::new(egui::Order::Foreground, egui::Id::new("comments_toolbar"))
                    })
            },
        );
        CommentContext {
            project: self.session.document().id(),
            canvas: self.canvas,
            revision: self.session.revision(),
            blocked: self.palette.is_some()
                || self.turn_into.is_some()
                || self.reordering.is_some()
                || ctx.input(|i| !i.raw.hovered_files.is_empty() || !i.raw.dropped_files.is_empty())
                || self.wiring.is_some()
                || (egui::Popup::is_any_open(ctx) && !owner.is_some_and(|id| egui::Popup::is_id_open(ctx, id)))
                || ((canvas::text_input_focused(ctx) || self.text_focused) && !own)
                || self.drag.is_some()
                || self.marquee.is_some()
                || self.navigation.pointer_owned
                || self.navigation.drag.is_some()
                || self.comments.drag.is_some()
                || self.comments.gesture_cancelled
                || ctx.dragged_id().is_some_and(|id| !self.comments.marker_ids.contains(&id)),
        }
    }
    pub fn comments_enabled(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<(), Error> {
        if !view_command(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a discussion view command"));
        }
        let previous = self.session.swap_resource(Some(self.discussion_context(ctx, None)));
        let result = self.session.enabled(id, params);
        self.session.swap_resource(previous);
        result
    }
    pub fn dispatch_comments(&mut self, ctx: &egui::Context, id: &str, params: &Json) -> Result<Json, Error> {
        self.dispatch_comments_scoped(ctx, id, params, None)
    }
    fn dispatch_comments_scoped(&mut self, ctx: &egui::Context, id: &str, params: &Json, owner: Option<egui::Id>) -> Result<Json, Error> {
        if !view_command(id) {
            return Err(Error::new(ErrorKind::InvalidInput, "not a discussion view command"));
        }
        *self.comments_mailbox.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "discussions host is busy"))? = None;
        let context = self.discussion_context(ctx, owner);
        let previous = self.session.swap_resource(Some(context));
        let result = self.session.dispatch(id, params);
        self.session.swap_resource(previous);
        result?;
        let request = self
            .comments_mailbox
            .try_borrow_mut()
            .map_err(|_| Error::new(ErrorKind::Conflict, "discussions host is busy"))?
            .take()
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "discussion host did not acknowledge"))?;
        match request.get("action").and_then(Json::as_str) {
            Some("toggle") => {
                if self.comments.open {
                    self.comments.close();
                } else {
                    self.comments.open = true;
                }
            }
            Some("open") => {
                let id: CommentId = serde_json::from_value(request.get("id").cloned().unwrap_or(Json::Null))
                    .map_err(|_| Error::new(ErrorKind::InvalidInput, "discussion id is unavailable"))?;
                self.comments.open = true;
                self.comments.editing = None;
                self.comments.drafts.retain(|_, draft| draft.dirty || draft.error.is_some());
                self.comments.placement = None;
                self.comments.active = Some(id);
                self.comments.message = None;
                self.comments.chunk = 0;
            }
            Some("compose") => {
                let pos: Position = serde_json::from_value(request.get("pos").cloned().unwrap_or(Json::Null))
                    .map_err(|_| Error::new(ErrorKind::InvalidInput, "pin position is unavailable"))?;
                let node: Option<NodeId> = serde_json::from_value(request.get("node").cloned().unwrap_or(Json::Null))
                    .map_err(|_| Error::new(ErrorKind::InvalidInput, "pin node is unavailable"))?;
                let id = self.comments.allocate_comment(self.session.document())?;
                self.begin_draft(ctx, Target::Create(id), Some((node, pos)))?;
            }
            _ => return Err(Error::new(ErrorKind::InvalidInput, "unknown discussion intent")),
        }
        ctx.request_repaint();
        Ok(json!({"open":self.comments.open,"drafts":self.comments.drafts.len()}))
    }
    pub(super) fn compose_on_node(&mut self, ctx: &egui::Context, node: NodeId, owner: Option<egui::Id>) {
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        let Some(rect) = self.navigation.geometry.rects.get(&node) else {
            self.notice = Some("Node geometry is unavailable; use a canvas pin instead.".into());
            return;
        };
        let t = self.theme.tokens();
        let outputs =
            self.session.document().canvases().get(&self.canvas).and_then(|canvas| canvas.nodes().get(&node)).is_some_and(|node| !node.outputs.is_empty());
        let size = rect.size();
        // Persist world geometry only. Minimum screen-size hit targets belong to drawing,
        // otherwise composing at overview zoom permanently pushes the anchor far off-card.
        let right = if outputs { t.handle_radius + card::PORT_HIT } else { 0.0 };
        let offset = Position { x: size.x + right + t.comment_marker_gap + t.comment_marker_ring, y: size.y * 0.5 };
        if let Err(error) = self.dispatch_comments_scoped(ctx, "comment.compose", &json!({"node":node,"pos":offset}), owner) {
            self.notice = Some(error.message);
        }
    }
    fn begin_draft(&mut self, ctx: &egui::Context, target: Target, anchor: Option<(Option<NodeId>, Position)>) -> Result<(), Error> {
        if let Some((&key, _)) = self.comments.drafts.iter().find(|(_, draft)| {
            Arc::ptr_eq(&draft.session, &self.session.view_identity())
                && draft.project == self.session.document().id()
                && draft.canvas == self.canvas
                && draft.target == target
        }) {
            self.comments.editing = Some(key);
            self.comments.focus_pending = Some(key);
            self.comments.open = true;
            ctx.request_repaint();
            return Ok(());
        }
        let scene = self.session.document().canvases().get(&self.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas is unavailable"))?;
        let root = scene.comments().get(&target.root());
        if !matches!(target, Target::Create(_)) && root.is_none() {
            return Err(Error::new(ErrorKind::NotFound, "discussion is unavailable; the draft is retained"));
        }
        if matches!(target, Target::NewReply(..)) && root.is_some_and(|root| root.resolved) {
            return Err(Error::new(ErrorKind::Conflict, "reopen the discussion before replying"));
        }
        let text = match target {
            Target::Create(_) | Target::NewReply(..) => "",
            Target::Root(_) => root.map(|root| root.text.as_str()).ok_or_else(|| Error::new(ErrorKind::NotFound, "discussion is unavailable"))?,
            Target::Reply(_, id) => root
                .and_then(|root| root.replies.iter().find(|reply| reply.id == id))
                .map(|reply| reply.text.as_str())
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "reply is unavailable"))?,
        };
        if self.comments.drafts.len() >= MAX_DRAFTS || self.comments.bytes().saturating_add(text.len()) > MAX_DRAFT_BYTES {
            return Err(Error::new(ErrorKind::Conflict, "Unsaved draft limit reached. Save or discard another draft first."));
        }
        let (node, pos) = anchor.unwrap_or_else(|| root.map_or((None, Position::default()), |root| (root.node, root.pos)));
        let draft = Draft {
            session: self.session.view_identity(),
            project: self.session.document().id(),
            canvas: self.canvas,
            revision: self.session.revision(),
            target,
            text: text.to_owned(),
            editor_height: 0.0,
            pos,
            node,
            dirty: matches!(target, Target::Create(_) | Target::NewReply(..)),
            error: None,
        };
        let key = self.comments.add_draft(draft)?;
        self.comments.focus_pending = Some(key);
        self.comments.active = Some(target.root());
        ctx.request_repaint();
        Ok(())
    }
    fn save_discussion(&mut self, ctx: &egui::Context, as_new: bool) {
        let Some(key) = self.comments.editing else {
            return;
        };
        let Some(draft) = self.comments.drafts.get(&key) else {
            return;
        };
        if !as_new
            && (!Arc::ptr_eq(&draft.session, &self.session.view_identity())
                || draft.project != self.session.document().id()
                || draft.canvas != self.canvas
                || draft.revision != self.session.revision())
        {
            if let Some(draft) = self.comments.drafts.get_mut(&key) {
                draft.error = Some("Document changed. Keep this draft, reload explicitly, or save it as a new canvas discussion.".into());
            }
            return;
        }
        let new_id = if as_new {
            match self.comments.allocate_comment(self.session.document()) {
                Ok(id) => Some(id),
                Err(error) => {
                    self.notice = Some(error.message);
                    return;
                }
            }
        } else {
            None
        };
        let Some(draft) = self.comments.drafts.get(&key) else {
            return;
        };
        if draft.text.len() > draft.limit() {
            self.notice = Some("Message exceeds its UTF-8 byte limit.".into());
            return;
        }
        let target = draft.target;
        let revision = if as_new { self.session.revision() } else { draft.revision };
        let (command, params) = if let Some(id) = new_id {
            (
                "comment.create",
                json!({"canvas":self.canvas,"id":id,"text":draft.text,"pos":{"x":self.view.to_world(self.viewport.min,self.viewport.center()).x,"y":self.view.to_world(self.viewport.min,self.viewport.center()).y},"expected_revision":revision}),
            )
        } else {
            match target {
                Target::Create(id) => {
                    ("comment.create", json!({"canvas":self.canvas,"id":id,"text":draft.text,"pos":draft.pos,"node":draft.node,"expected_revision":revision}))
                }
                Target::Root(id) => ("comment.edit", json!({"canvas":self.canvas,"id":id,"text":draft.text,"expected_revision":revision})),
                Target::Reply(id, reply) => {
                    ("comment.edit", json!({"canvas":self.canvas,"id":id,"reply":reply,"text":draft.text,"expected_revision":revision}))
                }
                Target::NewReply(id, reply) => {
                    ("comment.reply", json!({"canvas":self.canvas,"id":id,"reply":reply,"text":draft.text,"expected_revision":revision}))
                }
            }
        };
        match self.session.dispatch(command, &params) {
            Ok(_) => {
                self.comments.drafts.remove(&key);
                self.comments.editing = None;
                self.comments.active = Some(new_id.unwrap_or_else(|| target.root()));
                self.notice = None;
                ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("discussion_edit").with(key)));
            }
            Err(error) => {
                if let Some(draft) = self.comments.drafts.get_mut(&key) {
                    draft.error = Some(error.message);
                }
            }
        }
    }
    fn discussion_action(&mut self, ctx: &egui::Context, action: Action) {
        if !matches!(action, Action::Cancel | Action::Close | Action::Discard(_)) && self.discussion_context(ctx, None).blocked {
            self.notice = Some("Finish the active editor, popup or gesture before using discussion controls.".into());
            return;
        }
        let mut result = Ok(());
        match action {
            Action::Open(id) => result = self.dispatch_comments(ctx, "comment.open", &json!({"id":id})).map(|_| ()),
            Action::Compose(Some(node)) => {
                self.compose_on_node(ctx, node, None);
            }
            Action::Compose(None) => {
                let world = self.view.to_world(self.viewport.min, self.viewport.center());
                result = self.dispatch_comments(ctx, "comment.compose", &json!({"pos":{"x":world.x,"y":world.y}})).map(|_| ());
            }
            Action::Note => {
                result = (|| {
                    if self.discussion_context(ctx, None).blocked {
                        return Err(Error::new(ErrorKind::Conflict, "finish active editor or gesture before adding a Note"));
                    }
                    let id = crate::ids::note_id(self.session.document())?;
                    let mut node = self.session.kinds().get("structure.note")?.instantiate_instance(id, Default::default(), &Default::default())?;
                    let pos = self.view.to_world(self.viewport.min, self.viewport.center());
                    node.pos = Position { x: pos.x, y: pos.y };
                    self.session.dispatch("node.add", &json!({"canvas":self.canvas,"node":node}))?;
                    self.selection.only(id);
                    Ok(())
                })();
            }
            Action::Edit(target) => result = self.begin_draft(ctx, target, None),
            Action::Save => self.save_discussion(ctx, false),
            Action::SaveNew => self.save_discussion(ctx, true),
            Action::Cancel => {
                if let Some(key) = self.comments.editing.take() {
                    self.comments.drafts.remove(&key);
                    ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("discussion_edit").with(key)));
                }
            }
            Action::Resume(key) => {
                if self.comments.drafts.contains_key(&key) {
                    self.comments.editing = Some(key);
                    self.comments.focus_pending = Some(key);
                }
            }
            Action::Close => self.comments.close(),
            Action::Discard(key) => {
                self.comments.drafts.remove(&key);
                if self.comments.editing == Some(key) {
                    self.comments.editing = None;
                }
            }
            Action::Reload => {
                if let Some(key) = self.comments.editing.take()
                    && let Some(draft) = self.comments.drafts.remove(&key)
                {
                    let target = draft.target;
                    if matches!(target, Target::Create(_)) {
                        self.comments.drafts.insert(key, draft);
                        self.comments.editing = Some(key);
                        result = Err(Error::new(ErrorKind::Conflict, "New discussion has no stored message to reload; save as new or discard explicitly"));
                    } else if let Err(error) = self.begin_draft(ctx, target, None) {
                        self.comments.drafts.insert(key, draft);
                        self.comments.editing = Some(key);
                        result = Err(error);
                    }
                }
            }
            Action::Resolve(resolved) => {
                if let Some(id) = self.comments.active {
                    result = self
                        .session
                        .dispatch("comment.resolve", &json!({"canvas":self.canvas,"id":id,"resolved":resolved,"expected_revision":self.session.revision()}))
                        .map(|_| ());
                }
            }
            Action::RemoveReply(reply) => {
                if let Some(id) = self.comments.active {
                    result = self
                        .session
                        .dispatch("comment.reply_remove", &json!({"canvas":self.canvas,"id":id,"reply":reply,"expected_revision":self.session.revision()}))
                        .map(|_| ());
                }
            }
            Action::Remove => {
                if let Some(id) = self.comments.active {
                    result =
                        self.session.dispatch("comment.remove", &json!({"canvas":self.canvas,"id":id,"expected_revision":self.session.revision()})).map(|_| ());
                    if result.is_ok() {
                        self.comments.active = None;
                        self.comments.placement = None;
                    }
                }
            }
            Action::Detach | Action::Attach(_) => {
                if let Some(id) = self.comments.active {
                    result = (|| {
                        let scene =
                            self.session.document().canvases().get(&self.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas unavailable"))?;
                        let root = scene.comments().get(&id).ok_or_else(|| Error::new(ErrorKind::NotFound, "discussion unavailable"))?;
                        let world = root.world_position(scene)?;
                        let (node, pos) = if let Action::Attach(node) = action {
                            let n = scene.nodes().get(&node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node unavailable"))?;
                            (Some(node), Position { x: world.x - n.pos.x, y: world.y - n.pos.y })
                        } else {
                            (None, world)
                        };
                        self.session.dispatch(
                            "comment.move",
                            &json!({"canvas":self.canvas,"id":id,"node":node,"pos":pos,"expected_revision":self.session.revision()}),
                        )?;
                        Ok(())
                    })();
                }
            }
        }
        if let Err(error) = result {
            self.notice = Some(error.message);
        }
        ctx.request_repaint();
    }
    pub(super) fn comments_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let ctx = ui.ctx().clone();
        let t = self.theme.tokens();
        if self.comments.drag.as_ref().is_some_and(|drag| {
            !Arc::ptr_eq(&drag.session, &self.session.view_identity()) || drag.canvas != self.canvas || drag.revision != self.session.revision()
        }) {
            self.comments.drag = None;
            self.comments.gesture_cancelled = ctx.input(|i| i.pointer.primary_down());
        }
        if self.comments.gesture_cancelled && !ctx.input(|i| i.pointer.primary_down()) {
            self.comments.gesture_cancelled = false;
        }
        let own_focus = ctx.memory(|m| m.focused()).map_or_else(
            || self.comments.last_focus.is_some() && self.comments.last_focus == self.last_text_focus,
            |id| self.comments.drafts.keys().any(|key| id == egui::Id::new("discussion_edit").with(key)),
        );

        if self.comments.drag.is_some()
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
            && let Some(drag) = &mut self.comments.drag
        {
            drag.cancelled = true;
            drag.pos = drag.origin;
        }
        if self.comments.open
            && !egui::Popup::is_any_open(&ctx)
            && !self.discussion_context(&ctx, None).blocked
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            self.discussion_action(&ctx, if own_focus { Action::Cancel } else { Action::Close });
        }
        if !canvas::text_input_focused(&ctx) && !self.text_focused && !self.discussion_context(&ctx, None).blocked && ui.input_mut(|i| consume_shortcut(i, "C"))
        {
            let pos =
                self.view.to_world(self.viewport.min, ctx.pointer_hover_pos().filter(|pos| self.viewport.contains(*pos)).unwrap_or(self.viewport.center()));
            if let Err(error) = self.dispatch_comments(&ctx, "comment.compose", &json!({"pos":{"x":pos.x,"y":pos.y}})) {
                self.notice = Some(error.message);
            }
        }
        let available = !self.discussion_context(&ctx, None).blocked;
        let toolbar_rect = self.comments_toolbar_rect();
        let toolbar = egui::Area::new(egui::Id::new("comments_toolbar"))
            .order(egui::Order::Foreground)
            .movable(false)
            .fixed_pos(toolbar_rect.min)
            .constrain(false)
            .show(&ctx, |ui| ui.add_enabled(available, crate::icons::Icon::Comments.button(ui, "Comments", t).min_size(toolbar_rect.size())));
        // A short viewport can put this row over the minimap. Keep actionable chrome
        // above the map without raising it over an unrelated popup.
        if !egui::Popup::is_any_open(&ctx) && toolbar.response.rect.intersects(crate::navigation::layout(self.viewport, self.navigation.visible, t).overlay) {
            ctx.move_to_top(toolbar.response.layer_id);
        }
        let exposed =
            ctx.pointer_interact_pos().is_some_and(|pos| toolbar.response.rect.contains(pos) && ctx.layer_id_at(pos) == Some(toolbar.response.layer_id));
        if !available
            && self.text_focused
            && !egui::Popup::is_any_open(&ctx)
            && exposed
            && ctx.memory(|m| m.focused()).is_none_or(|id| Some(id) == self.last_text_focus)
            && ctx.input(|i| i.pointer.any_pressed() || i.pointer.any_released())
            && let Some(id) = self.last_text_focus
        {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.options_mut(|options| options.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        if toolbar.inner.clicked()
            && let Err(error) = self.dispatch_comments(&ctx, "canvas.comments_toggle", &json!({}))
        {
            self.notice = Some(error.message);
        }
        let mut blocked = exposed || self.comments.gesture_cancelled;
        let identity = self.session.view_identity();
        if !self.comments.session.as_ref().is_some_and(|previous| Arc::ptr_eq(previous, &identity)) {
            self.comments.session = Some(identity);
            self.comments.stamp = None;
            self.comments.drag = None;
            self.comments.active = None;
        }
        let stamp = (self.session.document().id(), self.canvas, self.session.revision());
        if self.comments.stamp != Some(stamp) {
            self.comments.pins.clear();
            if let Some(scene) = self.session.document().canvases().get(&self.canvas) {
                for root in scene.comments().values() {
                    if let Ok(pos) = root.world_position(scene) {
                        self.comments.pins.insert(root.id, pos);
                    }
                }
            }
            self.comments.stamp = Some(stamp);
        }
        let mut action = None;
        // Navigation's checked instance cache uses card::world_rect, including compact bodies.
        // Never infer a body from declared parameter counts or persist this display correction.
        self.navigation.geometry.refresh(&mut self.session, self.canvas, self.theme);
        let membership = self.membership();
        let field_shift = self.field_shift();
        self.comments.marker_ids.clear();
        if let Some(scene) = self.session.document().canvases().get(&self.canvas) {
            let mut visible = 0usize;
            let mut coincident: BTreeMap<(i32, i32), usize> = BTreeMap::new();
            for (id, pos) in &self.comments.pins {
                let Some(root) = scene.comments().get(id) else {
                    continue;
                };
                if root.resolved && !self.comments.show_resolved && !(self.comments.open && self.comments.active == Some(*id)) {
                    continue;
                }
                let pos = self.comments.drag.as_ref().filter(|drag| drag.id == *id).map_or(*pos, |drag| {
                    if let Some(node) = drag.node.and_then(|id| scene.nodes().get(&id)) {
                        Position { x: node.pos.x + drag.pos.x, y: node.pos.y + drag.pos.y }
                    } else {
                        drag.pos
                    }
                });
                let node_preview = root.node.and_then(|id| scene.nodes().get(&id)).and_then(|node| {
                    self.drag.as_ref().filter(|drag| drag.id == node.id).map(|drag| drag.pos).or_else(|| Self::field_preview(field_shift, node))
                });
                let pos = node_preview.map_or(pos, |preview| Position { x: preview.x + root.pos.x, y: preview.y + root.pos.y });
                let mut screen = self.view.to_screen(self.viewport.min, Pos2::new(pos.x, pos.y));
                let anchor_screen = screen;
                let mut node_bounds = None;
                let mut projection = None;
                if let Some(node) = root.node.and_then(|id| scene.nodes().get(&id)) {
                    if membership.hidden.contains(&node.id) {
                        continue;
                    }
                    if let Some(rect) = self.navigation.geometry.rects.get(&node.id) {
                        let shift = node_preview.map_or(Vec2::ZERO, |preview| Vec2::new(preview.x - node.pos.x, preview.y - node.pos.y));
                        let body = self.view.rect_to_screen(self.viewport.min, rect.translate(shift));
                        let hit = (t.handle_radius * self.view.zoom).max(t.handle_min_screen_radius) + card::PORT_HIT;
                        let right = if node.outputs.is_empty() { 0.0 } else { hit };
                        // Input-only instances need their left hit cells protected as well.
                        let left = if node.inputs.is_empty() { right } else { hit };
                        let top = card::outer_insets(t, self.view.zoom, !node.outputs.is_empty()).1 * self.view.zoom;
                        let decorated = Rect::from_min_max(body.min - Vec2::new(left, top), body.max + Vec2::new(right, right));
                        node_bounds = Some(decorated);
                        if decorated.intersects(Rect::from_min_size(screen, Vec2::splat(t.comment_marker_size)).expand(t.comment_marker_ring)) {
                            screen = projected_marker(screen, body, decorated, self.viewport, t);
                            projection = Some((body, decorated));
                        }
                    }
                }
                let dragging = self.comments.drag.as_ref().filter(|drag| drag.id == *id && !drag.cancelled);
                let placement = self
                    .comments
                    .placement
                    .filter(|(placed, position, _)| *placed == *id && *position == root.pos && self.comments.open && self.comments.active == Some(*id));
                let key = ((screen.x / t.comment_marker_size).floor() as i32, (screen.y / t.comment_marker_size).floor() as i32);
                let count = coincident.entry(key).or_default();
                let mut jitter = Vec2::new((*count % 4) as f32 * t.navigation_gap, (*count / 4) as f32 * t.navigation_gap);
                if let Some((body, bounds)) = projection {
                    // A left/above fallback must scatter away from, not back into, port cells.
                    if screen.x + t.comment_marker_size < bounds.left() {
                        jitter.x = -jitter.x;
                    }
                    if screen.y + t.comment_marker_size < bounds.top() {
                        jitter.y = -jitter.y;
                    }
                    screen += jitter;
                    let marker = Rect::from_min_size(screen, Vec2::splat(t.comment_marker_size)).expand(t.comment_marker_ring);
                    if !self.viewport.contains_rect(marker) || bounds.intersects(marker) {
                        screen = projected_marker(screen, body, bounds, self.viewport, t);
                    }
                } else {
                    screen += jitter;
                }
                *count = count.saturating_add(1);
                // Keep the selected bubble visible beside its panel. A leader preserves
                // the actual persisted anchor; this displacement is view state only.
                if let Some(drag) = dragging {
                    // Keep the original display displacement through the gesture. Re-projecting
                    // each moving hidden anchor would pin the bubble against the panel edge.
                    let delta = ctx.pointer_interact_pos().map_or(Vec2::ZERO, |pointer| pointer - drag.start);
                    screen = drag.display_origin + delta;
                } else if let Some((_, _, offset)) = placement {
                    screen = anchor_screen + offset;
                } else if self.comments.open && self.comments.active == Some(*id) {
                    let panel = self.discussion_rect();
                    if panel.intersects(Rect::from_min_size(screen, Vec2::splat(t.comment_marker_size)).expand(t.comment_marker_ring))
                        || panel.intersects(Rect::from_min_size(anchor_screen, Vec2::splat(t.comment_marker_size)))
                    {
                        let left = panel.left() - t.comment_marker_gap - t.comment_marker_size - t.comment_marker_ring;
                        if left >= self.viewport.left() + t.navigation_margin {
                            screen.x = left;
                        } else {
                            screen.x = panel.left();
                            screen.y = panel.top() - t.comment_marker_size - t.navigation_gap;
                        }
                        // Beside the panel must not mean on top of a node's preview or title.
                        // Keep the reachable-marker fallback only when neither vertical side fits.
                        if let Some(bounds) = node_bounds
                            && bounds.intersects(Rect::from_min_size(screen, Vec2::splat(t.comment_marker_size)).expand(t.comment_marker_ring))
                        {
                            let above = bounds.top() - t.comment_marker_size - t.comment_marker_gap - t.comment_marker_ring;
                            let below = bounds.bottom() + t.comment_marker_gap + t.comment_marker_ring;
                            if above >= self.viewport.top() + t.navigation_margin {
                                screen.y = above;
                            } else if below + t.comment_marker_size <= self.viewport.bottom() - t.navigation_margin {
                                screen.y = below;
                            }
                        }
                    }
                }
                if !self.viewport.expand(t.comment_marker_size).contains(screen) {
                    continue;
                }
                if visible >= 128 {
                    break;
                }
                visible += 1;
                if self.comments.open && self.comments.active == Some(*id) && screen.distance(anchor_screen) > t.comment_marker_ring {
                    ui.painter().line_segment([anchor_screen, screen], egui::Stroke::new(t.handle_stroke, t.text_muted));
                }
                // Clamping an offscreen badge would put it back over the card/output handle.
                let area = egui::Area::new(egui::Id::new("comment_pin").with(id))
                    .order(egui::Order::Foreground)
                    .movable(false)
                    .constrain(false)
                    .fixed_pos(screen)
                    .show(&ctx, |ui| {
                        let response = ui
                            .add_enabled_ui(available, |ui| {
                                crate::comments::pin_bubble(
                                    ui,
                                    t,
                                    root.resolved,
                                    self.comments.open && self.comments.active == Some(*id),
                                    root.replies.len() + 1,
                                )
                            })
                            .inner
                            .on_hover_text(format!(
                                "{} — {} messages{}",
                                crate::comments::preview(&root.text),
                                root.replies.len() + 1,
                                if root.resolved { " (resolved; reopen in Comments)" } else { " (open discussion)" }
                            ));
                        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Discussion pin {id}")));
                        response
                    });
                let response = area.inner;
                self.comments.marker_ids.push(response.id);
                let top = ctx.pointer_interact_pos().is_some_and(|pos| response.rect.contains(pos) && ctx.layer_id_at(pos) == Some(response.layer_id));
                blocked |= top;
                if response.clicked() && available {
                    action = Some(Action::Open(*id));
                }
                if response.drag_started_by(egui::PointerButton::Primary)
                    && available
                    && let Some(start) = ctx.input(|i| i.pointer.press_origin())
                {
                    self.comments.drag = Some(PinDrag {
                        session: self.session.view_identity(),
                        canvas: self.canvas,
                        id: *id,
                        revision: self.session.revision(),
                        pos: root.pos,
                        node: root.node,
                        start,
                        origin: root.pos,
                        offset: screen - anchor_screen,
                        display_origin: screen,
                        cancelled: false,
                    });
                }
            }
        }
        if let Some(drag) = &mut self.comments.drag {
            blocked = true;
            if !drag.cancelled
                && let Some(pointer) = ctx.pointer_interact_pos()
            {
                let delta = (pointer - drag.start) / self.view.zoom;
                drag.pos = Position { x: drag.origin.x + delta.x, y: drag.origin.y + delta.y };
            }
            if !ctx.input(|i| i.pointer.primary_down())
                && let Some(drag) = self.comments.drag.take()
                && !drag.cancelled
            {
                match self
                    .session
                    .dispatch("comment.move", &json!({"canvas":self.canvas,"id":drag.id,"node":drag.node,"pos":drag.pos,"expected_revision":drag.revision}))
                {
                    Ok(_) => self.comments.placement = Some((drag.id, drag.pos, drag.offset)),
                    Err(error) => self.notice = Some(error.message),
                }
            }
        }

        if self.comments.open {
            let rect = self.discussion_rect();
            let width = rect.width();
            let height = rect.height();
            let panel = egui::Area::new(egui::Id::new("discussions_panel"))
                .order(egui::Order::Foreground)
                .movable(false)
                .fixed_pos(rect.min)
                .constrain(false)
                .show(&ctx, |ui| {
                    if !available {
                        ui.disable();
                    }
                    egui::Frame::NONE
                        .fill(t.panel_fill)
                        .stroke(egui::Stroke::new(t.handle_stroke, t.panel_stroke))
                        .corner_radius(t.panel_radius)
                        .inner_margin(t.card_padding)
                        .show(ui, |ui| {
                            ui.set_width((width - 2.0 * t.card_padding).max(1.0));
                            ui.set_max_height(height);
                            egui::ScrollArea::vertical().max_height((height - 2.0 * t.card_padding).max(1.0)).show(ui, |ui| {
                                ui.heading("Comments · Local");
                                if ui.button("Close comments").clicked() {
                                    action = Some(Action::Close);
                                }
                                ui.add_enabled_ui(available, |ui| {
                                    ui.horizontal_wrapped(|ui| {
                                        if ui.button("Pin on canvas").clicked() {
                                            action = Some(Action::Compose(None));
                                        }
                                        if self.selection.len() == 1
                                            && ui.button("Pin on selected node").clicked()
                                            && let Some(id) = self.selection.nodes().iter().next()
                                        {
                                            action = Some(Action::Compose(Some(*id)));
                                        }
                                        if ui.button("Add Note").clicked() {
                                            action = Some(Action::Note);
                                        }
                                    });
                                });
                                ui.scope(|ui| {
                                    crate::comments::checkbox_visuals(ui.visuals_mut(), t);
                                    ui.checkbox(&mut self.comments.show_resolved, "Show resolved");
                                });
                                let scene = self.session.document().canvases().get(&self.canvas);
                                if let Some(scene) = scene {
                                    let mut count = 0usize;
                                    for root in scene
                                        .comments()
                                        .values()
                                        .filter(|root| self.comments.show_resolved || !root.resolved)
                                        .skip(self.comments.page.saturating_mul(MESSAGE_PAGE))
                                        .take(MESSAGE_PAGE)
                                    {
                                        count += 1;
                                        if ui
                                            .add(
                                                egui::Button::new(format!(
                                                    "{}{} · {}",
                                                    if root.resolved { "Resolved · " } else { "" },
                                                    crate::comments::preview(&root.text),
                                                    crate::comments::message_count(root.replies.len() + 1)
                                                ))
                                                .wrap(),
                                            )
                                            .clicked()
                                        {
                                            action = Some(Action::Open(root.id));
                                        }
                                    }
                                    ui.horizontal(|ui| {
                                        if self.comments.page > 0 && ui.button("Previous discussions").clicked() {
                                            self.comments.page = self.comments.page.saturating_sub(1);
                                        }
                                        if count == MESSAGE_PAGE && ui.button("More discussions").clicked() {
                                            self.comments.page = self.comments.page.saturating_add(1);
                                        }
                                    });
                                    if let Some(root) = self.comments.active.and_then(|id| scene.comments().get(&id)) {
                                        ui.separator();
                                        ui.label(if root.node.is_some() { "Pinned to node" } else { "Pinned to canvas" });
                                        ui.label(crate::comments::preview(&root.text));
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Edit root").clicked() {
                                                action = Some(Action::Edit(Target::Root(root.id)));
                                            }
                                            if ui.button(if root.resolved { "Reopen" } else { "Resolve" }).clicked() {
                                                action = Some(Action::Resolve(!root.resolved));
                                            }
                                            if ui.button("Delete discussion").clicked() {
                                                action = Some(Action::Remove);
                                            }
                                            if root.node.is_some() && ui.button("Detach pin").clicked() {
                                                action = Some(Action::Detach);
                                            }
                                            if self.selection.len() == 1
                                                && ui.button("Attach to selected node").clicked()
                                                && let Some(id) = self.selection.nodes().iter().next()
                                            {
                                                action = Some(Action::Attach(*id));
                                            }
                                        });
                                        if ui.button("Read root message").clicked() {
                                            self.comments.message = Some(root.id);
                                            self.comments.chunk = 0;
                                        }
                                        let page = self
                                            .comments
                                            .message
                                            .and_then(|id| root.replies.iter().position(|reply| reply.id == id))
                                            .map_or(0, |index| index / MESSAGE_PAGE);
                                        for (index, reply) in root.replies.iter().enumerate().skip(page.saturating_mul(MESSAGE_PAGE)).take(MESSAGE_PAGE) {
                                            ui.label(crate::comments::preview(&reply.text));
                                            ui.push_id(reply.id, |ui| {
                                                ui.horizontal_wrapped(|ui| {
                                                    if ui.button(format!("Read reply {}", index + 1)).clicked() {
                                                        self.comments.message = Some(reply.id);
                                                        self.comments.chunk = 0;
                                                    }
                                                    if ui.button(format!("Edit reply {}", index + 1)).clicked() {
                                                        action = Some(Action::Edit(Target::Reply(root.id, reply.id)));
                                                    }
                                                    if ui.button(format!("Delete reply {}", index + 1)).clicked() {
                                                        action = Some(Action::RemoveReply(reply.id));
                                                    }
                                                })
                                            });
                                        }
                                        if page > 0 && ui.button("Previous messages").clicked() {
                                            self.comments.message = root.replies.get((page - 1) * MESSAGE_PAGE).map(|reply| reply.id);
                                            self.comments.chunk = 0;
                                        }
                                        if root.replies.len() > (page + 1) * MESSAGE_PAGE && ui.button("More messages").clicked() {
                                            self.comments.message = root.replies.get((page + 1) * MESSAGE_PAGE).map(|reply| reply.id);
                                            self.comments.chunk = 0;
                                        }
                                        if !root.resolved && ui.button("Reply").clicked() {
                                            match self.comments.allocate_comment(self.session.document()) {
                                                Ok(id) => action = Some(Action::Edit(Target::NewReply(root.id, id))),
                                                Err(error) => self.notice = Some(error.message),
                                            }
                                        }
                                        if let Some(id) = self.comments.message {
                                            let text = if id == root.id {
                                                Some(root.text.as_str())
                                            } else {
                                                root.replies.iter().find(|reply| reply.id == id).map(|reply| reply.text.as_str())
                                            };
                                            if let Some(text) = text {
                                                let (chunk, end) = crate::comments::chunk(text, self.comments.chunk);
                                                ui.label(chunk);
                                                if end < text.len() && ui.button("Next text chunk").clicked() {
                                                    self.comments.chunk = end;
                                                }
                                                if self.comments.chunk > 0 && ui.button("Read from start").clicked() {
                                                    self.comments.chunk = 0;
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Some(key) = self.comments.editing {
                                    let other_bytes =
                                        self.comments.bytes().saturating_sub(self.comments.drafts.get(&key).map_or(0, |draft| draft.text.capacity()));
                                    if let Some(draft) = self.comments.drafts.get_mut(&key) {
                                        ui.separator();
                                        ui.label(match draft.target {
                                            Target::Create(_) => "New discussion draft",
                                            Target::NewReply(..) => "Reply draft",
                                            Target::Root(_) => "Edit root message",
                                            Target::Reply(..) => "Edit reply",
                                        });
                                        ui.label("Save explicitly; leaving focus keeps the draft");
                                        let id = egui::Id::new("discussion_edit").with(key);
                                        let mut rejected = false;
                                        let ceiling = draft.limit().saturating_add(4).min(MAX_DRAFT_BYTES.saturating_sub(other_bytes));
                                        let label = ui.label("Discussion message");
                                        let editing = self.comments.focus_pending == Some(key) || ctx.memory(|m| m.focused()) == Some(id);
                                        let response = if editing {
                                            let editor_height = t.body_size * t.text_rows as f32 + 2.0 * t.control_gap;
                                            let area = egui::ScrollArea::vertical()
                                                .id_salt(("discussion_buffer", key))
                                                .max_height(editor_height)
                                                .auto_shrink([false, false])
                                                .show(ui, |ui| {
                                                    ui.add(
                                                        egui::TextEdit::multiline(&mut crate::comments::Buffer::new(&mut draft.text, ceiling, &mut rejected))
                                                            .id(id)
                                                            .desired_rows(t.text_rows)
                                                            .desired_width(f32::INFINITY),
                                                    )
                                                    .labelled_by(label.id)
                                                });
                                            draft.editor_height = area.inner_rect.height();
                                            let response = area.inner;
                                            // Retain only our bounded draft, not egui's up-to100 full String
                                            // snapshots. Cancel and accepted-document undo are explicit.
                                            if let Some(mut state) = egui::TextEdit::load_state(&ctx, id) {
                                                state.clear_undoer();
                                                state.store(&ctx, id);
                                            }
                                            response
                                        } else {
                                            let height = draft.editor_height.max(t.body_size * t.text_rows as f32);
                                            ui.allocate_ui(Vec2::new(ui.available_width(), height), |ui| {
                                                ui.set_min_height(height);
                                                ui.add(egui::Label::new(crate::comments::preview(&draft.text)).truncate());
                                                let response = ui.button("Continue editing message");
                                                if response.clicked() {
                                                    self.comments.focus_pending = Some(key);
                                                    ctx.request_repaint();
                                                }
                                                response
                                            })
                                            .inner
                                        };
                                        if self.comments.focus_pending == Some(key) && ui.is_visible() {
                                            if response.has_focus() {
                                                self.comments.focus_pending = None;
                                            } else {
                                                response.request_focus();
                                                ctx.request_repaint();
                                            }
                                        }
                                        draft.dirty |= response.changed();
                                        if rejected {
                                            draft.error = Some("Paste exceeds the message or total unsaved-draft byte limit; nothing was truncated.".into());
                                        }
                                        if !Arc::ptr_eq(&draft.session, &self.session.view_identity())
                                            || draft.revision != self.session.revision()
                                            || draft.project != self.session.document().id()
                                            || draft.canvas != self.canvas
                                        {
                                            ui.label("Document changed. Review/reload or save as a new discussion; this draft is retained.");
                                        }
                                        if let Some(error) = &draft.error {
                                            ui.colored_label(t.accent, error);
                                        }
                                        ui.horizontal_wrapped(|ui| {
                                            if ui.button("Save message").clicked() {
                                                action = Some(Action::Save);
                                            }
                                            if ui.button("Cancel draft").clicked() {
                                                action = Some(Action::Cancel);
                                            }
                                            if ui.button("Reload current message").clicked() {
                                                action = Some(Action::Reload);
                                            }
                                            if ui.button("Save as new discussion").clicked() {
                                                action = Some(Action::SaveNew);
                                            }
                                        });
                                        if (response.has_focus() || response.lost_focus())
                                            && ui.input_mut(|i| {
                                                i.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
                                                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)
                                            })
                                        {
                                            action = Some(Action::Save);
                                        }
                                    }
                                }
                                if !self.comments.drafts.is_empty() {
                                    ui.separator();
                                    ui.label("Retained unsaved drafts");
                                    for (key, draft) in &self.comments.drafts {
                                        ui.horizontal(|ui| {
                                            if ui.button(format!("Resume draft {key}")).clicked() {
                                                action = Some(Action::Resume(*key));
                                            }
                                            if ui.button(format!("Discard draft {key}")).clicked() {
                                                action = Some(Action::Discard(*key));
                                            }
                                            if !Arc::ptr_eq(&draft.session, &self.session.view_identity())
                                                || draft.project != self.session.document().id()
                                                || draft.canvas != self.canvas
                                            {
                                                ui.label("orphaned");
                                            }
                                        });
                                    }
                                }
                            });
                        });
                });
            if !egui::Popup::is_any_open(&ctx) && panel.response.rect.intersects(crate::navigation::layout(self.viewport, self.navigation.visible, t).overlay) {
                ctx.move_to_top(panel.response.layer_id);
            }
            blocked |= ctx.pointer_interact_pos().is_some_and(|pos| panel.response.rect.contains(pos) && ctx.layer_id_at(pos) == Some(panel.response.layer_id));
        }
        if !available
            && blocked
            && self.text_focused
            && !egui::Popup::is_any_open(&ctx)
            && ctx.memory(|m| m.focused()).is_none_or(|id| Some(id) == self.last_text_focus)
            && ctx.input(|i| i.pointer.any_pressed() || i.pointer.any_released())
            && let Some(id) = self.last_text_focus
        {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.options_mut(|options| options.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        if let Some(action) = action {
            self.discussion_action(&ctx, action);
        }
        let focused = ctx.memory(|m| m.focused());
        self.comments.last_focus = focused.filter(|id| self.comments.drafts.keys().any(|key| *id == egui::Id::new("discussion_edit").with(key)));
        blocked
    }
}

#[cfg(test)]
#[path = "comment_layout_tests.rs"]
mod layout_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable};
    fn harness() -> Harness<'static, NodeCraftApp> {
        let base = NodeCraftApp::headless().unwrap();
        let canvas = base.canvas;
        let document = base.session.document().clone();
        let mut registry = nodecraft_engine::commands::Registry::builtins().unwrap();
        registry
            .register(nodecraft_engine::commands::CommandSpec {
                id: "test.replace_thread",
                label: "Replace thread",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "{}",
                enabled: |_, _| Ok(()),
                run: |_, value| {
                    let canvas: CanvasId = serde_json::from_value(value["canvas"].clone()).unwrap();
                    let comment: nodecraft_graph::Comment = serde_json::from_value(value["comment"].clone()).unwrap();
                    Ok(nodecraft_engine::commands::Effect::Edit {
                        changes: nodecraft_graph::Changeset::new(vec![
                            Change::RemoveComment { canvas, id: comment.id },
                            Change::PutComment { canvas, comment },
                        ]),
                        result: Json::Null,
                        gesture: None,
                    })
                },
            })
            .unwrap();
        let app = NodeCraftApp::from_session(Session::with_registry(document, registry).unwrap(), canvas).unwrap();
        Harness::builder().with_size(Vec2::new(1100.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
    }
    fn compose(h: &mut Harness<'static, NodeCraftApp>, text: &str) {
        h.key_press(egui::Key::C);
        h.run();
        h.event(egui::Event::Text(text.into()));
        h.run();
        assert_eq!(h.state().comments.drafts.get(&h.state().comments.editing.unwrap()).unwrap().text, text, "composition should focus its own editor");
    }
    #[test]
    #[ignore = "renders public review evidence to NODECRAFT_COMMENTS_EVIDENCE"]
    fn render_comments_review_evidence() {
        let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_COMMENTS_EVIDENCE").unwrap());
        std::fs::create_dir_all(&folder).unwrap();
        let mut h = Harness::builder()
            .wgpu()
            .with_size(Vec2::new(1200.0, 850.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        h.render().unwrap().save(folder.join("before.png")).unwrap();
        compose(&mut h, "Review the dawn colors");
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Reply").click();
        h.run();
        h.event(egui::Event::Text("Keep the soft light in the next pass.".into()));
        h.run();
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Add Note").click();
        h.run();
        let note =
            h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().find(|node| node.kind == "structure.note").unwrap().id;
        let world = h.state().view.to_world(h.state().viewport.min, Pos2::new(840.0, 350.0));
        let canvas = h.state().canvas;
        h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":note,"pos":{"x":world.x,"y":world.y}})).unwrap();
        h.run();
        h.render().unwrap().save(folder.join("discussion-note.png")).unwrap();
        h.get_by_role_and_label(Role::Button, "Resolve").click();
        h.run();
        let resolved = h.state().session.document().clone();
        h.render().unwrap().save(folder.join("resolved.png")).unwrap();
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_ne!(h.state().session.document(), &resolved);
        h.render().unwrap().save(folder.join("undo.png")).unwrap();
        h.state_mut().theme = ThemeKind::Light;
        h.run();
        h.render().unwrap().save(folder.join("light.png")).unwrap();
    }
    #[test]
    fn file_hover_blocks_discussion_commands_until_it_leaves() {
        let mut h = harness();
        h.input_mut().hovered_files.push(egui::HoveredFile::default());
        h.step();
        let before = h.state().session.document().clone();
        let ctx = h.ctx.clone();
        assert!(h.state_mut().comments_enabled(&ctx, "canvas.comments_toggle", &json!({})).is_err());
        assert!(h.state_mut().dispatch_comments(&ctx, "canvas.comments_toggle", &json!({})).is_err());
        assert_eq!(h.state().session.document(), &before);
        h.input_mut().hovered_files.clear();
        h.run();
        h.state_mut().dispatch_comments(&ctx, "canvas.comments_toggle", &json!({})).unwrap();
        assert!(h.state().comments.open);
    }
    #[test]
    fn tag_controls_preserve_dirty_discussion_focus_and_replacement_keeps_orphan_identity() {
        let mut h = harness();
        compose(&mut h, "Unsaved UTF8 é discussion");
        let key = h.state().comments.editing.unwrap();
        let token = h.state().comments.drafts.get(&key).unwrap().session.clone();
        let before = h.state().session.document().clone();
        let owner = egui::Id::new("discussion_edit").with(key);
        h.get_by_role_and_label(Role::Button, "Tag filter").click();
        h.run();
        assert_eq!(h.ctx.memory(|m| m.focused()), Some(owner));
        assert!(!h.state().tag_filter.open);
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, "Unsaved UTF8 é discussion");
        let canvas = h.state().canvas;
        h.state_mut().replace_session(Session::new(before.clone()).unwrap(), canvas).unwrap();
        let draft = h.state().comments.drafts.get(&key).unwrap();
        assert!(Arc::ptr_eq(&draft.session, &token));
        assert!(!Arc::ptr_eq(&draft.session, &h.state().session.view_identity()));
        assert_eq!(draft.text, "Unsaved UTF8 é discussion");
        assert!(h.state().comments.drag.is_none() && h.state().comments.pins.is_empty());
        h.state_mut().comments.editing = Some(key);
        let ctx = h.ctx.clone();
        h.state_mut().save_discussion(&ctx, false);
        assert!(h.state().comments.drafts.get(&key).unwrap().error.is_some());
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn tag_and_discussion_panels_have_disjoint_bounds_and_pin_gestures_block_tag_navigation() {
        let mut h = harness();
        h.state_mut().comments.open = true;
        h.state_mut().tag_filter.open = true;
        h.run();
        let app = h.state();
        assert!(!app.tag_rect().intersects(app.discussion_rect()));
        assert!(app.discussion_rect().left() > app.tag_rect().right());
        h.state_mut().comments.gesture_cancelled = true;
        let ctx = h.ctx.clone();
        assert!(h.state_mut().tag_filter_enabled(&ctx, "canvas.tag_filter_open", &json!({})).is_err());
    }
    #[test]
    fn real_compose_reply_resolve_note_and_undo_use_shared_commands() {
        let mut h = harness();
        let original = h.state().session.document().clone();
        compose(&mut h, "A local discussion");
        assert_eq!(h.state().session.document(), &original);
        assert!(!h.state().session.can_undo());
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        assert_eq!(
            h.state().session.revision(),
            1,
            "notice={:?}, draft errors={:?}",
            h.state().notice,
            h.state().comments.drafts.values().map(|draft| draft.error.as_deref()).collect::<Vec<_>>()
        );
        let id = h.state().comments.active.unwrap();
        h.get_by_role_and_label(Role::Button, "Reply").click();
        h.run();
        h.event(egui::Event::Text("A reply".into()));
        h.run();
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().comments().get(&id).unwrap().replies.len(), 1);
        h.get_by_role_and_label(Role::Button, "Resolve").click();
        h.run();
        assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().comments().get(&id).unwrap().resolved);
        h.get_by_role_and_label(Role::CheckBox, "Show resolved").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Resolved · A local discussion · 2 messages");
        h.get_by_role_and_label(Role::Button, "Reopen").click();
        h.run();
        h.get_by_role_and_label(Role::Button, "Add Note").click();
        h.run();
        assert!(h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().any(|node| node.kind == "structure.note"));
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert!(!h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().any(|node| node.kind == "structure.note"));
    }
    #[test]
    fn escape_cancel_blur_retention_and_unrelated_node_editor_ownership() {
        let mut h = harness();
        let original = h.state().session.document().clone();
        compose(&mut h, "Unsaved discussion");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().comments.drafts.is_empty());
        assert_eq!(h.state().session.document(), &original);
        h.get_by_role_and_label(Role::Button, "Close comments").click();
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Text").click();
        h.run();
        h.event(egui::Event::Text("uncommitted node draft".into()));
        h.run();
        h.key_press(egui::Key::C);
        h.run();
        assert!(!h.state().comments.open);
        assert!(h.state().comments.drafts.is_empty());
        h.get_by_role_and_label(Role::Button, "Comments").click();
        h.run();
        assert_eq!(h.state().session.document(), &original);
        assert!(canvas::text_input_focused(&h.ctx));
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(h.state().session.document(), &original);
    }
    #[test]
    fn stale_same_id_recreation_never_overwrites_and_failed_reload_retains_orphan() {
        let mut h = harness();
        compose(&mut h, "Stored");
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        let id = h.state().comments.active.unwrap();
        h.get_by_role_and_label(Role::Button, "Edit root").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("Draft".into()));
        h.run();
        let key = h.state().comments.editing.unwrap();
        let canvas = h.state().canvas;
        let root = h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().clone();
        h.state_mut().session.dispatch("test.replace_thread", &json!({"canvas":canvas,"comment":root})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().save_discussion(&ctx, false);
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().text, "Stored");
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, "Draft");
        h.state_mut().session.dispatch("comment.remove", &json!({"canvas":canvas,"id":id})).unwrap();
        h.state_mut().discussion_action(&ctx, Action::Reload);
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, "Draft");
    }
}

#[cfg(test)]
mod more_tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable};
    fn harness() -> Harness<'static, NodeCraftApp> {
        Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap())
    }
    fn click_secondary(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2) {
        h.hover_at(pos);
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE });
            h.run();
        }
    }
    #[test]
    fn active_and_resolved_bubbles_and_filter_work_in_both_themes() {
        for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
            let mut h = harness();
            h.state_mut().theme = theme;
            crate::theme::apply(&h.ctx, theme);
            let canvas = h.state().canvas;
            let id = CommentId::from_bytes([9; 16]);
            h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Discuss framing","pos":{"x":0,"y":0}})).unwrap();
            h.run();
            let label = format!("Discussion pin {id}");
            h.get_by_role_and_label(Role::Button, &label).click();
            h.run();
            assert_eq!(h.state().comments.active, Some(id));
            let pin = h.get_by_role_and_label(Role::Button, &label).rect();
            let panel = h.ctx.memory(|m| m.area_rect(egui::Id::new("discussions_panel"))).unwrap();
            assert!(!panel.intersects(pin), "active bubble must stay beside the panel");
            let revision = h.state().session.revision();
            h.get_by_role_and_label(Role::Button, "Resolve").click();
            h.run();
            assert_eq!(h.state().session.revision(), revision + 1);
            assert!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().resolved);
            h.get_by_role_and_label(Role::Button, &label);
            h.get_by_role_and_label(Role::Button, "Reopen");
            h.get_by_role_and_label(Role::CheckBox, "Show resolved").click();
            h.run();
            assert!(h.state().comments.show_resolved);
            h.get_by_role_and_label(Role::Button, "Resolved · Discuss framing · 1 message");
            h.get_by_role_and_label(Role::CheckBox, "Show resolved").click();
            h.run();
            assert!(!h.state().comments.show_resolved);
            assert_eq!(h.state().session.revision(), revision + 1, "filter is pure view state");
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
            h.run();
            assert!(!h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().resolved);
            h.get_by_role_and_label(Role::Button, "Resolve");
            h.get_by_role_and_label(Role::Button, "Resolve").click();
            h.run();
            h.get_by_role_and_label(Role::Button, "Close comments").click();
            h.run();
            assert!(h.query_by_role_and_label(Role::Button, &label).is_none(), "closed resolved pins obey the filter");
        }
    }

    #[test]
    fn failed_entropy_preserves_document_history_and_existing_draft() {
        let mut h = harness();
        h.key_press(egui::Key::C);
        h.run();
        h.event(egui::Event::Text("Retain draft on allocation failure".into()));
        h.run();
        let key = h.state().comments.editing.unwrap();
        h.get_by_role_and_label(Role::CheckBox, "Show resolved").click();
        h.run();
        let document = h.state().session.document().clone();
        let history = (h.state().session.revision(), h.state().session.can_undo(), h.state().session.can_redo());
        let ctx = h.ctx.clone();
        crate::ids::FAIL_ENTROPY.with(|flag| flag.set(true));
        let result = h.state_mut().dispatch_comments(&ctx, "comment.compose", &json!({"pos":{"x":20,"y":30}}));
        h.state_mut().discussion_action(&ctx, Action::Note);
        crate::ids::FAIL_ENTROPY.with(|flag| flag.set(false));
        assert!(result.is_err());
        assert!(h.state().notice.as_ref().unwrap().contains("randomness"));
        assert_eq!(h.state().session.document(), &document);
        assert_eq!((h.state().session.revision(), h.state().session.can_undo(), h.state().session.can_redo()), history);
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, "Retain draft on allocation failure");
    }

    #[test]
    fn default_node_pin_offset_clears_body_and_header_at_low_and_high_zoom() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        let node = NodeId::from_bytes([3; 16]);
        for zoom in [0.15, 1.0, 2.0] {
            h.state_mut().view.zoom = zoom;
            h.state_mut().compose_on_node(&ctx, node, None);
            let draft = h.state().comments.drafts.get(&h.state().comments.editing.unwrap()).unwrap();
            let size = h.state().navigation.geometry.rects.get(&node).unwrap().size();
            let t = h.state().theme.tokens();
            assert!(draft.pos.x > size.x + t.comment_marker_ring / zoom);
            assert!(draft.pos.y > 0.0 && draft.pos.y < size.y);
            h.state_mut().discussion_action(&ctx, Action::Cancel);
        }
    }
    #[test]
    fn node_context_pin_follows_move_detaches_on_delete_and_restores_on_undo() {
        let mut h = harness();
        let node = NodeId::from_bytes([3; 16]);
        let point = h.get_by_role_and_label(Role::Button, "Node A quiet forest at dawn, soft light through the trees.").rect().min + Vec2::splat(6.0);
        click_secondary(&mut h, point);
        h.get_by_role_and_label(Role::Button, "Comment on this node").click();
        h.run();
        h.event(egui::Event::Text("Node discussion".into()));
        h.run();
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        let canvas = h.state().canvas;
        let id = h.state().comments.active.unwrap();
        let root = h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().clone();
        assert_eq!(root.node, Some(node));
        let ctx = h.ctx.clone();
        h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas,"id":node,"pos":{"x":200,"y":100}})).unwrap();
        h.run();
        let scene = h.state().session.document().canvases().get(&canvas).unwrap();
        assert_eq!(h.state().comments.pins[&id], root.world_position(scene).unwrap());
        h.state_mut().session.dispatch("node.remove", &json!({"canvas":canvas,"id":node})).unwrap();
        h.run();
        assert!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().node.is_none());
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().node, Some(node));
        assert!(h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).is_ok());
    }
    #[test]
    fn screen_readable_markers_drag_once_and_escape_cancels_without_selection_changes() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Canvas discussion","pos":{"x":0,"y":0}})).unwrap();
        h.run();
        let label = format!("Discussion pin {id}");
        for zoom in [0.15, 2.0] {
            h.state_mut().view.zoom = zoom;
            h.run();
            let rect = h.get_by_role_and_label(Role::Button, &label).rect();
            assert!(rect.width() >= h.state().theme.tokens().comment_marker_size && rect.height() >= h.state().theme.tokens().comment_marker_size);
        }
        h.state_mut().view.zoom = 1.0;
        h.run();
        let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
        let revision = h.state().session.revision();
        let selection = h.state().selection.clone();
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        h.hover_at(from + Vec2::new(140.0, 80.0));
        h.run();
        assert!(h.state().comments.drag.is_some());
        assert_eq!(h.state().session.revision(), revision);
        h.key_press(egui::Key::Escape);
        h.run();
        h.event(egui::Event::PointerButton {
            pos: from + Vec2::new(140.0, 80.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().selection, selection);
        let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        h.hover_at(from + Vec2::new(80.0, 50.0));
        h.run();
        h.event(egui::Event::PointerButton {
            pos: from + Vec2::new(80.0, 50.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert_eq!(h.state().session.revision(), revision + 1);
        assert_eq!(h.state().selection, selection);
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().pos, Position::default());
    }
    #[test]
    fn displaced_open_panel_pin_tracks_pointer_and_releases_without_jump() {
        for (width, zoom, pinned) in [(1100.0, 1.0, false), (1100.0, 2.0, true), (420.0, 0.5, true)] {
            let base = harness();
            let canvas = base.state().canvas;
            let app = NodeCraftApp::from_session(Session::new(base.state().session.document().clone()).unwrap(), canvas).unwrap();
            let mut h = Harness::builder().with_size(Vec2::new(width, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
            h.state_mut().view.zoom = zoom;
            h.run();
            let id = CommentId::from_bytes([9; 16]);
            let target = h.state().discussion_rect().min + Vec2::new(80.0, 80.0);
            let world = h.state().view.to_world(h.state().viewport.min, target);
            let node = pinned.then_some(NodeId::from_bytes([3; 16]));
            let node_pos = node.map(|id| h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&id).unwrap().pos).unwrap_or_default();
            let origin = Position { x: world.x - node_pos.x, y: world.y - node_pos.y };
            h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Behind panel","node":node,"pos":origin})).unwrap();
            let ctx = h.ctx.clone();
            h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).unwrap();
            h.run();
            let label = format!("Discussion pin {id}");
            let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
            assert!(from.distance(target) > 50.0, "fixture must actually be displaced");
            let original = h.state().session.document().clone();
            let revision = h.state().session.revision();
            // A plain click must not adopt the temporary displayed position as its anchor.
            h.get_by_role_and_label(Role::Button, &label).click();
            h.run();
            assert_eq!(h.state().session.document(), &original);
            let direction = if width < 500.0 { 1.0 } else { -1.0 };
            for cancel in [true, false] {
                let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
                h.hover_at(from);
                h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
                h.run();
                for dx in [12.0, 24.0, 36.0].map(|dx| dx * direction) {
                    h.hover_at(from + Vec2::new(dx, 0.0));
                    h.run();
                    let displayed = h.get_by_role_and_label(Role::Button, &label).rect().center();
                    assert!((displayed - (from + Vec2::new(dx, 0.0))).length() < 1.0, "bubble must follow small horizontal deltas: {displayed:?}");
                    assert_eq!(h.state().session.revision(), revision);
                }
                if cancel {
                    h.key_press(egui::Key::Escape);
                    h.run();
                }
                h.event(egui::Event::PointerButton {
                    pos: from + Vec2::new(36.0 * direction, 0.0),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
                h.run();
                if cancel {
                    assert_eq!(h.state().session.document(), &original);
                    assert_eq!(h.state().session.revision(), revision);
                } else {
                    assert_eq!(h.state().session.revision(), revision + 1);
                    let root = h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap();
                    assert_eq!(root.node, node);
                    assert!((root.pos.x - origin.x - 36.0 * direction / zoom).abs() < 0.001);
                    assert_eq!(root.pos.y, origin.y);
                    let displayed = h.get_by_role_and_label(Role::Button, &label).rect().center();
                    assert!((displayed - (from + Vec2::new(36.0 * direction, 0.0))).length() < 1.0, "release must retain the visible drop");
                    let moved = h.state().session.document().clone();
                    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
                    h.run();
                    assert_eq!(h.state().session.document(), &original);
                    h.state_mut().session.dispatch("edit.redo", &json!({})).unwrap();
                    h.run();
                    assert_eq!(h.state().session.document(), &moved);
                }
            }
        }
    }

    #[test]
    fn note_text_is_searchable_through_landed_search_without_document_mutation() {
        let mut h = harness();
        let ctx = h.ctx.clone();
        h.state_mut().discussion_action(&ctx, Action::Note);
        let id = *h.state().selection.nodes().iter().next().unwrap();
        let canvas = h.state().canvas;
        h.state_mut()
            .session
            .dispatch("node.param_set", &json!({"canvas":canvas,"id":id,"name":"text","value":{"type":"text","value":"Unique shoreline note"}}))
            .unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let events = h.state().session.events_since(0).unwrap();
        let history = (h.state().session.can_undo(), h.state().session.can_redo());
        h.state_mut().dispatch_search(&ctx, "canvas.search_open", &json!({})).unwrap();
        h.run();
        h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query":"Unique shoreline"})).unwrap();
        h.run();
        assert_eq!(h.state().search.results, vec![id]);
        h.state_mut().dispatch_search(&ctx, "canvas.search_next", &json!({})).unwrap();
        h.run();
        assert!(h.state().selection.contains(id));
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().session.events_since(0).unwrap(), events);
        assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    }

    #[test]
    fn held_discussion_pin_does_not_admit_main_quick_add_keys() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Held pin","pos":{"x":0,"y":0}})).unwrap();
        h.run();
        let label = format!("Discussion pin {id}");
        let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        h.hover_at(from + Vec2::new(35.0, 15.0));
        h.run();
        assert!(h.state().comments.drag.is_some());
        let ctx = h.ctx.clone();
        assert!(h.state_mut().dispatch_search(&ctx, "canvas.search_open", &json!({})).is_err());
        assert!(!h.state().search.open);
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let events = h.state().session.events_since(0).unwrap();
        let history = (h.state().session.can_undo(), h.state().session.can_redo());
        #[derive(Debug)]
        struct UnreadDrop;
        impl egui::DroppedFile for UnreadDrop {
            fn path(&self) -> &std::path::Path {
                panic!("a held discussion pin must refuse drops before touching the file");
            }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                panic!("a held discussion pin must not read a dropped file");
            }
        }
        h.input_mut().dropped_files.push(Arc::new(UnreadDrop));
        h.step();
        assert!(h.state().comments.drag.is_some());
        assert_eq!(h.state().notice.as_deref(), Some("Finish moving the discussion pin before dropping files."));
        let tokens = *h.state().theme.tokens();
        h.state_mut().split_grid(NodeId::from_bytes([3; 16]), 2, 2, &tokens);
        assert_eq!(h.state().notice.as_deref(), Some("Finish moving the discussion pin before splitting an image."));
        assert_eq!(h.state().session.document(), &before);
        h.event(egui::Event::Key { key: egui::Key::T, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
        h.step();
        assert!(
            h.ctx.input(|i| i.events.iter().any(|event| matches!(event, egui::Event::Key { key: egui::Key::T, pressed: true, .. }))),
            "rejected quick-add must not consume T"
        );
        h.event(egui::Event::Key { key: egui::Key::T, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
        h.run();
        assert_eq!(h.state().session.document(), &before, "held comment drag owns input before quick-add admission");
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().session.events_since(0).unwrap(), events);
        assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
        h.key_press(egui::Key::Escape);
        h.run();
        h.event(egui::Event::PointerButton {
            pos: from + Vec2::new(35.0, 15.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert_eq!(h.state().session.document(), &before);
        h.hover_at(h.state().viewport.min + Vec2::new(800.0, 120.0));
        h.key_press(egui::Key::T);
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), before.canvases().get(&canvas).unwrap().nodes().len() + 1);
    }

    #[test]
    fn full_accepted_tail_survives_edit_blur_and_cancel_with_bounded_retention() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        let text = "x".repeat(1024 * 1024 - 8) + " tail-é";
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":text,"pos":{"x":0,"y":0}})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).unwrap();
        h.run();
        h.get_by_role_and_label(Role::Button, "Edit root").click();
        h.run();
        let key = h.state().comments.editing.unwrap();
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, text);
        assert!(h.state().comments.bytes() <= MAX_DRAFT_BYTES);
        // Unfocused drafts use a bounded preview and keep their unseen accepted tail.
        h.get_by_role_and_label(Role::CheckBox, "Show resolved").click();
        h.run();
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, text);
        h.get_by_role_and_label(Role::Button, "Cancel draft").scroll_to_me();
        h.run();
        h.get_by_role_and_label(Role::Button, "Cancel draft").click();
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().text, text);
        assert!(h.state().comments.editing.is_none(), "explicit Cancel must discard the editor buffer");
        h.get_by_role_and_label(Role::Button, "Edit root").click();
        h.run();
        h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::Home);
        h.key_press(egui::Key::Delete);
        h.event(egui::Event::Text("Y".into()));
        h.run();
        let key = h.state().comments.editing.unwrap();
        let changed = &h.state().comments.drafts.get(&key).unwrap().text;
        assert_eq!(changed.len(), text.len());
        assert!(changed.starts_with('Y'));
        assert!(changed.ends_with(" tail-é"));
        h.get_by_role_and_label(Role::Button, "Save message").scroll_to_me();
        h.run();
        let save = h.get_by_role_and_label(Role::Button, "Save message").rect();
        assert!(h.state().viewport.contains_rect(save), "Save must remain reachable for a full-size draft: {save:?}");
        h.get_by_role_and_label(Role::Button, "Save message").click();
        h.run();
        let stored = &h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().text;
        assert_eq!(stored.len(), text.len());
        assert!(stored == &("Y".to_owned() + text.get(1..).unwrap()), "same-size Save must preserve the unseen tail; notice={:?}", h.state().notice);
    }
    #[test]
    fn session_replacement_invalidates_draft_identity_and_pin_cache_even_at_same_ids_and_revision() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Original","pos":{"x":0,"y":0}})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).unwrap();
        h.run();
        h.state_mut().begin_draft(&ctx, Target::Root(id), None).unwrap();
        h.run();
        let key = h.state().comments.editing.unwrap();
        h.state_mut().comments.drafts.get_mut(&key).unwrap().text = "Old draft".into();
        let mut replacement = h.state().session.document().clone();
        Change::SetCommentText { canvas, id, text: "Replacement".into() }.apply(&mut replacement).unwrap();
        Change::SetCommentPin { canvas, id, node: None, pos: Position { x: 500.0, y: 40.0 } }.apply(&mut replacement).unwrap();
        let mut session = Session::new(replacement).unwrap();
        session.dispatch("node.title", &json!({"canvas":canvas,"id":NodeId::from_bytes([3;16]),"title":"Same revision"})).unwrap();
        assert_eq!(session.revision(), h.state().session.revision());
        h.state_mut().session = session;
        h.state_mut().save_discussion(&ctx, false);
        h.run();
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().text, "Replacement");
        assert_eq!(h.state().comments.pins[&id], Position { x: 500.0, y: 40.0 });
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, "Old draft");
        h.state_mut().begin_draft(&ctx, Target::Root(id), None).unwrap();
        assert_ne!(h.state().comments.editing, Some(key));
    }
    #[test]
    fn secondary_pin_drag_never_publishes_or_takes_the_primary_gesture() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Pin","pos":{"x":0,"y":0}})).unwrap();
        h.run();
        let revision = h.state().session.revision();
        let document = h.state().session.document().clone();
        let from = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {id}")).rect().center();
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        h.hover_at(from + Vec2::new(100.0, 80.0));
        h.run();
        assert!(h.state().comments.drag.is_none());
        h.event(egui::Event::PointerButton {
            pos: from + Vec2::new(100.0, 80.0),
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(h.state().session.document(), &document);
    }
    #[test]
    fn open_panel_controls_cannot_steal_a_dirty_node_parameter_editor() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Stored","pos":{"x":0,"y":0}})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).unwrap();
        h.run();
        h.state_mut().begin_draft(&ctx, Target::Root(id), None).unwrap();
        h.run();
        let key = h.state().comments.editing.unwrap();
        h.state_mut().comments.drafts.get_mut(&key).unwrap().dirty = true;
        // Park the discussion draft explicitly, then edit the normal node without closing panel.
        h.state_mut().comments.editing = None;
        h.ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        h.run();
        h.get_by_role_and_label(Role::TextInput, "Text").click();
        h.run();
        h.event(egui::Event::Text("dirty node".into()));
        h.run();
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let owner = h.ctx.memory(|m| m.focused());
        for label in ["Edit root".to_owned(), "Reply".to_owned(), format!("Resume draft {key}")] {
            h.get_by_role_and_label(Role::Button, &label).click();
            h.run();
            assert_eq!(h.ctx.memory(|m| m.focused()), owner);
            assert_eq!(h.state().session.document(), &before);
            assert_eq!(h.state().session.revision(), revision);
            assert!(h.state().comments.editing.is_none());
        }
    }

    #[test]
    fn title_editor_and_active_pin_gesture_exclude_discussion_and_other_commands() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([9; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Stored","pos":{"x":0,"y":0}})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).unwrap();
        h.run();
        h.get_by_role_and_label(Role::Button, "Node A quiet forest at dawn, soft light through the trees.").focus();
        h.run();
        h.key_press(egui::Key::F2);
        h.run();
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        h.event(egui::Event::Text("Dirty title".into()));
        h.run();
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let owner = h.ctx.memory(|m| m.focused());
        h.get_by_role_and_label(Role::Button, "Edit root").click();
        h.run();
        assert_eq!(h.ctx.memory(|m| m.focused()), owner);
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), revision);
        h.key_press(egui::Key::Escape);
        h.run();
        h.get_by_role_and_label(Role::Button, "Close comments").click();
        h.run();
        let from = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {id}")).rect().center();
        h.hover_at(from);
        h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.run();
        h.hover_at(from + Vec2::new(80.0, 50.0));
        h.run();
        assert!(h.state().comments.drag.is_some());
        let view = h.state().view;
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::M);
        h.run();
        assert!(h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).is_err());
        assert_eq!(h.state().view, view);
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":NodeId::from_bytes([3;16]),"title":"External edit"})).unwrap();
        h.run();
        assert!(h.state().comments.drag.is_none());
        assert!(h.state().comments.gesture_cancelled);
        let revised = h.state().session.revision();
        h.event(egui::Event::PointerButton {
            pos: from + Vec2::new(80.0, 50.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert_eq!(h.state().session.revision(), revised);
        assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().comments().get(&id).unwrap().pos, Position::default());
    }
    #[test]
    fn escape_after_blur_closes_panel_and_retains_exact_dirty_draft() {
        let mut h = harness();
        h.key_press(egui::Key::C);
        h.run();
        h.event(egui::Event::Text("Retain this dirty discussion".into()));
        h.run();
        let key = h.state().comments.editing.unwrap();
        let before = h.state().session.document().clone();
        let selection = h.state().selection.clone();
        h.get_by_role_and_label(Role::CheckBox, "Show resolved").click();
        h.run();
        assert!(!canvas::text_input_focused(&h.ctx));
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().comments.open);
        assert_eq!(h.state().comments.drafts.get(&key).unwrap().text, "Retain this dirty discussion");
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().selection, selection);
        assert_eq!(h.state().session.revision(), 0);
    }
    #[test]
    fn long_ids_titles_and_thread_previews_stay_inside_the_real_panel_and_viewport() {
        let mut h = harness();
        let canvas = h.state().canvas;
        let id = CommentId::from_bytes([0xab; 16]);
        let reply = CommentId::from_bytes([0xcd; 16]);
        h.state_mut().session.dispatch("node.title", &json!({"canvas":canvas,"id":NodeId::from_bytes([3;16]),"title":"Long title ".repeat(300)})).unwrap();
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"x".repeat(2048),"pos":{"x":0,"y":0}})).unwrap();
        h.state_mut().session.dispatch("comment.reply", &json!({"canvas":canvas,"id":id,"reply":reply,"text":"y".repeat(2048)})).unwrap();
        let ctx = h.ctx.clone();
        h.state_mut().dispatch_comments(&ctx, "comment.open", &json!({"id":id})).unwrap();
        h.run();
        let actual = h.ctx.memory(|memory| memory.area_rect(egui::Id::new("discussions_panel"))).unwrap();
        let t = h.state().theme.tokens();
        assert!(actual.width() <= t.comments_width + 2.0 * t.handle_stroke, "real panel expanded to {actual:?}");
        assert!(h.state().viewport.contains_rect(actual));
        for label in ["Read reply 1", "Edit reply 1", "Delete reply 1"] {
            let rect = h.get_by_role_and_label(Role::Button, label).rect();
            assert!(actual.contains_rect(rect), "{label} escaped panel: {rect:?}");
        }
    }
}
