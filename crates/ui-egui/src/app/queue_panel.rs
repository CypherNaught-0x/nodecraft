//! The queue panel (#82): every job the run host is running or recently ran, newest first, with
//! its card, provider and model, status and price. A live job can be cancelled; a failed or
//! cancelled model job can be retried, which quotes it again first like Generate does.
use super::*;
use nodecraft_engine::run_jobs::{JobState, JobView, cause_label, cause_of, fix_hint};

/// What a click in the panel asked for, applied after it is drawn.
enum Action {
    Cancel(nodecraft_engine::run_jobs::JobKey),
    Retry(NodeId),
    Clear,
}

impl NodeCraftApp {
    /// The toggle's label: "Queue", or "Queue (N)" while N jobs are queued or running.
    pub(super) fn queue_label(&self) -> Option<String> {
        let runs = self.runs.as_ref()?;
        let live = runs.jobs().iter().filter(|view| !view.state.is_final()).count();
        Some(if live == 0 { "Queue".into() } else { format!("Queue ({live})") })
    }

    pub(super) fn queue_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.queue_open {
            return;
        }
        let Some(runs) = &self.runs else { return };
        let jobs: Vec<JobView> = runs.jobs().iter().rev().cloned().collect();
        let mut open = true;
        let mut action = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Queue")
            .id(egui::Id::new("nodecraft.queue"))
            .open(&mut open)
            // Under its toggle and search; notices own the bottom-left corner, the map the bottom-right.
            .anchor(egui::Align2::RIGHT_TOP, [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap])
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                if jobs.is_empty() {
                    ui.label(egui::RichText::new("Nothing has run yet. Jobs appear here while they run.").color(t.text_muted));
                    return;
                }
                egui::ScrollArea::vertical().max_height(t.minimap_height * 2.0).show(ui, |ui| {
                    egui::Grid::new("queue_rows").num_columns(5).spacing([t.control_gap * 2.0, t.control_gap]).striped(true).show(ui, |ui| {
                        for view in &jobs {
                            self.queue_row(ui, view, t, &mut action);
                            ui.end_row();
                        }
                    });
                });
                if jobs.iter().any(|view| view.state.is_final()) && ui.button("Clear finished").clicked() {
                    action = Some(Action::Clear);
                }
            });
        self.queue_open = open;
        let result = match action {
            Some(Action::Cancel(key)) => self.runs.as_mut().map_or(Ok(()), |runs| runs.cancel(key)),
            Some(Action::Retry(node)) => self.request_run(node, super::generate::RunKind::Generate),
            Some(Action::Clear) => {
                if let Some(runs) = &mut self.runs {
                    runs.clear_finished();
                }
                Ok(())
            }
            None => Ok(()),
        };
        if let Err(error) = result {
            self.notice = Some(error.message);
        }
    }

    fn queue_row(&self, ui: &mut egui::Ui, view: &JobView, t: &Tokens, action: &mut Option<Action>) {
        let node = self.session.document().canvases().get(&view.canvas).and_then(|canvas| canvas.nodes().get(&view.node));
        let title = node.map_or_else(
            || "Deleted card".to_owned(),
            |node| crate::card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor())),
        );
        ui.label(title);
        match (&view.provider, &view.model) {
            (Some(provider), Some(model)) => ui.label(format!("{provider} · {model}")),
            (Some(provider), None) => ui.label(provider.as_str()),
            _ => ui.label(egui::RichText::new("Local").color(t.text_muted)),
        };
        let color = match &view.state {
            JobState::Failed { .. } => t.danger,
            JobState::Done | JobState::Cancelled => t.text_muted,
            _ => t.accent,
        };
        let status = ui.label(egui::RichText::new(view.status()).color(color));
        if let JobState::Failed { reason, cause } = &view.state {
            let hint = cause.or_else(|| cause_of(reason)).map(fix_hint);
            status.on_hover_text(hint.map_or_else(|| reason.clone(), |hint| format!("{reason}\n\n{hint}")));
        }
        ui.label(price(view));
        if !view.state.is_final() {
            if ui.add_enabled(!view.cancel_requested, egui::Button::new("Cancel")).on_hover_text("Stop this job").clicked() {
                *action = Some(Action::Cancel(view.key));
            }
        } else if matches!(view.state, JobState::Failed { .. } | JobState::Cancelled) && view.provider.is_some() {
            let can = view.canvas == self.canvas && node.is_some() && self.runs.as_ref().is_some_and(|runs| !runs.is_running(view.canvas, view.node));
            if ui.add_enabled(can, egui::Button::new("Retry")).on_hover_text("Quote this card again and run it").clicked() {
                *action = Some(Action::Retry(view.node));
            }
        } else {
            ui.label("");
        }
    }
}

/// The price column. A job that may have reached the provider is never shown as refunded or free.
fn price(view: &JobView) -> String {
    if view.provider.is_none() {
        return "Free".into();
    }
    let estimate = view.estimate.map(nodecraft_exec::cost::format_money);
    match (&view.state, estimate) {
        (JobState::Cancelled, _) if !view.sent => "Not charged".into(),
        (JobState::Cancelled | JobState::Failed { .. }, Some(estimate)) if view.sent => format!("{estimate}, unresolved"),
        (JobState::Cancelled | JobState::Failed { .. }, None) if view.sent => "Unresolved".into(),
        (_, Some(estimate)) => estimate,
        (_, None) => "Unpriced".into(),
    }
}

/// The run status a card shows under itself: what its job is doing, or why its last run failed.
pub(super) fn card_status(runs: &super::runs::Runs, canvas: CanvasId, node: NodeId, t: &Tokens) -> Option<(String, egui::Color32, Option<String>)> {
    if let Some(status) = runs.card_status(canvas, node) {
        return Some((status, t.accent, None));
    }
    let why = runs.failure(canvas, node)?;
    let cause = cause_of(why);
    let label = cause.map_or_else(|| "Run failed".to_owned(), |cause| format!("Run failed: {}", cause_label(cause)));
    let hover = cause.map_or_else(|| why.to_owned(), |cause| format!("{why}\n\n{}", fix_hint(cause)));
    Some((label, t.danger, Some(hover)))
}
