//! One model-choice controller for card and bulk intents. The engine owns migration/commit.
use super::*;
use crate::model_picker::{ModelPicker, Outcome, ServerEntry, ServerGroup, ServerSection, ServerWorkflow};
use nodecraft_engine::{
    comfy_models,
    generation_catalog::{GenerationCatalog, GenerationCatalogStamp, GenerationWiring},
    model_swap::Preview,
};
use serde::Deserialize;

enum Step {
    Picking(Box<ModelPicker>),
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
    /// The ComfyUI server entry under review, applied with `comfy.models.use` (#970).
    server_model: Option<(String, String)>,
    /// Which read of the default ComfyUI server the picker's server section shows.
    server_read: ServerRead,
}
/// The last check's id and the installed node list's size: a refresh or a new default changes it.
type ServerRead = (Option<u64>, Option<usize>);
/// What `comfy.models.list` answers, as the picker reads it.
#[derive(Deserialize)]
struct Listed {
    read: bool,
    server: Option<String>,
    groups: Vec<comfy_models::Group>,
    workflows: Vec<comfy_models::Workflow>,
    current: Vec<Current>,
}
#[derive(Deserialize)]
struct Current {
    checkpoint: String,
    present: Option<bool>,
}

#[cfg(test)]
impl ModelChoice {
    /// The cards the picker was opened for, sorted.
    pub(super) fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }
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
        // Only Generate image cards run a server checkpoint or take an image workflow's place.
        let listed = exact_ids
            .first()
            .and_then(|id| self.session.document().canvases().get(&canvas)?.nodes().get(id))
            .is_some_and(|node| node.kind == nodecraft_nodes::generation::KIND)
            .then(|| self.list_server(canvas, &exact_ids));
        let live = self.session.document().canvases().get(&canvas).ok_or_else(|| invalid("model canvas is unavailable"))?;
        // Captures own port/count slots only, borrowing payload-free Port metadata. Admit the
        // entire temporary capture before allocating it; it is gone before the first paint.
        let mut capture_bytes = exact_ids.len().checked_mul(std::mem::size_of::<GenerationWiring<'_>>()).ok_or_else(allocation)?;
        let mut kind = None;
        for id in &exact_ids {
            let node = live.nodes().get(id).ok_or_else(|| invalid("selected model node is unavailable"))?;
            if !nodecraft_nodes::generation::is_generation(&node.kind) {
                return Err(Error::new(ErrorKind::Unsupported, "this selection contains a node without a model-swap contract"));
            }
            if *kind.get_or_insert(node.kind.as_str()) != node.kind {
                return Err(Error::new(ErrorKind::Unsupported, "choose a model for image and text cards separately"));
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
            captures.push(snapshot.capture_wiring(live, self.session.kinds(), *id)?);
        }
        let kind = kind.ok_or_else(|| invalid("selected model node is unavailable"))?;
        let mut picker = ModelPicker::for_targets(snapshot.run_catalog().catalog(), |model| {
            for wiring in &captures {
                snapshot.validate_destination_wiring(kind, wiring, &model.id, true).map_err(|e| e.message)?;
            }
            Ok(())
        })?;
        if let Some((listed, can_refresh)) = listed {
            picker = picker.with_server(server_section(listed, can_refresh, kind, &snapshot, &captures), snapshot.run_catalog().catalog());
        }
        drop(captures);
        self.selection.replace(exact_ids.iter().copied().collect());
        self.model_choice = Some(ModelChoice {
            stamp: snapshot.stamp(),
            snapshot,
            session: self.session.view_identity(),
            canvas,
            nodes: exact_ids,
            revision: expected_revision,
            step: Step::Picking(Box::new(picker)),
            stale: false,
            error: None,
            server_model: None,
            server_read: self.server_read(),
        });
        self.model_frame_owned = true;
        Ok(())
    }

    fn server_read(&self) -> ServerRead {
        let (last, nodes) = nodecraft_engine::comfy_workflow_cmds::last(&self.session);
        (last.map(|report| report.id), nodes)
    }

    /// What the ComfyUI server lists and the imported workflows (#970), read from the engine as
    /// data, and whether this host can read the server again.
    /// A list that can't be read is reported in the section's heading row, not hidden.
    fn list_server(&mut self, canvas: CanvasId, ids: &[NodeId]) -> (Result<Listed, String>, bool) {
        let listed = self
            .session
            .dispatch("comfy.models.list", &json!({"canvas": canvas, "nodes": ids}))
            .map_err(|error| error.message)
            .and_then(|value| serde_json::from_value(value).map_err(|_| "The ComfyUI server's model list could not be read.".to_owned()));
        (listed, self.session.enabled("comfy.nodes.refresh", &json!({})).is_ok())
    }

    /// A copy of the imported workflow card `id` beside the first target (#970).
    fn place_workflow(&mut self, choice: &ModelChoice, from: (CanvasId, NodeId), t: &crate::theme::Tokens) -> Result<(), Error> {
        let canvas = self.session.document().canvases().get(&choice.canvas).ok_or_else(|| invalid("model canvas is unavailable"))?;
        let beside = choice.nodes.first().and_then(|id| canvas.nodes().get(id)).map(|node| node.pos).unwrap_or_default();
        let pos = nodecraft_graph::Position { x: beside.x + t.card_width + t.layout_gap, y: beside.y };
        let id = crate::ids::node_id(self.session.document(), ("comfy.models.add", from.1, self.session.revision()))
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "no free card id was found; try again"))?;
        let request = json!({"canvas": choice.canvas, "id": id, "pos": pos, "workflow": {"canvas": from.0, "id": from.1}});
        self.session.dispatch("comfy.models.add", &request).map(|_| ())
    }

    fn model_targets_dirty(&self, ids: &[NodeId]) -> bool {
        self.bulk.targets_dirty(self.canvas, ids) || ids.iter().any(|id| self.editors.get(id).is_some_and(EditorState::has_uncommitted))
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
        let mut refresh = matches!(choice.step, Step::Picking(_)) && !choice.stale && choice.server_read != self.server_read();
        let query = match &choice.step {
            Step::Picking(picker) => picker.query().to_owned(),
            Step::Review(_) => String::new(),
        };
        let mut back = false;
        let mut apply = false;
        if choice.stale {
            crate::floating::on_screen(egui::Window::new("Model preview changed"), ui.ctx(), None, &t)
                .id(egui::Id::new("nodecraft.model_stale"))
                .order(egui::Order::Foreground)
                .collapsible(false)
                .show(ui.ctx(), |ui| {
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
                    Outcome::ServerModel { group, name } => {
                        let request = json!({"canvas":choice.canvas,"nodes":choice.nodes,"group":group,"name":name});
                        match self.session.dispatch("comfy.models.preview", &request) {
                            Ok(mut value) => {
                                if let Some(object) = value.as_object_mut() {
                                    object.remove("server_model");
                                }
                                match serde_json::from_value::<Preview>(value) {
                                    Ok(preview) => {
                                        choice.step = Step::Review(preview);
                                        choice.server_model = Some((group, name));
                                        choice.error = None;
                                    }
                                    Err(_) => choice.error = Some("The complete model preview could not be read. Nothing was changed.".into()),
                                }
                            }
                            Err(error) => choice.error = Some(error.message),
                        }
                    }
                    Outcome::Workflow { canvas, id } => match self.place_workflow(&choice, (canvas, id), &t) {
                        Ok(()) => {
                            self.notice = Some("Placed a copy of the imported workflow beside the card. The card itself is unchanged.".into());
                            close = true;
                        }
                        Err(error) => choice.error = Some(error.message),
                    },
                    Outcome::RefreshServer => match self.session.dispatch("comfy.nodes.refresh", &json!({})) {
                        Ok(_) => self.notice = Some("Reading the ComfyUI server's models…".into()),
                        Err(error) => choice.error = Some(error.message),
                    },
                },
                Step::Review(preview) => {
                    crate::floating::on_screen(egui::Window::new("Review model change"), ui.ctx(), Some(t.dialog_width), &t).id(egui::Id::new("nodecraft.model_review")).order(egui::Order::Foreground).collapsible(false).show(
                        ui.ctx(),
                        |ui| {
                            ui.label(format!("Change {} node(s) to {}", preview.targets.len(), preview.model));
                            if let Some((_, name)) = &choice.server_model {
                                ui.label(format!("Checkpoint from the ComfyUI server: {name}"));
                            }
                            ui.label("This changes model settings only. Starting a paid run still requires its own quote and consent.");
                            if dirty {
                                ui.label(
                                    "Unsaved parameter edits are preserved. Cancel this picker, then finish or cancel those edits before applying a model change.",
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
                crate::floating::on_screen(egui::Window::new("Model choice unavailable"), ui.ctx(), None, &t)
                    .id(egui::Id::new("nodecraft.model_unavailable"))
                    .order(egui::Order::Foreground)
                    .collapsible(false)
                    .show(ui.ctx(), |ui| {
                        ui.label(error);
                    });
            }
        }
        if apply
            && !dirty
            && let Step::Review(preview) = &choice.step
        {
            let (command, request) = match &choice.server_model {
                Some((group, name)) => (
                    "comfy.models.use",
                    json!({"canvas":choice.canvas,"nodes":choice.nodes,"group":group,"name":name,"expected_revision":preview.revision,"preview_binding":preview.binding}),
                ),
                None => (
                    "node.set_model",
                    json!({"canvas":choice.canvas,"nodes":choice.nodes,"model":preview.model,"expected_revision":preview.revision,"preview_binding":preview.binding}),
                ),
            };
            match self.session.dispatch(command, &request) {
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
        if (refresh || back) && !close {
            // The rebuilt picker first paints on the next frame.
            ui.ctx().request_repaint();
            match self.open_model_picker(choice.canvas, choice.nodes.clone(), self.session.revision()) {
                Ok(()) => {
                    if let Some(ModelChoice { step: Step::Picking(picker), .. }) = &mut self.model_choice
                        && !back
                    {
                        let _ = picker.set_query(&query);
                    }
                }
                Err(error) => {
                    choice.error = Some(error.message);
                    self.model_choice = Some(choice);
                }
            }
        } else if !close {
            self.model_choice = Some(choice);
        }
        true
    }
}
/// The picker's server section from what `comfy.models.list` answered, with each mapped model's
/// fit for these cards checked once, here (#970). Only image workflows are offered to image cards.
fn server_section(
    listed: Result<Listed, String>,
    can_refresh: bool,
    kind: &str,
    snapshot: &GenerationCatalog,
    captures: &[GenerationWiring<'_>],
) -> ServerSection {
    let listed = match listed {
        Ok(listed) => listed,
        Err(error) => {
            return ServerSection {
                server: None,
                read: false,
                can_refresh,
                error: Some(error),
                missing: Vec::new(),
                groups: Vec::new(),
                workflows: Vec::new(),
            };
        }
    };
    let catalog = snapshot.run_catalog();
    let mut fits: std::collections::BTreeMap<String, Option<String>> = std::collections::BTreeMap::new();
    let mut groups = Vec::new();
    for group in listed.groups {
        let mut entries = Vec::new();
        for entry in group.models {
            let unavailable = match (entry.unavailable, &group.unavailable, &entry.model) {
                (Some(reason), _, _) => Some(reason),
                (None, Some(reason), _) => Some(reason.clone()),
                (None, None, None) => Some("No built-in workflow runs it.".into()),
                (None, None, Some(model)) => fits
                    .entry(model.clone())
                    .or_insert_with(|| {
                        captures.iter().try_for_each(|w| snapshot.validate_destination_wiring(kind, w, model, true).map_err(|e| e.message)).err()
                    })
                    .clone(),
            };
            let detail = match (entry.note, entry.model.as_deref().and_then(|model| catalog.catalog().get(model))) {
                (Some(note), _) => note,
                (None, Some(model)) => format!("Runs through the built-in {} workflow", model.label),
                (None, None) => String::new(),
            };
            entries.push(ServerEntry { group: group.id.clone(), name: entry.name, detail, unavailable });
        }
        groups.push(ServerGroup { label: group.label, entries, more: group.more });
    }
    let workflows = listed
        .workflows
        .into_iter()
        .filter(|w| w.modalities.iter().any(|m| m == "image"))
        .map(|w| ServerWorkflow { canvas: w.canvas, id: w.id, title: w.title, unavailable: w.unavailable })
        .collect();
    let mut missing: Vec<String> = listed.current.into_iter().filter(|c| c.present == Some(false)).map(|c| c.checkpoint).collect();
    missing.sort();
    missing.dedup();
    ServerSection { server: listed.server, read: listed.read, can_refresh, error: None, missing, groups, workflows }
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
mod comfy970_tests;
#[cfg(test)]
mod tests;
