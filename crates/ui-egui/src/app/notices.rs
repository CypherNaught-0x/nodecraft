//! Typed notices (#1110): errors kept apart from successes, stacked, dismissible and announced.
//!
//! `NodeCraftApp::notice` stays the single "latest message" slot that most of the shell (and its
//! tests) reads and writes. This queue is what the canvas shows for it: a call site that knows
//! its message's kind goes through [`Notices::error`], [`Notices::success`] and friends, which
//! queue a typed entry and hand back the text for the slot; any other change of the slot is
//! picked up once a frame by [`Notices::sync`] and shown as information that follows the slot.
//!
//! Errors are sticky until dismissed; information, successes and warnings time out after
//! [`TIMEOUT_SECS`] while visible, paused while the pointer is over them or they hold focus.
//! Each entry has its kind's icon and coloured edge (colour is never the only cue), a close
//! button (Escape dismisses the focused one) and an optional command button. Only the newest
//! [`VISIBLE`] show, newest on top. All of it is view state: nothing here touches the document.
use super::*;
use crate::icons::Icon;
use std::time::Duration;

/// How many notices show at once, newest on top.
pub(crate) const VISIBLE: usize = 3;
/// How many are kept in all; past this the oldest non-sticky one goes first.
const MAX_ENTRIES: usize = 8;
/// How long a visible information, success or warning notice stays, in seconds.
pub(crate) const TIMEOUT_SECS: f32 = 6.0;
/// The accessible label of each notice's close button.
pub(crate) const DISMISS: &str = "Dismiss notice";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    Info,
    Success,
    Warning,
    Error,
}

impl NoticeKind {
    /// The edge and icon colour, from the theme.
    pub(crate) fn color(self, t: &Tokens) -> egui::Color32 {
        match self {
            Self::Info => t.text_muted,
            Self::Success => t.accent,
            Self::Warning => t.stale,
            Self::Error => t.danger,
        }
    }
    pub(crate) fn icon(self) -> Icon {
        match self {
            Self::Info => Icon::Info,
            Self::Success => Icon::Success,
            Self::Warning => Icon::Warning,
            Self::Error => Icon::Error,
        }
    }
    /// Errors interrupt a screen reader; everything else waits its turn.
    pub(crate) fn live(self) -> egui::accesskit::Live {
        match self {
            Self::Error => egui::accesskit::Live::Assertive,
            Self::Info | Self::Success | Self::Warning => egui::accesskit::Live::Polite,
        }
    }
}

/// A button on a notice that runs a command by its id, as the palette would.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoticeAction {
    pub label: &'static str,
    pub command: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct Notice {
    pub kind: NoticeKind,
    pub text: String,
    pub action: Option<NoticeAction>,
    /// Stays until dismissed (errors).
    pub sticky: bool,
    id: u64,
    /// Mirrors the legacy slot: shown while the slot holds its text, never timed out.
    legacy: bool,
    /// Seconds left while visible, for a timed notice.
    remaining: f32,
}

impl Notice {
    fn timed(&self) -> bool {
        !self.sticky && !self.legacy
    }
}

/// What one frame of the stack asked for.
#[derive(Default)]
pub(crate) struct Shown {
    pub dismissed: Vec<u64>,
    pub action: Option<(u64, &'static str)>,
}

#[derive(Default)]
pub(crate) struct Notices {
    /// Oldest first.
    entries: Vec<Notice>,
    /// The slot's text as last seen, to tell a change made outside these helpers.
    seen: Option<String>,
    next_id: u64,
    /// `ctx.input().time` when the stack was last shown; none while it isn't.
    last_time: Option<f64>,
    /// The notice whose close or action button held focus last frame (Escape dismisses it;
    /// egui drops focus on Escape before this frame's widgets run).
    focused: Option<u64>,
    /// The notices that were hovered or focused last frame: their timers wait.
    paused: Vec<u64>,
}

impl Notices {
    /// Queue `text` as `kind` and return it for the legacy slot:
    /// `self.notice = self.notices.notify(kind, text)`.
    pub(crate) fn notify(&mut self, kind: NoticeKind, text: impl Into<String>) -> Option<String> {
        self.push(kind, text.into(), None)
    }
    pub(crate) fn error(&mut self, text: impl Into<String>) -> Option<String> {
        self.notify(NoticeKind::Error, text)
    }
    pub(crate) fn success(&mut self, text: impl Into<String>) -> Option<String> {
        self.notify(NoticeKind::Success, text)
    }
    pub(crate) fn warning(&mut self, text: impl Into<String>) -> Option<String> {
        self.notify(NoticeKind::Warning, text)
    }
    /// Queue `text` with a button that runs `command`.
    pub(crate) fn with_action(&mut self, kind: NoticeKind, text: impl Into<String>, label: &'static str, command: &'static str) -> Option<String> {
        self.push(kind, text.into(), Some(NoticeAction { label, command }))
    }

    fn push(&mut self, kind: NoticeKind, text: String, action: Option<NoticeAction>) -> Option<String> {
        // The slot is about to hold this text, so the entry that mirrored it goes.
        self.entries.retain(|entry| !entry.legacy);
        self.insert(kind, text.clone(), action, false);
        self.seen = Some(text.clone());
        Some(text)
    }

    fn insert(&mut self, kind: NoticeKind, text: String, action: Option<NoticeAction>, legacy: bool) {
        // The same message again moves to the top rather than showing twice.
        if let Some(index) = self.entries.iter().position(|entry| entry.text == text) {
            let mut entry = self.entries.remove(index);
            entry.remaining = TIMEOUT_SECS;
            if !legacy {
                entry.kind = kind;
                entry.sticky = kind == NoticeKind::Error;
                entry.action = action;
                entry.legacy = false;
            }
            self.entries.push(entry);
            return;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.entries.push(Notice { kind, text, action, sticky: kind == NoticeKind::Error, id, legacy, remaining: TIMEOUT_SECS });
        while self.entries.len() > MAX_ENTRIES {
            let index = self.entries.iter().position(|entry| !entry.sticky).unwrap_or(0);
            if index >= self.entries.len() {
                break;
            }
            self.entries.remove(index);
        }
    }

    /// Follow a change of the legacy slot made without these helpers: the mirrored entry goes,
    /// and new text shows as information. Typed entries stay (errors until dismissed).
    pub(crate) fn sync(&mut self, slot: &Option<String>) {
        if self.seen.as_deref() == slot.as_deref() {
            return;
        }
        self.entries.retain(|entry| !entry.legacy);
        if let Some(text) = slot {
            self.insert(NoticeKind::Info, text.clone(), None, true);
        }
        self.seen = slot.clone();
    }

    /// Remove one notice; the slot is cleared too when it still holds that message.
    pub(crate) fn dismiss(&mut self, id: u64, slot: &mut Option<String>) {
        if let Some(index) = self.entries.iter().position(|entry| entry.id == id) {
            let entry = self.entries.remove(index);
            if slot.as_deref() == Some(entry.text.as_str()) {
                *slot = None;
                self.seen = None;
            }
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The notices on screen, newest first.
    pub(crate) fn visible(&self) -> impl Iterator<Item = &Notice> {
        self.entries.iter().rev().take(VISIBLE)
    }

    /// The stack isn't on screen this frame: timers and focus wait.
    pub(crate) fn idle(&mut self) {
        self.last_time = None;
        self.focused = None;
        self.paused.clear();
    }

    /// Start a shown frame: Escape dismisses the notice that held focus, and visible timed
    /// notices that weren't hovered or focused count down; the expired ones go.
    pub(crate) fn begin(&mut self, ctx: &egui::Context, slot: &mut Option<String>) {
        let (now, escape) = ctx.input(|i| (i.time, i.key_pressed(egui::Key::Escape)));
        if escape && let Some(id) = self.focused.take() {
            self.dismiss(id, slot);
        }
        let elapsed = self.last_time.map_or(0.0, |last| now - last);
        // A clock that jumps back or isn't finite counts as no time.
        let elapsed = if elapsed.is_finite() && elapsed > 0.0 { elapsed as f32 } else { 0.0 };
        self.last_time = Some(now);
        let visible: Vec<u64> = self.visible().map(|entry| entry.id).collect();
        for entry in &mut self.entries {
            if entry.timed() && visible.contains(&entry.id) && !self.paused.contains(&entry.id) {
                entry.remaining -= elapsed;
            }
        }
        self.entries.retain(|entry| !entry.timed() || entry.remaining > 0.0);
        // Wake for the next visible timed notice to leave.
        let next = self.visible().filter(|entry| entry.timed() && !self.paused.contains(&entry.id)).map(|entry| entry.remaining).fold(f32::INFINITY, f32::min);
        if let Ok(after) = Duration::try_from_secs_f32(next) {
            ctx.request_repaint_after(after);
        }
    }

    /// Lay out the visible notices, newest on top, and say what was clicked.
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, t: &Tokens) -> Shown {
        let mut shown = Shown::default();
        let mut focused = None;
        let mut paused = Vec::new();
        for (index, notice) in self.entries.iter().rev().take(VISIBLE).enumerate() {
            if index > 0 {
                ui.add_space(t.control_gap);
            }
            let color = notice.kind.color(t);
            let row = ui.push_id(("notice", notice.id), |ui| {
                ui.horizontal_top(|ui| {
                    ui.add_space(t.notice_edge + t.control_gap);
                    ui.add(notice.kind.icon().image(ui.ctx(), t.icon_size, t).tint(color));
                    let close_width = t.icon_size + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x;
                    let text_width = (ui.available_width() - close_width).max(t.icon_size);
                    let (label, action) = ui
                        .scope(|ui| {
                            ui.set_max_width(text_width);
                            ui.vertical(|ui| {
                                let label = ui.add(egui::Label::new(notice.text.as_str()).wrap());
                                let action = notice.action.as_ref().map(|action| (action.command, ui.small_button(action.label)));
                                (label, action)
                            })
                            .inner
                        })
                        .inner;
                    let close = ui.add(egui::Button::image(Icon::Close.image(ui.ctx(), t.icon_size, t)));
                    close.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), DISMISS));
                    let close = close.on_hover_text(DISMISS);
                    (label, action, close)
                })
            });
            let (label, action, close) = row.inner.inner;
            let rect = row.response.rect;
            ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(t.notice_edge, rect.height())), 0.0, color);
            ui.ctx().accesskit_node_builder(label.id, |node| {
                node.set_live(notice.kind.live());
                node.set_live_atomic();
            });
            let action_focus = action.as_ref().is_some_and(|(_, button)| button.has_focus());
            if close.has_focus() || action_focus {
                focused = Some(notice.id);
            }
            if close.has_focus() || action_focus || ui.rect_contains_pointer(rect) {
                paused.push(notice.id);
            }
            if close.clicked() {
                shown.dismissed.push(notice.id);
            }
            if let Some((command, button)) = action
                && button.clicked()
            {
                shown.action = Some((notice.id, command));
            }
        }
        self.focused = focused;
        self.paused = paused;
        shown
    }
}

impl NodeCraftApp {
    /// Show `text` as a notice of `kind`, and keep it in the latest-message slot.
    pub(crate) fn notify(&mut self, kind: NoticeKind, text: impl Into<String>) {
        self.notice = self.notices.notify(kind, text);
    }
}
