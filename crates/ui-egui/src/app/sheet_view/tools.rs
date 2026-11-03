//! The sheet's second toolbar (#163): selecting every row, the model and settings of the selected
//! rows (or every row), references, retrying failed rows and cancelling running ones. Each
//! dispatches the same command a card or the canvas would.
use super::*;

/// A node on the sheet that can be wired into rows: anything but a row, with one output.
struct Reference {
    id: NodeId,
    title: String,
    /// Wired into every one of the targets, and into any of them.
    all: bool,
    any: bool,
}

impl NodeCraftApp {
    pub(super) fn sheet_tools(&mut self, ui: &mut egui::Ui, rows: &Rows, selected: &[NodeId], actions: &mut Vec<Action>) {
        let ids: Vec<NodeId> = rows.rows.iter().map(|row| row.id).collect();
        let some = !selected.is_empty();
        // What the model, settings and reference controls act on: the selection, else every row.
        let which = match selected.len() {
            0 => "every row".to_owned(),
            1 => "the selected row".to_owned(),
            n => format!("the {n} selected rows"),
        };
        let targets = if some { selected.to_vec() } else { ids.clone() };
        ui.horizontal(|ui| {
            let all = !ids.is_empty() && selected.len() == ids.len();
            if ui.add_enabled(!ids.is_empty(), egui::Button::new(if all { "Select none" } else { "Select all" })).clicked() {
                actions.push(Action::Select(if all { Vec::new() } else { ids.clone() }));
            }
            if ui
                .add_enabled(!targets.is_empty(), egui::Button::new("Model…"))
                .on_hover_text(format!("Choose the model of {which}; settings the new model also has are kept, and the picker lists any it drops"))
                .clicked()
            {
                actions.push(Action::Model(targets.clone()));
            }
            let settings = ui
                .add_enabled(some, egui::Button::selectable(self.sheet.settings && some, "Settings"))
                .on_hover_text("Edit the selected rows' settings together; a setting they don't share says Mixed")
                .on_disabled_hover_text("Select rows to edit their settings");
            if settings.clicked() {
                self.sheet.settings = !self.sheet.settings;
            }
            ui.menu_button("References", |ui| {
                let found = self.sheet_references(&targets);
                if found.is_empty() {
                    ui.label("The sheet has no sources to wire in. Drop images onto the sheet to add them.");
                }
                // Without a selection, wiring reaches every row: a shared reference.
                let rows = some.then(|| targets.clone());
                for reference in found {
                    if !reference.all && ui.button(format!("Wire \"{}\" into {which}", reference.title)).clicked() {
                        actions.push(Action::Wire(reference.id, rows.clone()));
                        ui.close();
                    }
                    if reference.any && ui.button(format!("Unwire \"{}\" from {which}", reference.title)).clicked() {
                        actions.push(Action::Unwire(reference.id, rows.clone()));
                        ui.close();
                    }
                }
            });
            ui.separator();
            let (retry, running) = self.retry_and_running(rows);
            let label = if retry.is_empty() { "Retry failed".to_owned() } else { format!("Retry {} failed", retry.len()) };
            if ui
                .add_enabled(!retry.is_empty(), egui::Button::new(label))
                .on_hover_text("Quote and run again only the rows whose last run failed or left a partial result")
                .clicked()
            {
                actions.push(Action::Retry(retry));
            }
            let label = if running.is_empty() { "Cancel".to_owned() } else { format!("Cancel {}", running.len()) };
            if ui.add_enabled(!running.is_empty(), egui::Button::new(label)).on_hover_text("Stop the rows that are queued or running").clicked() {
                actions.push(Action::Cancel(running));
            }
        });
    }

    /// The rows "Retry failed" runs (`sheet_cmds::batch::retry_failed`) and the rows queued or
    /// running, top to bottom.
    fn retry_and_running(&self, rows: &Rows) -> (Vec<NodeId>, Vec<NodeId>) {
        let Some(runs) = &self.runs else { return (Vec::new(), Vec::new()) };
        // One pass over the jobs, not one per row: a big batch has as many jobs as rows.
        let live: BTreeSet<NodeId> = runs.jobs().iter().filter(|job| job.canvas == self.canvas && !job.state.is_final()).map(|job| job.node).collect();
        let running: Vec<NodeId> = rows.rows.iter().filter(|row| live.contains(&row.id)).map(|row| row.id).collect();
        let failed: BTreeSet<NodeId> = rows.rows.iter().filter(|row| runs.failure(self.canvas, row.id).is_some()).map(|row| row.id).collect();
        // Only look through the sheet when there is something to retry.
        if failed.is_empty() && !rows.rows.iter().any(|row| row.result == "partial") {
            return (Vec::new(), running);
        }
        let retry = self
            .session
            .document()
            .canvases()
            .get(&self.canvas)
            .map(|sheet| nodecraft_engine::sheet_cmds::batch::retry_failed(sheet, &failed, &running.iter().copied().collect()))
            .unwrap_or_default();
        (retry, running)
    }

    /// The sheet's nodes that can be references, with whether each is wired into `targets`.
    fn sheet_references(&self, targets: &[NodeId]) -> Vec<Reference> {
        let Some(sheet) = self.session.document().canvases().get(&self.canvas) else { return Vec::new() };
        let targets: BTreeSet<NodeId> = targets.iter().copied().collect();
        let mut found: Vec<&Node> =
            sheet.nodes().values().filter(|node| !nodecraft_nodes::generation::is_generation(&node.kind) && node.outputs.len() == 1).collect();
        found.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y).then(a.id.cmp(&b.id)));
        found
            .into_iter()
            .map(|node| {
                let reached: BTreeSet<NodeId> =
                    sheet.edges().values().filter(|edge| edge.from.node == node.id && targets.contains(&edge.to.node)).map(|edge| edge.to.node).collect();
                Reference {
                    id: node.id,
                    title: line(&card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor()))),
                    all: !targets.is_empty() && reached.len() == targets.len(),
                    any: !reached.is_empty(),
                }
            })
            .collect()
    }

    /// Asks every queued or running job of `rows` on the open sheet to stop.
    pub(super) fn cancel_rows(&mut self, rows: &[NodeId]) -> Result<(), Error> {
        let canvas = self.canvas;
        let Some(runs) = self.runs.as_mut() else { return Err(Error::new(ErrorKind::Unsupported, "nothing is running")) };
        let rows: BTreeSet<NodeId> = rows.iter().copied().collect();
        let keys: Vec<_> = runs
            .jobs()
            .iter()
            .filter(|job| job.canvas == canvas && rows.contains(&job.node) && !job.state.is_final() && !job.cancel_requested)
            .map(|job| job.key)
            .collect();
        if keys.is_empty() {
            return Err(Error::new(ErrorKind::NotFound, "those rows have nothing queued or running"));
        }
        let mut first = None;
        for key in keys {
            // A job that ended meanwhile has nothing to cancel; the rest still are.
            if let Err(error) = runs.cancel(key) {
                first.get_or_insert(error);
            }
        }
        match first {
            Some(error) if runs.jobs().iter().all(|job| !(job.canvas == canvas && rows.contains(&job.node) && job.cancel_requested)) => Err(error),
            _ => Ok(()),
        }
    }

    /// The row settings panel for the selected rows, while it is open: the canvas's bulk
    /// parameter panel (#473), generated from the rows' parameter contracts, with Mixed for a
    /// setting they don't share. One row is edited with `node.param_set`, several with
    /// `node.params_set_bulk`; Change model opens the model picker.
    pub(super) fn sheet_settings(&mut self, ui: &mut egui::Ui, enabled: bool, t: &Tokens) {
        let ids: BTreeSet<NodeId> = self.selection.nodes().clone();
        if !self.sheet.settings || ids.is_empty() {
            return;
        }
        let place = crate::bulk::Place::SHEET;
        let (_, request) = self.bulk.show_as(ui, &self.session, self.canvas, &ids, enabled, &place, t);
        let Some(request) = request.filter(|_| enabled) else { return };
        let result = match request {
            crate::bulk::Intent::Parameters(params) => match params["ids"].as_array().map(Vec::as_slice) {
                Some([id]) => self
                    .session
                    .dispatch("node.param_set", &json!({"canvas": params["canvas"], "id": id, "name": params["name"], "value": params["value"]}))
                    .map(|_| ()),
                _ => self.session.dispatch("node.params_set_bulk", &params).map(|_| ()),
            },
            crate::bulk::Intent::OpenModel { canvas, exact_ids, revision } => self.open_model_picker(canvas, exact_ids, revision),
        };
        match result {
            Ok(()) => self.notice = None,
            Err(error) => {
                self.bulk.failed(error.message.clone());
                self.notice = Some(error.message);
            }
        }
    }
}

/// `node`'s aspect-ratio setting, found by meaning in its parameter contract (#162): what the
/// card's toolbar offers as Aspect. `None` when the model has none. Rows of one kind and model
/// share it ([`Aspect::of`] reads each row's value).
pub(super) fn aspect_of(session: &Session, node: &Node) -> Option<Aspect> {
    let contract = session.kinds().get(&node.kind).ok()?.instance_contract(node).ok()?;
    let name = crate::card::generate::Roles::of(&contract).aspect?;
    let spec = contract.parameter_groups().flat_map(|group| group.schema.params.iter()).find(|spec| spec.name == name)?;
    let nodecraft_types::ParamControl::Choice { options } = &spec.control else { return None };
    let default = match spec.default.as_ref() {
        Some(Value::Text(value)) => value.clone(),
        _ => String::new(),
    };
    let found = Aspect { name, options: options.iter().map(|option| (option.value.clone(), option.label.clone())).collect(), current: String::new(), default };
    Some(found.of(node))
}
