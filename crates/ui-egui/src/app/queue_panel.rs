//! The queue panel (#82): every job the run host is running or recently ran, newest first, with
//! its card, provider and model, status and price. A live job can be cancelled; a failed or
//! cancelled model job can be retried, which quotes it again first like Generate does.
//!
//! Above the jobs, paid results a run couldn't keep (#369) wait for a decision: record again,
//! put on the canvas, or discard. None of these calls the provider again.
use super::*;
use nodecraft_engine::run_host::{HeldAction, HeldView, MAX_HELD};
use nodecraft_engine::run_jobs::{JobState, JobView, cause_label, cause_of, fix_hint};

/// What a click in the panel asked for, applied after it is drawn.
enum Action {
    Cancel(nodecraft_engine::run_jobs::JobKey),
    Retry(NodeId),
    Clear,
    /// A held result's command (`run.held_retry`, …) and its id.
    Held(&'static str, u64),
}

impl NodeCraftApp {
    /// The toggle's label: "Queue", or "Queue (N)" while N jobs are queued or running.
    pub(super) fn queue_label(&self) -> Option<String> {
        let runs = self.runs.as_ref()?;
        let live = runs.jobs().iter().filter(|view| !view.state.is_final()).count();
        let held = runs.held().len();
        Some(match (live, held) {
            (0, 0) => "Queue".into(),
            (live, 0) => format!("Queue ({live})"),
            (live, held) => format!("Queue ({live}, {held} held)"),
        })
    }

    pub(super) fn queue_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.queue_open {
            return;
        }
        let Some(runs) = &self.runs else { return };
        let jobs: Vec<JobView> = runs.jobs().iter().rev().cloned().collect();
        let held: Vec<HeldView> = runs.held().to_vec();
        let mut open = true;
        let mut action = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Queue")
            .id(egui::Id::new("nodecraft.queue"))
            .open(&mut open)
            // Under its toggle and search; notices own the bottom-left corner, the map the bottom-right.
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap])
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                if !held.is_empty() {
                    self.held_section(ui, &held, t, &mut action);
                }
                if jobs.is_empty() && held.is_empty() {
                    ui.label(egui::RichText::new("Nothing has run yet. Jobs appear here while they run.").color(t.text_muted));
                    return;
                }
                if jobs.is_empty() {
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
            Some(Action::Held(command, id)) => {
                let done = self.session.dispatch(command, &serde_json::json!({"id": id})).map(|_| ());
                if let Some(runs) = &mut self.runs {
                    runs.refresh_held();
                }
                done
            }
            None => Ok(()),
        };
        if let Err(error) = result {
            self.notice = Some(error.message);
        }
    }

    /// The held results, each with what can be done with it.
    fn held_section(&self, ui: &mut egui::Ui, held: &[HeldView], t: &Tokens, action: &mut Option<Action>) {
        let full = held.len() >= MAX_HELD;
        let heading = if full {
            format!("{} paid results need a decision before anything else runs", held.len())
        } else {
            format!("{} paid result{} couldn't be kept", held.len(), if held.len() == 1 { "" } else { "s" })
        };
        ui.label(egui::RichText::new(heading).color(if full { t.danger } else { t.text }));
        ui.label(egui::RichText::new("They are kept until NodeCraft quits. Nothing here is sent or charged again.").color(t.text_muted));
        egui::Grid::new("queue_held").num_columns(3).spacing([t.control_gap * 2.0, t.control_gap]).striped(true).show(ui, |ui| {
            for view in held {
                let node = self.session.document().canvases().get(&view.canvas).and_then(|canvas| canvas.nodes().get(&view.node));
                let title = node.map_or_else(
                    || "Deleted card".to_owned(),
                    |node| crate::card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor())),
                );
                ui.label(title).on_hover_text(format!("{} · {}", view.provider, view.model));
                ui.label(egui::RichText::new(&view.reason).color(t.danger));
                ui.horizontal(|ui| {
                    let idle = view.pending.is_none();
                    let retry = ui.add_enabled(idle && view.retry, egui::Button::new("Retry"));
                    let retry = if view.retry {
                        retry.on_hover_text("Record it on its card again")
                    } else {
                        retry.on_disabled_hover_text("Its step needed other jobs' results too")
                    };
                    if retry.clicked() {
                        *action = Some(Action::Held("run.held_retry", view.id));
                    }
                    if ui.add_enabled(idle, egui::Button::new("Put on canvas")).on_hover_text("Add its outputs as source nodes beside its card").clicked() {
                        *action = Some(Action::Held("run.held_place", view.id));
                    }
                    if ui.add_enabled(idle, egui::Button::new("Discard")).on_hover_text("Let it go; its charge stands").clicked() {
                        *action = Some(Action::Held("run.held_discard", view.id));
                    }
                    if let Some(pending) = view.pending {
                        ui.label(egui::RichText::new(if pending == HeldAction::Retry { "Retrying…" } else { "Placing…" }).color(t.text_muted));
                    }
                });
                ui.end_row();
            }
        });
        ui.separator();
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

/// The price column: the run's estimate, since the settled charge isn't shown here yet. A job
/// that may have reached the provider is never shown as refunded or free.
fn price(view: &JobView) -> String {
    if view.provider.is_none() {
        return "Free".into();
    }
    let estimate = view.estimate.map(nodecraft_exec::cost::format_money);
    match (&view.state, estimate) {
        (JobState::Cancelled | JobState::Failed { .. }, _) if !view.sent => "Not charged".into(),
        (JobState::Cancelled | JobState::Failed { .. }, Some(estimate)) => format!("{estimate} est., unresolved"),
        (JobState::Cancelled | JobState::Failed { .. }, None) => "Unresolved".into(),
        (_, Some(estimate)) => format!("{estimate} est."),
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
