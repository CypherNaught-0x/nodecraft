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

/// How many notices the stack shows at once: the newest. A new notice is always shown; to make
/// room it replaces the oldest passing (non-sticky) notice, never a sticky one, and sticky
/// notices it covers are counted on a "more" line under the stack.
pub const MAX_NOTICES: usize = 3;
/// The most sticky notices kept; beyond this the oldest goes (one that isn't about a run first),
/// so the stack stays bounded.
pub const MAX_STICKY: usize = 32;
/// The longest step the timeout clock takes in one frame, and the longest the stack waits for a
/// repaint while a notice is timing out: a frame after a long gap (an idle or backgrounded
/// window) counts as at most this, so nothing expires unseen, and an idle window still repaints
/// often enough that a notice goes after about [`TIMEOUT_SECONDS`].
pub const MAX_STEP_SECONDS: f64 = 0.5;
/// The owner of errors about adding to the canvas (an id that couldn't be allocated, no free
/// space for a paste): any later change to the document proves the canvas takes additions again,
/// so the shell settles them then.
pub const CANVAS_ADD: &str = "canvas.add";
/// The owner of a failed save, settled by the next save that completes.
pub const FILE_SAVE: &str = "file.save";
/// The owner of a failed or refused export, settled by the next export that completes.
pub const EXPORT: &str = "export";
/// How long an information, success or warning notice stays on screen.
pub const TIMEOUT_SECONDS: f64 = 6.0;
/// The longest notice text kept, in UTF-8 bytes; longer text is cut at a character boundary.
pub const MAX_TEXT_BYTES: usize = 4096;

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
    /// Pushed since the last tick: its first tick never advances it, so it is painted first.
    fresh: bool,
    /// What raised it, so that code can take it back once the thing succeeds ([`Notices::settle`]).
    owner: Option<String>,
    /// About a run: kept over other sticky notices when the stack is full.
    run: bool,
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
        Self { id: 0, kind, text, action: None, sticky: kind.sticky(), shown: 0.0, fresh: true, owner: None, run: false }
    }

    /// Raised by `owner` (a short key such as `"wire"` or `"node.param_set <id>"`), which takes it
    /// back with [`Notices::settle`] when the same thing later succeeds.
    pub fn owned_by(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// About a run (it costs money if missed): sticky, and the last sticky notice to be dropped.
    pub fn from_run(mut self) -> Self {
        self.sticky = true;
        self.run = true;
        self
    }

    /// Keep it until dismissed whatever its kind (a run warning that costs money if missed).
    pub fn keep(mut self) -> Self {
        self.sticky = true;
        self
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
    /// The pass the stack was last drawn in: the clock runs, and the focus and hover below count,
    /// only while it is drawn pass after pass.
    drawn_pass: Option<u64>,
    /// The notice whose button had the keyboard focus, or that the pointer was over, last frame.
    focused: Option<u64>,
    hovered: Option<u64>,
}

impl Notices {
    /// Add `notice` on top, where it is always shown, and return its id. Room is made by
    /// dropping the oldest passing notices only; sticky ones it covers wait below, counted.
    pub fn push_notice(&mut self, mut notice: Notice) -> u64 {
        // The same message again refreshes the one already shown instead of stacking a copy.
        if let Some(index) = self.queue.iter().position(|old| old.kind == notice.kind && old.text == notice.text) {
            let old = self.queue.remove(index);
            notice.id = old.id;
            notice.sticky |= old.sticky;
            notice.run |= old.run;
            notice.owner = notice.owner.or(old.owner);
        } else {
            self.next_id = self.next_id.wrapping_add(1);
            notice.id = self.next_id;
        }
        while self.queue.len() >= MAX_NOTICES {
            match self.queue.iter().position(|old| !old.sticky) {
                Some(index) => {
                    self.queue.remove(index);
                }
                None => break,
            }
        }
        if notice.sticky && self.queue.iter().filter(|old| old.sticky).count() >= MAX_STICKY {
            let oldest = self.queue.iter().position(|old| old.sticky && !old.run).or_else(|| self.queue.iter().position(|old| old.sticky));
            if let Some(index) = oldest {
                self.queue.remove(index);
            }
        }
        let id = notice.id;
        self.queue.push(notice);
        id
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
        self.report_id(error);
    }

    /// [`Self::report`], returning the notice's id so the code that raised it can take it back
    /// with [`Self::dismiss`] once the thing it was about has succeeded.
    pub fn report_id(&mut self, error: Error) -> u64 {
        self.push_notice(Notice::new(NoticeKind::of_error(error.kind), error.message))
    }

    /// [`Self::report`] on behalf of `owner`, which [`Self::settle`]s it once that succeeds.
    pub fn report_for(&mut self, owner: impl Into<String>, error: Error) {
        self.push_notice(Notice::new(NoticeKind::of_error(error.kind), error.message).owned_by(owner));
    }

    /// [`Self::error`] on behalf of `owner`, which [`Self::settle`]s it once that succeeds.
    pub fn error_for(&mut self, owner: impl Into<String>, text: impl Into<String>) {
        self.push_notice(Notice::new(NoticeKind::Error, text).owned_by(owner));
    }

    /// `owner` has since succeeded: take back the notices it raised, sticky or not.
    pub fn settle(&mut self, owner: &str) {
        self.queue.retain(|notice| notice.owner.as_deref() != Some(owner));
    }

    /// Show `text` as `kind`, or [`Self::clear`] when there is none: what an action that
    /// reports its own outcome does on success.
    pub fn set(&mut self, kind: NoticeKind, text: Option<String>) {
        match text {
            Some(text) => self.push(kind, text),
            None => self.clear(),
        }
    }

    /// What an action that reports only failures does: [`Self::clear`] on success, report an
    /// error.
    pub fn outcome<T>(&mut self, result: Result<T, Error>) {
        match result {
            Ok(_) => self.clear(),
            Err(error) => self.report(error),
        }
    }

    /// Clear the passing notices after an action has succeeded. Sticky ones (every error, and
    /// run warnings) are never cleared by an unrelated success: only the person dismisses them,
    /// or the code that raised one takes it back by id or text ([`Self::dismiss`],
    /// [`Self::dismiss_text`]).
    pub fn clear(&mut self) {
        self.queue.retain(|notice| notice.sticky);
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
    /// Only a stack drawn in the previous pass counts: a hidden one has no focus to take.
    pub fn focused(&self, ctx: &egui::Context) -> Option<u64> {
        self.focused.filter(|id| self.drawn_last_pass(ctx) && self.queue.iter().any(|notice| notice.id == *id))
    }

    fn drawn_last_pass(&self, ctx: &egui::Context) -> bool {
        self.drawn_pass.is_some_and(|pass| pass.saturating_add(1) == ctx.cumulative_pass_nr())
    }

    /// Advance the timeout clock to `now` (egui time), pausing the notice under the pointer or
    /// holding the focus, and drop notices that have timed out. `running` is false when the
    /// stack wasn't drawn in the previous pass: the time since then doesn't count. Returns how
    /// long until the next one times out, for a repaint.
    fn tick(&mut self, now: f64, running: bool) -> Option<f64> {
        let step = match self.last_time {
            Some(last) if running && now.is_finite() && last.is_finite() => (now - last).clamp(0.0, MAX_STEP_SECONDS),
            _ => 0.0,
        };
        self.last_time = Some(now);
        if !running {
            self.focused = None;
            self.hovered = None;
        }
        let paused = [self.focused, self.hovered];
        // Only the drawn (newest) notices age; one pushed since the last tick starts next tick.
        let hidden = self.queue.len().saturating_sub(MAX_NOTICES);
        for notice in self.queue.iter_mut().skip(hidden) {
            if std::mem::take(&mut notice.fresh) {
                continue;
            }
            if !notice.sticky && !paused.contains(&Some(notice.id)) {
                notice.shown += step;
            }
        }
        self.queue.retain(|notice| notice.sticky || notice.shown < TIMEOUT_SECONDS);
        self.queue
            .iter()
            .filter(|notice| !notice.sticky && !paused.contains(&Some(notice.id)))
            .map(|notice| (TIMEOUT_SECONDS - notice.shown).clamp(0.0, MAX_STEP_SECONDS))
            .min_by(f64::total_cmp)
    }

    /// Draw the stack, newest on top, advancing the clock. `escape` is the notice that took this
    /// frame's Escape (see [`Self::focused`]); it is dismissed.
    pub fn show(&mut self, ui: &mut egui::Ui, t: &Tokens, escape: Option<u64>) -> Shown {
        if let Some(id) = escape {
            self.dismiss(id);
        }
        let now = ui.input(|i| i.time);
        let running = self.drawn_last_pass(ui.ctx());
        self.drawn_pass = Some(ui.ctx().cumulative_pass_nr());
        if let Some(next) = self.tick(now, running) {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(next.clamp(0.0, MAX_STEP_SECONDS)));
        }
        let mut shown = Shown::default();
        let mut dismissed = Vec::new();
        let (mut focused, mut hovered) = (None, None);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let stack_height = ui.ctx().content_rect().height() * 0.45;
        egui::ScrollArea::vertical().id_salt("notice_stack").max_height(stack_height).auto_shrink([false, true]).show(ui, |ui| {
            for notice in self.queue.iter().rev().take(MAX_NOTICES) {
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
                if pointer.is_some_and(|pos| response.rect.intersect(ui.clip_rect()).contains(pos)) {
                    hovered = Some(notice.id);
                }
            }
        });
        // Older notices wait under the stack, counted, until the shown ones are dismissed.
        let more = self.queue.len().saturating_sub(MAX_NOTICES);
        if more > 0 {
            let line = if more == 1 {
                "1 more notice below; dismiss these to see it.".to_owned()
            } else {
                format!("{more} more notices below; dismiss these to see them.")
            };
            theme::panel_frame(t).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(egui::RichText::new(line).color(t.text_muted)).wrap());
            });
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
                if close.has_focus() {
                    close.scroll_to_me(None);
                }
                if let Some(button) = &notice.action {
                    let pressed = ui.button(button.label.as_str());
                    action = pressed.clicked();
                    focused |= pressed.has_focus();
                    if pressed.has_focus() {
                        pressed.scroll_to_me(None);
                    }
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    let body_height = (ui.ctx().content_rect().height() * 0.25).min(t.comments_height);
                    let text = egui::ScrollArea::vertical()
                        .id_salt(id.with("details"))
                        .max_height(body_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| ui.push_id(id, |ui| ui.add(egui::Label::new(egui::RichText::new(notice.text.as_str()).color(t.text)).wrap())).inner)
                        .inner;
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
    inline(ui, t, NoticeKind::Error, text, false)
}

/// The newest notice as one compact line in an editor header. The global notice stack keeps
/// its full scrollable details and dismissal/action controls.
pub fn inline_notice(ui: &mut egui::Ui, t: &Tokens, notice: &Notice) -> egui::Response {
    inline(ui, t, notice.kind, &notice.text, true)
}

fn inline(ui: &mut egui::Ui, t: &Tokens, kind: NoticeKind, text: &str, compact: bool) -> egui::Response {
    // Warnings and errors are drawn in their colour (both are 4.5:1 text tokens); the rest in text.
    let ink = match kind {
        NoticeKind::Error => t.danger,
        NoticeKind::Warning => t.stale,
        NoticeKind::Info | NoticeKind::Success => t.text,
    };
    ui.horizontal(|ui| {
        let icon = ui.add(kind.icon().image(ui.ctx(), t.icon_size, t).tint(kind.colour(t)).alt_text(kind.label()));
        icon.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, kind.label()));
        let label = egui::Label::new(egui::RichText::new(text).color(ink));
        ui.add(if compact { label.truncate() } else { label.wrap() })
    })
    .inner
}

#[cfg(test)]
mod tests;
