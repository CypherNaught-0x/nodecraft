//! Explicit, bounded presentation of the engine's read-only last staleness check.
use super::*;
use crate::navigation::{Geometry, SessionIdentity};
use nodecraft_types::ProjectId;
use std::sync::Arc;

const MAX_TEXT: usize = 1536;
const MAX_REASON: usize = 512;
const MALFORMED: &str = "The staleness report could not be displayed safely.";

pub(super) struct StaleSnapshot {
    identity: Arc<SessionIdentity>,
    project: ProjectId,
    canvas: CanvasId,
    revision: u64,
    text: String,
}
impl StaleSnapshot {
    fn capture(app: &mut NodeCraftApp) -> Self {
        Self {
            identity: Geometry::identity(&mut app.session),
            project: app.session.document().id(),
            canvas: app.canvas,
            revision: app.session.revision(),
            text: String::new(),
        }
    }
    fn matches(&self, app: &mut NodeCraftApp) -> bool {
        Arc::ptr_eq(&self.identity, &Geometry::identity(&mut app.session))
            && self.project == app.session.document().id()
            && self.canvas == app.canvas
            && self.revision == app.session.revision()
            && app.session.document().canvases().contains_key(&app.canvas)
    }
}
fn append(text: &mut String, value: &str) -> Result<(), &'static str> {
    if text.len().checked_add(value.len()).is_none_or(|n| n > MAX_TEXT) {
        return Err(MALFORMED);
    }
    text.push_str(value);
    Ok(())
}
fn normalized(text: &mut String, value: &str) -> Result<(), &'static str> {
    for c in value.chars() {
        let c = if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { ' ' } else { c };
        append(text, c.encode_utf8(&mut [0; 4]))?;
    }
    Ok(())
}
fn kind(value: &str) -> Option<&'static str> {
    Some(match value {
        "invalid_input" => "invalid_input",
        "unsupported" => "unsupported",
        "not_found" => "not_found",
        "conflict" => "conflict",
        "overflow" => "overflow",
        "underflow" => "underflow",
        "currency_mismatch" => "currency_mismatch",
        "authentication" => "authentication",
        "network" => "network",
        "timeout" => "timeout",
        "busy" => "busy",
        "budget" => "budget",
        "content_blocked" => "content_blocked",
        "internal" => "internal",
        "unknown" => "unknown",
        _ => return None,
    })
}
fn summary(report: &serde_json::Value, total: usize) -> Result<String, &'static str> {
    let stale = report.get("nodes").and_then(serde_json::Value::as_array).ok_or(MALFORMED)?.len();
    let unknown = report.get("unknown_total").and_then(serde_json::Value::as_u64).and_then(|n| usize::try_from(n).ok()).ok_or(MALFORMED)?;
    if stale > total || unknown > total {
        return Err(MALFORMED);
    }
    let diagnostic = report.get("unknown").ok_or(MALFORMED)?;
    let mut text = String::new();
    text.try_reserve_exact(MAX_TEXT).map_err(|_| MALFORMED)?;
    append(&mut text, "Last staleness check: ")?;
    append(&mut text, &stale.to_string())?;
    append(&mut text, " stale results; ")?;
    append(&mut text, &unknown.to_string())?;
    append(&mut text, " checked results could not be hashed.")?;
    if unknown == 0 {
        if !diagnostic.is_null() {
            return Err(MALFORMED);
        }
        if stale == 0 {
            append(&mut text, " No stale or locally invalid checked results reported.")?;
        }
    } else {
        let uuid = |field: &str| -> Result<&str, &'static str> {
            let value = diagnostic.get(field).and_then(serde_json::Value::as_str).filter(|s| s.len() == 36).ok_or(MALFORMED)?;
            value.parse::<NodeId>().map_err(|_| MALFORMED)?;
            Ok(value)
        };
        let node = uuid("node")?;
        let cause = uuid("cause")?;
        let category = diagnostic.get("kind").and_then(serde_json::Value::as_str).and_then(kind).ok_or(MALFORMED)?;
        let reason = diagnostic.get("reason").and_then(serde_json::Value::as_str).filter(|s| s.len() <= MAX_REASON).ok_or(MALFORMED)?;
        append(&mut text, "\nFirst invalid checked result: ")?;
        append(&mut text, node)?;
        append(&mut text, ". Failing input node: ")?;
        append(&mut text, cause)?;
        append(&mut text, ". ")?;
        append(&mut text, category)?;
        append(&mut text, ": ")?;
        normalized(&mut text, reason)?;
    }
    append(
        &mut text,
        "\nOnly results with recorded hashes are hash-checked; stale status can propagate downstream. Unrecorded results are not verified fresh.",
    )?;
    Ok(text)
}
impl NodeCraftApp {
    pub(super) fn palette_command_params(&self, id: &str) -> serde_json::Value {
        if id == "canvas.stale" { json!({"canvas": self.canvas}) } else { json!({}) }
    }
    pub(super) fn dispatch_palette_command(&mut self, id: &str) {
        let stamp = (id == "canvas.stale").then(|| StaleSnapshot::capture(self));
        let result = self.session.dispatch(id, &self.palette_command_params(id));
        let Some(mut stamp) = stamp else {
            match result {
                Ok(_) => self.notice = None,
                Err(error) => self.notice = Some(error.message),
            }
            return;
        };
        self.stale_snapshot = None;
        let outcome = match result {
            Ok(report) if stamp.matches(self) => {
                let total = self.session.document().canvases().get(&self.canvas).map_or(0, |canvas| canvas.nodes().len());
                summary(&report, total)
            }
            Ok(_) => Err("The staleness check changed context; run it again."),
            Err(error) => {
                let mut message = String::new();
                if message.try_reserve_exact(MAX_TEXT).is_err() {
                    self.notice = Some(MALFORMED.into());
                    return;
                }
                let _ = append(&mut message, "Staleness check failed (");
                let category = match error.kind {
                    ErrorKind::InvalidInput => "invalid_input",
                    ErrorKind::Unsupported => "unsupported",
                    ErrorKind::NotFound => "not_found",
                    ErrorKind::Conflict => "conflict",
                    ErrorKind::Overflow => "overflow",
                    ErrorKind::Underflow => "underflow",
                    ErrorKind::CurrencyMismatch => "currency_mismatch",
                    ErrorKind::Authentication => "authentication",
                    ErrorKind::Network => "network",
                    ErrorKind::Timeout => "timeout",
                    ErrorKind::Busy => "busy",
                    ErrorKind::Budget => "budget",
                    ErrorKind::ContentBlocked => "content_blocked",
                    ErrorKind::Internal => "internal",
                    _ => "unknown",
                };
                let _ = append(&mut message, category);
                let _ = append(&mut message, "): ");
                let mut bytes = 0;
                for c in error.message.chars() {
                    if bytes + c.len_utf8() > MAX_REASON {
                        break;
                    }
                    bytes += c.len_utf8();
                    let _ = normalized(&mut message, c.encode_utf8(&mut [0; 4]));
                }
                self.notice = Some(message);
                return;
            }
        };
        match outcome {
            Ok(text) => {
                stamp.text = text;
                self.stale_snapshot = Some(stamp);
            }
            Err(message) => self.notice = Some(message.into()),
        }
    }
    pub(super) fn show_notices(&mut self, ui: &egui::Ui, viewport: Rect, t: &Tokens) {
        if let Some(snapshot) = self.stale_snapshot.take()
            && snapshot.matches(self)
        {
            self.stale_snapshot = Some(snapshot);
        }
        let hidden = self
            .editors
            .values()
            .any(|editor| editor.hidden_dirty)
            .then_some("Unsaved draft in a hidden node or field. Zoom or pan back to edit it, or discard drafts.");
        let notice = self.notice.as_deref().or(hidden);
        // A pending drop shows its own progress in this corner.
        if (notice.is_some() || self.stale_snapshot.is_some()) && !self.drop_pending() {
            let mut dismiss_check = false;
            let notice_id = ui.id().with("notice");
            let previous_size = egui::AreaState::load(ui.ctx(), notice_id).map(|state| state.rect().size());
            let area = egui::Area::new(notice_id)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::LEFT_BOTTOM, Vec2::new(t.card_padding, -t.card_padding))
                .show(ui.ctx(), |ui| {
                    egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding).show(ui, |ui| {
                        ui.set_max_width((2.0 * t.card_width).min(viewport.width().max(0.0)));
                        if let Some(notice) = notice {
                            ui.label(notice);
                        }
                        if hidden.is_some() && ui.button("Discard drafts").clicked() {
                            self.editors.clear();
                        }
                        if let Some(snapshot) = &self.stale_snapshot {
                            ui.add(egui::Label::new(snapshot.text.as_str()).wrap());
                            dismiss_check = ui.small_button("Dismiss check").clicked();
                        }
                    });
                });
            // A changed bottom-anchored area needs a settled frame before its controls are hit-tested.
            if previous_size != Some(area.response.rect.size()) {
                ui.ctx().request_repaint();
            }
            if dismiss_check {
                self.stale_snapshot = None;
            }
            for (open, id) in [(self.palette.is_some(), "quick_add_palette"), (self.turn_into.is_some(), "turn_into_menu")] {
                if open {
                    ui.ctx().move_to_top(egui::LayerId::new(egui::Order::Foreground, egui::Id::new(id)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
