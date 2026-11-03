//! Human folder choice and preview for the shared workflow-result commands (#966).
use super::{NodeCraftApp, Target};
use nodecraft_engine::{
    export_job::{ExportResult, ExportWatch},
    local_export::{ExportHost, dispatch_with_host},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

pub(super) enum State {
    Preview { session: Arc<()>, revision: u64, host: ExportHost, params: Value, pattern: String, active: bool, preview: Result<Value, String> },
    Working(ExportWatch),
    Done(ExportResult),
}
impl NodeCraftApp {
    /// Reusable result selector seam: None saves selected nodes/all nodes, explicit targets
    /// identify historical generations and output items without changing history selection.
    pub(in crate::app) fn start_results_save(&mut self, targets: Option<Vec<nodecraft_engine::results_export::ResultSelector>>) {
        if !cfg!(all(unix, not(target_arch = "wasm32"))) {
            self.notice =
                Some("Secure folder saving is currently available on macOS and Linux; export individual results with Save as on this platform.".into());
            return;
        }
        let mut params = json!({"canvas":self.canvas,"history":"all"});
        if let Some(targets) = targets {
            params["targets"] = json!(targets);
        } else {
            params["nodes"] = json!(self.selection.nodes());
        }
        self.pick_export(Target::Results(params), "");
    }
    pub(super) fn results_folder(&mut self, mut params: Value, path: PathBuf) {
        let result = (|| {
            let host = ExportHost::authorize(&path)?;
            params["destination"] = json!(path);
            let preview = dispatch_with_host(&mut self.session, &host, "export.results_plan", &params)?;
            Ok::<_, nodecraft_types::Error>((host, preview))
        })();
        match result {
            Ok((host, preview)) => {
                self.exports.results = Some(State::Preview {
                    session: self.session.view_identity(),
                    revision: self.session.revision(),
                    host,
                    params,
                    pattern: nodecraft_engine::results_export::DEFAULT_TEMPLATE.into(),
                    active: false,
                    preview: Ok(preview),
                })
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
    pub(in crate::app) fn results_watch(&self) -> Option<ExportWatch> {
        match self.exports.results.as_ref() {
            Some(State::Working(watch)) if watch.status().result.is_none() => Some(watch.clone()),
            _ => None,
        }
    }
    pub(in crate::app) fn results_overlay(&mut self, ctx: &egui::Context) -> bool {
        let Some(mut state) = self.exports.results.take() else {
            return false;
        };
        let t = *self.theme.tokens();
        let mut dismiss = false;
        let mut start = false;
        let mut cancel = None;
        egui::Modal::new(egui::Id::new("results_save966"))
            .frame(egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding))
            .backdrop_color(t.canvas_bg)
            .show(ctx, |ui| {
                ui.set_max_width(t.card_width * 2.0);
                match &mut state {
                    State::Preview { session, revision, host, params, pattern, active, preview } => {
                        ui.heading("Save workflow results");
                        ui.label("Original media and recorded run settings. Existing files are kept.");
                        let explicit = params.get("targets").is_some();
                        let mut changed = false;
                        if !explicit {
                            ui.horizontal(|ui| {
                                changed |= ui.selectable_value(active, false, "All history").changed();
                                changed |= ui.selectable_value(active, true, "Active results only").changed();
                            });
                        } else {
                            ui.label("Only the chosen historical result items.");
                        }
                        ui.label("Filename pattern (original extension is added)");
                        changed |= ui.add(egui::TextEdit::singleline(pattern).char_limit(128)).changed();
                        ui.small("{title}  {generation}  {index}  {node}  {output}  {item}  {hash}");
                        if !Arc::ptr_eq(session, &self.session.view_identity()) {
                            *preview = Err("The project was replaced; choose results and a folder again.".into());
                        } else if changed || *revision != self.session.revision() {
                            params["template"] = json!(pattern);
                            params["history"] = json!(if *active { "active" } else { "all" });
                            *preview = dispatch_with_host(&mut self.session, host, "export.results_plan", params).map_err(|e| e.message);
                            *revision = self.session.revision();
                        }
                        match preview {
                            Ok(preview) => {
                                ui.label(format!("{} media files; {} ready", preview["total"], preview["ready"]));
                                ui.label("First filenames:");
                                if let Some(examples) = preview["examples"].as_array() {
                                    for example in examples {
                                        ui.label(example["file"].as_str().or_else(|| example["error"].as_str()).unwrap_or("Unavailable filename"));
                                    }
                                }
                                ui.label("workflow-results.json records actual names, hashes and historical settings. Name clashes receive a numbered suffix.");
                            }
                            Err(reason) => {
                                ui.colored_label(t.danger, reason.as_str());
                            }
                        }
                        ui.horizontal(|ui| {
                            start = ui.add_enabled(preview.is_ok(), egui::Button::new("Save files")).clicked();
                            dismiss = ui.button("Cancel").clicked();
                        });
                    }
                    State::Working(watch) => {
                        let status = watch.status();
                        ui.heading("Saving workflow results…");
                        if let Some(progress) = status.progress {
                            ui.label(format!("{} of {} checked • {} saved • {} failed", progress.completed, progress.total, progress.written, progress.failed));
                            let ratio = if progress.total == 0 { 0.0 } else { progress.completed as f32 / progress.total as f32 };
                            ui.add(egui::ProgressBar::new(ratio));
                        } else {
                            ui.label("Waiting for the background worker…");
                        }
                        ui.label("Completed files are kept when cancelled.");
                        if ui
                            .add_enabled(
                                status.cancellable && self.session.export_watch().is_some_and(|current| current.same_job(watch)),
                                egui::Button::new("Cancel save"),
                            )
                            .clicked()
                        {
                            cancel = Some(status.id);
                        }
                        ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    }
                    State::Done(result) => {
                        ui.heading("Workflow result save finished");
                        match result {
                            ExportResult::Results { report } => {
                                ui.label(format!(
                                    "{} saved • {} failed{}",
                                    report.written(),
                                    report.failed(),
                                    if report.cancelled { " • cancelled" } else { "" }
                                ));
                                if let Some(name) = &report.manifest {
                                    ui.label(format!("Manifest: {name}"));
                                }
                                if let Some(error) = &report.manifest_error {
                                    ui.colored_label(t.danger, format!("Manifest: {error}"));
                                }
                                egui::ScrollArea::vertical().max_height(t.card_width).show(ui, |ui| {
                                    for entry in &report.entries {
                                        ui.label(format!(
                                            "{}: {}{}",
                                            entry.file.as_deref().or(entry.suggested_name.as_deref()).unwrap_or("Result"),
                                            entry.state,
                                            entry.reason.as_ref().map(|e| format!(" — {e}")).unwrap_or_default()
                                        ));
                                    }
                                });
                            }
                            ExportResult::Failed { error, .. } => {
                                ui.colored_label(t.danger, &error.message);
                            }
                            ExportResult::Cancelled => {
                                ui.label("Save cancelled.");
                            }
                            ExportResult::Exported { .. } => {
                                ui.label("Saved.");
                            }
                        }
                        dismiss = ui.button("Done").clicked();
                    }
                }
            });
        if let Some(id) = cancel
            && let Err(error) = self.session.dispatch("export.cancel", &json!({"id":id}))
        {
            self.notice = Some(error.message);
        }
        if start && let State::Preview { host, params, .. } = &state {
            if let Some(runner) = &self.exports.runner {
                self.session.set_export_runner(Arc::clone(runner));
            }
            match dispatch_with_host(&mut self.session, host, "export.results_start", params) {
                Ok(_) => {
                    if let Some(watch) = self.session.export_watch() {
                        state = State::Working(watch);
                    }
                }
                Err(error) => self.notice = Some(error.message),
            }
        }
        if let State::Working(watch) = &state
            && let Some(result) = watch.status().result
        {
            state = State::Done(result);
        }
        if !dismiss {
            self.exports.results = Some(state);
        }
        true
    }
}
