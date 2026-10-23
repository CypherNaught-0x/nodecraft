//! Who else is in a shared project, and where (#120). Each frame says where this person is (the
//! open canvas, the pointer over it and the selected cards) when that has changed, at most every
//! [`SAY_EVERY`]; the others are drawn on the same canvas: a pointer with their name, an outline
//! round each card they have selected, and their initials beside the Share chip, which take you
//! to them.
use super::*;
use nodecraft_sync::{MAX_SELECTION, Person, Presence};

/// How often this person says where they are, at most.
const SAY_EVERY: Duration = Duration::from_millis(50);
/// The most initials shown beside the Share chip; the rest are counted.
const MAX_AVATARS: usize = 5;
/// The most of a name drawn on a pointer.
const MAX_NAME_CHARS: usize = 24;

/// What `person` is drawn in: one of the theme's presence colours, by their place in the room.
pub(super) fn color(t: &Tokens, person: &Person) -> egui::Color32 {
    let n = t.presence_colors.len().max(1) as u64;
    usize::try_from(person.peer % n).ok().and_then(|i| t.presence_colors.get(i)).copied().unwrap_or(t.accent)
}

/// `name`, cut short for a pointer or a list.
pub(super) fn short(name: &str) -> String {
    if name.chars().count() <= MAX_NAME_CHARS {
        return name.into();
    }
    let mut cut: String = name.chars().take(MAX_NAME_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The first letter of `name`, for its avatar.
fn initial(name: &str) -> String {
    name.chars().find(|c| c.is_alphanumeric()).map_or_else(|| "?".into(), |c| c.to_uppercase().collect())
}

impl NodeCraftApp {
    /// Where this person is: the open canvas, the pointer if it is over it, and the selection.
    fn presence_here(&self, ctx: &egui::Context) -> Presence {
        // A private canvas isn't shared, nor is anything about being on it.
        if self.session.document().canvases().get(&self.canvas).is_none_or(|canvas| canvas.is_private()) {
            return Presence::default();
        }
        let cursor = ctx
            .pointer_hover_pos()
            .filter(|pointer| self.viewport.contains(*pointer))
            .map(|pointer| self.view.to_world(self.viewport.min, pointer))
            .map(|world| [world.x, world.y]);
        let selection = self.selection.nodes().iter().take(MAX_SELECTION).copied().collect();
        let presence = Presence { canvas: Some(self.canvas), cursor, selection };
        // A pointer far off the canvas (or a view gone wrong) isn't said; the selection still is.
        if presence.validate().is_ok() { presence } else { Presence { cursor: None, ..presence } }
    }

    /// Tells the room where this person is, if that changed and it has been long enough.
    pub(super) fn say_presence(&mut self, ctx: &egui::Context) {
        let presence = self.presence_here(ctx);
        let Some(live) = self.sharing.as_mut().and_then(|sharing| sharing.live.as_mut()) else { return };
        if !matches!(live.state, State::Connected(_)) || live.joining || live.said.as_ref() == Some(&presence) {
            return;
        }
        let now = Instant::now();
        if let Some(next) = live.said_at.map(|at| at + SAY_EVERY)
            && now < next
        {
            // Said once the wait is over, even if nothing moves meanwhile.
            ctx.request_repaint_after(next - now);
            return;
        }
        // A worker that has gone is noticed, and reported, by the next drive.
        if live.worker.presence(presence.clone()).is_ok() {
            live.said = Some(presence);
            live.said_at = Some(now);
        }
    }

    /// The others on this canvas: an outline round each card they have selected, and their
    /// pointer with their name, above the cards.
    pub(in crate::app) fn draw_people(&self, ui: &egui::Ui, viewport: Rect, t: &Tokens) {
        // While reconnecting, where the others were is out of date: nobody is drawn until then.
        let Some(live) = self.sharing.as_ref().and_then(|sharing| sharing.live.as_ref()).filter(|live| matches!(live.state, State::Connected(_))) else {
            return;
        };
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.id().with("people"))).with_clip_rect(viewport);
        let here = live.status.people.iter().filter(|person| person.presence.canvas == Some(self.canvas));
        for person in here {
            let color = color(t, person);
            for id in &person.presence.selection {
                if let Some(rect) = self.painted.get(id) {
                    let rect = self.view.rect_to_screen(viewport.min, *rect).expand(t.presence_ring * 2.0);
                    painter.rect_stroke(rect, t.card_radius, egui::Stroke::new(t.presence_ring, color), egui::StrokeKind::Outside);
                }
            }
            let Some([x, y]) = person.presence.cursor else { continue };
            let tip = self.view.to_screen(viewport.min, egui::pos2(x, y));
            if !viewport.contains(tip) {
                continue;
            }
            let size = t.presence_pointer;
            let arrow = vec![tip, tip + egui::vec2(0.0, size), tip + egui::vec2(size * 0.66, size * 0.66)];
            painter.add(egui::Shape::convex_polygon(arrow, color, egui::Stroke::new(t.presence_outline, t.presence_ink)));
            let galley = painter.layout_no_wrap(short(&person.name), egui::FontId::proportional(t.label_size), t.presence_ink);
            let pad = egui::vec2(t.label_gap * 2.0, t.label_gap);
            let pill = Rect::from_min_size(tip + egui::vec2(size * 0.6, size * 0.8), galley.size() + pad * 2.0);
            painter.rect_filled(pill, pill.height() * 0.5, color);
            painter.galley(pill.min + pad, galley, t.presence_ink);
        }
    }

    /// Each other person's initial, beside the Share chip; clicking one goes to where they are.
    pub(super) fn people_avatars(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let Some(live) = self.sharing.as_ref().and_then(|sharing| sharing.live.as_ref()).filter(|live| matches!(live.state, State::Connected(_))) else {
            return;
        };
        let people = &live.status.people;
        let mut go = None;
        for person in people.iter().take(MAX_AVATARS) {
            let side = t.presence_avatar;
            let (rect, response) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
            let color = color(t, person);
            ui.painter().circle_filled(rect.center(), side * 0.5, color);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, initial(&person.name), egui::FontId::proportional(t.label_size), t.presence_ink);
            let role = match person.role {
                Role::Owner => "shares this project",
                Role::Edit => "can edit",
                Role::Comment => "can comment",
                Role::View => "can look",
            };
            let hint = if person.presence.canvas.is_some() { "click to go to them" } else { "not on a canvas" };
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, short(&person.name)));
            if response.on_hover_text(format!("{} · {role} · {hint}", short(&person.name))).clicked() {
                go = Some(person.presence.clone());
            }
        }
        if people.len() > MAX_AVATARS {
            ui.label(egui::RichText::new(format!("+{}", people.len() - MAX_AVATARS)).color(t.text_muted));
        }
        if let Some(presence) = go {
            self.go_to(&presence);
        }
    }

    /// Opens the canvas another person is on and centres where they point, or the first card
    /// they have selected.
    fn go_to(&mut self, presence: &Presence) {
        let Some(canvas) = presence.canvas else {
            self.notice = Some("They aren't on a canvas.".into());
            return;
        };
        if !self.session.document().canvases().contains_key(&canvas) {
            self.notice = Some("They're on a canvas you can't see.".into());
            return;
        }
        self.open_canvas(canvas);
        let at =
            presence.cursor.map(|[x, y]| egui::pos2(x, y)).or_else(|| presence.selection.iter().find_map(|id| self.painted.get(id)).map(|rect| rect.center()));
        if let Some(at) = at
            && self.view.center_on(self.viewport, f64::from(at.x), f64::from(at.y))
        {
            // Placed by hand: the canvas mustn't fit itself to its cards over it.
            self.centred = true;
        }
    }
}
