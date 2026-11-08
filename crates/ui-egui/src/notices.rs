//! Typed, dismissible notices in the bottom-left corner (#1110).
//!
//! A notice is view state: the shell keeps a short queue of them, newest on top. Errors stay until
//! the person dismisses them; information, successes and warnings time out after
//! [`TIMEOUT_SECONDS`] of being on screen, and the clock stops while the pointer is over a notice
//! or one of its buttons has the keyboard focus. Each kind has its own edge colour and icon, so
//! colour is never the only cue, and each notice is an AccessKit live region: polite for
//! information and successes, assertive for warnings and errors.
//!
//! Engine errors map to a kind in one place, [`NoticeKind::of_error`]; run events map in
//! `app::runs` through [`NoticeKind::of_run`].
use crate::{
    icons::Icon,
    theme::{self, Tokens},
};
use egui::{Color32, Stroke};
use nodecraft_types::{Error, ErrorKind};

/// How many notices the stack holds at once; a fourth replaces the oldest that isn't an error.
pub const MAX_NOTICES: usize = 3;
/// How long an information, success or warning notice stays on screen.
pub const TIMEOUT_SECONDS: f64 = 6.0;
/// The longest notice text kept, in UTF-8 bytes; longer text is cut at a character boundary.
pub const MAX_TEXT_BYTES: usize = 4096;
/// The longest step the timeout clock takes in one frame, so a notice drawn again after a pause
/// (another view, a stalled frame) is not already gone.
const MAX_STEP_SECONDS: f64 = 0.25;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NoticeKind {
    Info,
    Success,
    Warning,
    Error,
}

impl NoticeKind {
    /// The one mapping from an engine error to a notice kind. A refusal to start because
    /// something else is still busy is a warning; everything else, including provider, budget,
    /// key and network failures, is an error, since runs cost money.
    pub fn of_error(kind: ErrorKind) -> Self {
        match kind {
            ErrorKind::Busy => Self::Warning,
            _ => Self::Error,
        }
    }

    /// The kind of notice a run event raises, if any (see `app::runs`).
    pub fn of_run(event: RunEvent) -> Self {
        match event {
            RunEvent::Failed | RunEvent::Refused => Self::Error,
            RunEvent::Unsaved => Self::Warning,
            RunEvent::Placed => Self::Success,
        }
    }

    /// The kind of a background worker's one-line report (option lists, ComfyUI nodes,
    /// templates, update checks), which says in its words whether it worked: a refusal or a
    /// failure is an error, something left out a warning, work still going information, and
    /// anything else a success.
    pub fn of_report(text: &str) -> Self {
        const FAILED: [&str; 10] = ["weren't", "were not", "couldn't", "Couldn't", "stopped", "discarded", "unavailable", "There's no", "failed", "wasn't"];
        const PARTIAL: [&str; 2] = ["wasn't installed", "skipped"];
        let says = |words: &[&str]| words.iter().any(|word| text.contains(word));
        // "Fetched 3 lists. X wasn't installed" is partial; a report that failed outright says so.
        let failed = FAILED.iter().filter(|word| **word != "wasn't").any(|word| text.contains(word));
        if says(&PARTIAL) && !failed {
            Self::Warning
        } else if says(&FAILED) {
            Self::Error
        } else if text.ends_with('…') {
            Self::Info
        } else {
            Self::Success
        }
    }

    /// Errors stay until dismissed.
    pub fn sticky(self) -> bool {
        self == Self::Error
    }

    /// The edge and icon colour.
    pub fn colour(self, t: &Tokens) -> Color32 {
        match self {
            Self::Info => t.text_muted,
            Self::Success => t.accent,
            Self::Warning => t.stale,
            Self::Error => t.danger,
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            Self::Info => Icon::Info,
            Self::Success => Icon::Success,
            Self::Warning => Icon::Warning,
            Self::Error => Icon::Error,
        }
    }

    /// What a screen reader hears besides the text.
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Information",
            Self::Success => "Done",
            Self::Warning => "Warning",
            Self::Error => "Error",
        }
    }

    pub fn live(self) -> egui::accesskit::Live {
        match self {
            Self::Info | Self::Success => egui::accesskit::Live::Polite,
            Self::Warning | Self::Error => egui::accesskit::Live::Assertive,
        }
    }
}

/// The run events that notify, as `app::runs` sees them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEvent {
    Failed,
    Refused,
    Unsaved,
    Placed,
}

/// A button on a notice that dispatches a command, as the palette would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeAction {
    pub label: String,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    id: u64,
    pub kind: NoticeKind,
    pub text: String,
    pub action: Option<NoticeAction>,
    pub sticky: bool,
    /// Seconds on screen while not paused.
    shown: f64,
}

impl Notice {
    pub fn new(kind: NoticeKind, text: impl Into<String>) -> Self {
        let mut text = text.into();
        if text.len() > MAX_TEXT_BYTES {
            let mut end = MAX_TEXT_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        Self { id: 0, kind, text, action: None, sticky: kind.sticky(), shown: 0.0 }
    }

    pub fn with_action(mut self, label: impl Into<String>, command: impl Into<String>) -> Self {
        self.action = Some(NoticeAction { label: label.into(), command: command.into() });
        self
    }

    pub fn id(&self) -> u64 {
        self.id
    }
}

/// What the person did with the notice stack this frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shown {
    /// The command of an action button that was pressed.
    pub action: Option<String>,
}

/// The shell's notice queue, oldest first.
#[derive(Debug, Default)]
pub struct Notices {
    queue: Vec<Notice>,
    next_id: u64,
    last_time: Option<f64>,
    /// The notice whose button had the keyboard focus, or that the pointer was over, last frame.
    focused: Option<u64>,
    hovered: Option<u64>,
}

impl Notices {
    pub fn push_notice(&mut self, mut notice: Notice) {
        // The same message again refreshes the one already shown instead of stacking a copy.
        if let Some(index) = self.queue.iter().position(|old| old.kind == notice.kind && old.text == notice.text) {
            let old = self.queue.remove(index);
            notice.id = old.id;
        } else {
            self.next_id = self.next_id.wrapping_add(1);
            notice.id = self.next_id;
        }
        while self.queue.len() >= MAX_NOTICES {
            let index = self.queue.iter().position(|old| !old.sticky).unwrap_or(0);
            if index < self.queue.len() {
                self.queue.remove(index);
            } else {
                break;
            }
        }
        self.queue.push(notice);
    }

    pub fn push(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.push_notice(Notice::new(kind, text));
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(NoticeKind::Info, text);
    }

    pub fn success(&mut self, text: impl Into<String>) {
        self.push(NoticeKind::Success, text);
    }

    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(NoticeKind::Warning, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(NoticeKind::Error, text);
    }

    /// An engine error, as the kind [`NoticeKind::of_error`] gives it.
    pub fn report(&mut self, error: Error) {
        self.push(NoticeKind::of_error(error.kind), error.message);
    }

    /// Show `text` as `kind`, or clear the stack when there is none: what an action that reports
    /// its own outcome does on success.
    pub fn set(&mut self, kind: NoticeKind, text: Option<String>) {
        match text {
            Some(text) => self.push(kind, text),
            None => self.clear(),
        }
    }

    /// What an action that reports only failures does: clear on success, report an error.
    pub fn outcome<T>(&mut self, result: Result<T, Error>) {
        match result {
            Ok(_) => self.clear(),
            Err(error) => self.report(error),
        }
    }

    /// Clear every notice: the action they were about has since succeeded.
    pub fn clear(&mut self) {
        self.queue.clear();
    }

    pub fn dismiss(&mut self, id: u64) {
        self.queue.retain(|notice| notice.id != id);
    }

    /// Remove every notice that says `text`.
    pub fn dismiss_text(&mut self, text: &str) {
        self.queue.retain(|notice| notice.text != text);
    }

    /// Remove the newest notice and return its text.
    pub fn take_latest(&mut self) -> Option<String> {
        self.queue.pop().map(|notice| notice.text)
    }

    pub fn latest(&self) -> Option<&Notice> {
        self.queue.last()
    }

    pub fn latest_text(&self) -> Option<&str> {
        self.latest().map(|notice| notice.text.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Newest first, as they are drawn.
    pub fn iter(&self) -> impl Iterator<Item = &Notice> {
        self.queue.iter().rev()
    }

    /// The notice whose button held the keyboard focus last frame. egui drops the focus on the
    /// Escape press itself, so the shell reads this to give that notice its Escape.
    pub fn focused(&self) -> Option<u64> {
        self.focused.filter(|id| self.queue.iter().any(|notice| notice.id == *id))
    }

    /// Advance the timeout clock to `now` (egui time), pausing the notice under the pointer or
    /// holding the focus, and drop notices that have timed out. Returns how long until the next
    /// one does, for a repaint.
    fn tick(&mut self, now: f64) -> Option<f64> {
        let step = match self.last_time {
            Some(last) if now.is_finite() && last.is_finite() => (now - last).clamp(0.0, MAX_STEP_SECONDS),
            _ => 0.0,
        };
        self.last_time = Some(now);
        let paused = [self.focused, self.hovered];
        for notice in &mut self.queue {
            if !notice.sticky && !paused.contains(&Some(notice.id)) {
                notice.shown += step;
            }
        }
        self.queue.retain(|notice| notice.sticky || notice.shown < TIMEOUT_SECONDS);
        self.queue
            .iter()
            .filter(|notice| !notice.sticky && !paused.contains(&Some(notice.id)))
            .map(|notice| TIMEOUT_SECONDS - notice.shown)
            .min_by(f64::total_cmp)
    }

    /// Draw the stack, newest on top, advancing the clock. `escape` is the notice that took this
    /// frame's Escape (see [`Self::focused`]); it is dismissed.
    pub fn show(&mut self, ui: &mut egui::Ui, t: &Tokens, escape: Option<u64>) -> Shown {
        if let Some(id) = escape {
            self.dismiss(id);
        }
        let now = ui.input(|i| i.time);
        if let Some(next) = self.tick(now) {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(next.clamp(0.0, TIMEOUT_SECONDS)));
        }
        let mut shown = Shown::default();
        let mut dismissed = Vec::new();
        let (mut focused, mut hovered) = (None, None);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        for notice in self.queue.iter().rev() {
            let id = ui.id().with(("notice", notice.id));
            let response = show_one(ui, t, id, notice);
            if response.closed {
                dismissed.push(notice.id);
            }
            if response.action {
                shown.action = notice.action.as_ref().map(|action| action.command.clone());
            }
            if response.focused {
                focused = Some(notice.id);
            }
            if pointer.is_some_and(|pos| response.rect.contains(pos)) {
                hovered = Some(notice.id);
            }
        }
        for id in dismissed {
            self.dismiss(id);
        }
        self.focused = focused;
        self.hovered = hovered;
        shown
    }
}

struct OneResponse {
    rect: egui::Rect,
    closed: bool,
    action: bool,
    focused: bool,
}

fn show_one(ui: &mut egui::Ui, t: &Tokens, id: egui::Id, notice: &Notice) -> OneResponse {
    let colour = notice.kind.colour(t);
    let mut closed = false;
    let mut action = false;
    let mut focused = false;
    let frame = theme::panel_frame(t).inner_margin(egui::Margin { left: (t.card_padding + t.notice_edge) as i8, ..egui::Margin::same(t.card_padding as i8) });
    let inner = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let icon = ui.add(notice.kind.icon().image(ui.ctx(), t.icon_size, t).tint(colour).alt_text(notice.kind.label()));
            icon.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, notice.kind.label()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let close = Icon::Close.icon_button(ui, "Dismiss notice", t);
                closed = close.clicked();
                focused |= close.has_focus();
                if let Some(button) = &notice.action {
                    let pressed = ui.button(button.label.as_str());
                    action = pressed.clicked();
                    focused |= pressed.has_focus();
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    let text = ui.push_id(id, |ui| ui.add(egui::Label::new(egui::RichText::new(notice.text.as_str()).color(t.text)).wrap())).inner;
                    let (live, kind) = (notice.kind.live(), notice.kind.label());
                    ui.ctx().accesskit_node_builder(text.id, |node| {
                        node.set_live(live);
                        node.set_live_atomic();
                        node.set_description(kind);
                    });
                });
            });
        });
    });
    let rect = inner.response.rect;
    // The edge sits inside the frame's left margin, the full height of the notice.
    let x = rect.left() + t.stroke_hairline + t.notice_edge / 2.0;
    let inset = t.stroke_hairline + t.inset;
    ui.painter().line_segment([egui::pos2(x, rect.top() + inset), egui::pos2(x, rect.bottom() - inset)], Stroke::new(t.notice_edge, colour));
    OneResponse { rect, closed, action, focused }
}

/// An error bound to a control (a card's parameter, the model picker's choice): `danger` text
/// after the error icon, so colour isn't the only cue.
pub fn inline_error(ui: &mut egui::Ui, t: &Tokens, text: &str) -> egui::Response {
    inline(ui, t, NoticeKind::Error, text)
}

/// The newest notice as one line inside a view that has no notice corner (a deck being edited
/// or presented, an app form): its kind's icon, then its text.
pub fn inline_notice(ui: &mut egui::Ui, t: &Tokens, notice: &Notice) -> egui::Response {
    inline(ui, t, notice.kind, &notice.text)
}

fn inline(ui: &mut egui::Ui, t: &Tokens, kind: NoticeKind, text: &str) -> egui::Response {
    // Warnings and errors are drawn in their colour (both are 4.5:1 text tokens); the rest in text.
    let ink = match kind {
        NoticeKind::Error => t.danger,
        NoticeKind::Warning => t.stale,
        NoticeKind::Info | NoticeKind::Success => t.text,
    };
    ui.horizontal(|ui| {
        let icon = ui.add(kind.icon().image(ui.ctx(), t.icon_size, t).tint(kind.colour(t)).alt_text(kind.label()));
        icon.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, kind.label()));
        ui.add(egui::Label::new(egui::RichText::new(text).color(ink)).wrap())
    })
    .inner
}

#[cfg(test)]
mod tests;
