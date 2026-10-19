//! One model-choice controller for card and bulk intents. The engine owns migration/commit.
use super::*;
use crate::model_picker::{ModelPicker, Outcome};
use nodecraft_engine::{
    generation_catalog::{GenerationCatalog, GenerationCatalogStamp, GenerationWiring},
    model_swap::Preview,
};

enum Step {
    Picking(ModelPicker),
    Review(Preview),
}
pub(super) struct ModelChoice {
    snapshot: GenerationCatalog,
    stamp: GenerationCatalogStamp,
    session: Arc<()>,
    canvas: CanvasId,
    nodes: Vec<NodeId>,
    revision: u64,
    step: Step,
    stale: bool,
    error: Option<String>,
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn allocation() -> Error {
    Error::new(ErrorKind::Internal, "model picker storage could not be reserved")
}

impl NodeCraftApp {
    /// The common card/bulk opening intent. Selection does not authorize a run or an edit.
    pub fn open_model_picker(&mut self, canvas: CanvasId, mut exact_ids: Vec<NodeId>, expected_revision: u64) -> Result<(), Error> {
        if canvas != self.canvas || expected_revision != self.session.revision() {
            return Err(Error::new(ErrorKind::Conflict, "model selection changed; select the nodes again"));
        }
        if exact_ids.is_empty() || exact_ids.len() > nodecraft_graph::MAX_CHANGES {
            return Err(invalid("select a bounded nonempty set of model nodes"));
        }
        exact_ids.sort_unstable();
        if exact_ids.windows(2).any(|p| p.first() == p.get(1)) {
            return Err(invalid("model selection contains duplicate node IDs"));
        }
        if self.palette.is_some()
            || self.turn_into.is_some()
            || self.keys.open
            || self.drag.is_some()
            || self.marquee.is_some()
            || self.wiring.is_some()
            || self.reordering.is_some()
            || self.navigation.pointer_owned
            || self.navigation.drag.is_some()
            || self.comments.drag.is_some()
            || self.comments.gesture_cancelled
            || self.drop_pointer_owned()
        {
            return Err(Error::new(ErrorKind::Conflict, "finish the current canvas interaction before choosing a model"));
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.files.blocks_model_picker() || self.session.file_status().operation.is_some() || self.recovery.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "finish the current file operation before choosing a model"));
        }
        let snapshot = self.session.generation_catalog()?.clone();
        let live = self.session.document().canvases().get(&canvas).ok_or_else(|| invalid("model canvas is unavailable"))?;
        // Captures own port/count slots only, borrowing payload-free Port metadata. Admit the
        // entire temporary capture before allocating it; it is gone before the first paint.
        let mut capture_bytes = exact_ids.len().checked_mul(std::mem::size_of::<GenerationWiring<'_>>()).ok_or_else(allocation)?;
        for id in &exact_ids {
            let node = live.nodes().get(id).ok_or_else(|| invalid("selected model node is unavailable"))?;
            if node.kind != nodecraft_nodes::generation::KIND {
                return Err(Error::new(ErrorKind::Unsupported, "this selection contains a node without a model-swap contract"));
            }
            snapshot.validate_instance_binding(self.session.kinds().get(&node.kind)?, node)?;
            capture_bytes = capture_bytes
                .checked_add(node.inputs.len().checked_mul(std::mem::size_of::<(&nodecraft_graph::Port, usize)>()).ok_or_else(allocation)?)
                .ok_or_else(allocation)?;
            if capture_bytes > MAX_SCHEMA_CACHE_BYTES {
                return Err(Error::new(ErrorKind::Budget, "model input metadata exceeds the view limit; select fewer nodes"));
            }
        }
        let mut captures = Vec::new();
        captures.try_reserve_exact(exact_ids.len()).map_err(|_| allocation())?;
        for id in &exact_ids {
            captures.push(snapshot.capture_wiring(live, *id)?);
        }
        let picker = ModelPicker::for_targets(snapshot.run_catalog().catalog(), |model| {
            for wiring in &captures {
                snapshot.validate_destination_wiring(wiring, &model.id, true).map_err(|e| e.message)?;
            }
            Ok(())
        })?;
        drop(captures);
        self.selection.replace(exact_ids.iter().copied().collect());
        self.model_choice = Some(ModelChoice {
            stamp: snapshot.stamp(),
            snapshot,
            session: self.session.view_identity(),
            canvas,
            nodes: exact_ids,
            revision: expected_revision,
            step: Step::Picking(picker),
            stale: false,
            error: None,
        });
        self.model_frame_owned = true;
        Ok(())
    }

    fn model_targets_dirty(&self, ids: &[NodeId]) -> bool {
        ids.iter().any(|id| self.editors.get(id).is_some_and(EditorState::has_uncommitted))
    }

    /// Called before all canvas shortcut/gesture owners. A closing/Apply pass remains owned.
    pub(super) fn model_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        self.model_frame_owned = self.model_choice.is_some();
        let Some(mut choice) = self.model_choice.take() else {
            return false;
        };
        let t = *self.theme.tokens();
        let same_selection = self.selection.edges().is_empty() && self.selection.nodes().iter().copied().eq(choice.nodes.iter().copied());
        choice.stale |= !Arc::ptr_eq(&choice.session, &self.session.view_identity())
            || choice.canvas != self.canvas
            || choice.revision != self.session.revision()
            || !same_selection
            || !self.session.generation_catalog_stamp().is_ok_and(|stamp| stamp == choice.stamp);
        let dirty = self.model_targets_dirty(&choice.nodes);
        let mut close = false;
        let mut refresh = false;
        let mut back = false;
        let mut apply = false;
        if choice.stale {
            egui::Window::new("Model preview changed").order(egui::Order::Foreground).collapsible(false).show(ui.ctx(), |ui| {
                ui.label("The document, selection or model catalog changed. Nothing was applied. Refresh and review the new proposal.");
                if let Some(error) = &choice.error {
                    ui.label(error);
                }
                if ui.button("Refresh models").clicked() {
                    refresh = true;
                }
                if ui.button("Cancel model change").clicked() {
                    close = true;
                }
            });
            if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                close = true;
            }
        } else {
            match &mut choice.step {
                Step::Picking(picker) => match picker.show(ui.ctx(), choice.snapshot.run_catalog().catalog(), &t) {
                    Outcome::Open => {}
                    Outcome::Closed => close = true,
                    Outcome::CatalogChanged => choice.stale = true,
                    Outcome::Unavailable(message) => choice.error = Some(message.into()),
                    Outcome::Chosen(model) => {
                        match self.session.dispatch("node.model_preview", &json!({"canvas":choice.canvas,"nodes":choice.nodes,"model":model})) {
                            Ok(value) => match serde_json::from_value::<Preview>(value) {
                                Ok(preview) => {
                                    choice.step = Step::Review(preview);
                                    choice.error = None;
                                }
                                Err(_) => choice.error = Some("The complete model preview could not be read. Nothing was changed.".into()),
                            },
                            Err(error) => choice.error = Some(error.message),
                        }
                    }
                },
                Step::Review(preview) => {
                    egui::Window::new("Review model change").order(egui::Order::Foreground).collapsible(false).default_width(t.dialog_width).show(
                        ui.ctx(),
                        |ui| {
                            ui.label(format!("Change {} node(s) to {}", preview.targets.len(), preview.model));
                            ui.label("This changes model settings only. Starting a paid run still requires its own quote and consent.");
                            if dirty {
                                ui.label(
                                    "Unsaved card edits are preserved. Cancel this picker, then finish or cancel those edits before applying a model change.",
                                );
                            }
                            if let Some(error) = &choice.error {
                                ui.label(error);
                            }
                            // Reserve the action row inside the window's actual resized content
                            // bounds; a fixed report maximum can otherwise push Apply below its clip.
                            let footer = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
                            let report_height = (ui.available_height() - footer).max(0.0).min(t.comments_height);
                            egui::ScrollArea::vertical().max_height(report_height).show(ui, |ui| {
                                for target in &preview.targets {
                                    ui.label(format!(
                                        "{}: {} to {}",
                                        target.id,
                                        target.previous_model.as_deref().unwrap_or("Unavailable saved model"),
                                        preview.model
                                    ));
                                    for dropped in &target.dropped {
                                        ui.label(format!("Drop {}: {}", dropped.param, reason_label(&dropped.reason)));
                                    }
                                    for name in &target.reset {
                                        ui.label(format!("Reset {name}: hidden or disabled under the destination's rules"));
                                    }
                                    for name in &target.defaults {
                                        ui.label(format!("Use destination default: {name}"));
                                    }
                                }
                            });
                            ui.horizontal(|ui| {
                                if ui.button("Back to models").clicked() {
                                    back = true;
                                }
                                if ui.button("Cancel model change").clicked() {
                                    close = true;
                                }
                                if ui.add_enabled(!dirty, egui::Button::new("Apply model change")).clicked() {
                                    apply = true;
                                }
                            });
                        },
                    );
                    if ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                        close = true;
                    }
                }
            }
            // Picker-stage failures remain visible without rebuilding catalog data every frame.
            if matches!(choice.step, Step::Picking(_))
                && let Some(error) = &choice.error
            {
                egui::Window::new("Model choice unavailable").order(egui::Order::Foreground).collapsible(false).show(ui.ctx(), |ui| {
                    ui.label(error);
                });
            }
        }
        if apply
            && !dirty
            && let Step::Review(preview) = &choice.step
        {
            let request = json!({"canvas":choice.canvas,"nodes":choice.nodes,"model":preview.model,"expected_revision":preview.revision,"preview_binding":preview.binding});
            match self.session.dispatch("node.set_model", &request) {
                Ok(_) => {
                    self.notice = Some(
                        if preview.targets.iter().any(|target| target.changed) {
                            "Model settings changed. Review the next run's quote before starting it."
                        } else {
                            "Model settings already match this choice."
                        }
                        .into(),
                    );
                    close = true;
                }
                Err(error) => choice.error = Some(error.message),
            }
        }
        if refresh || back {
            if let Err(error) = self.open_model_picker(choice.canvas, choice.nodes.clone(), self.session.revision()) {
                choice.error = Some(error.message);
                self.model_choice = Some(choice);
            }
        } else if !close {
            self.model_choice = Some(choice);
        }
        true
    }
}
fn reason_label(code: &str) -> &str {
    match code {
        "not_in_source" => "not declared by the saved source contract",
        "not_in_target" => "not supported by the destination",
        "control_changed" => "control type changed",
        "unit_changed" => "units differ",
        "meaning_changed" => "parameter meaning differs",
        "unverified_meaning" => "shared meaning is not verified between models",
        "invalid_in_target" => "outside destination types, options or range",
        "source_unavailable" => "saved source model is unavailable; no value compatibility can be inferred",
        _ => "not compatible",
    }
}

#[cfg(test)]
mod tests;
