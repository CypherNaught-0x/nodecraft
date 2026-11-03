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
    /// Why wiring it in would leave a row no mode to run in, if it would.
    misfit: Option<String>,
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
                    if !reference.all {
                        let label = format!("Wire \"{}\" into {which}", reference.title);
                        let wire = match &reference.misfit {
                            None => ui.button(label),
                            Some(why) => ui
                                .add_enabled(false, egui::Button::new(format!("{label} (the model can't take it)")))
                                .on_disabled_hover_text(format!("No mode of the row's model would take it: {why}")),
                        };
                        if wire.clicked() {
                            actions.push(Action::Wire(reference.id, rows.clone()));
                            ui.close();
                        }
                    }
                    if reference.any && ui.button(format!("Unwire \"{}\" from {which}", reference.title)).clicked() {
                        actions.push(Action::Unwire(reference.id, rows.clone()));
                        ui.close();
                    }
                }
            });
            ui.separator();
            let (retry, running, left) = self.retry_and_running(rows);
            let label = if retry.is_empty() { "Retry failed".to_owned() } else { format!("Retry {} failed", retry.len()) };
            let mut hint = "Quote and run again only the rows whose last run failed or left a partial result".to_owned();
            if left > 0 {
                hint.push_str(&format!(
                    ". {left} failed {} left out: a held result to place or discard in the queue, or a run that may already have been charged; run {} by hand if you mean to pay again",
                    if left == 1 { "row is" } else { "rows are" },
                    if left == 1 { "it" } else { "them" }
                ));
            }
            if ui.add_enabled(!retry.is_empty(), egui::Button::new(label)).on_hover_text(hint.as_str()).on_disabled_hover_text(hint.as_str()).clicked()
            {
                actions.push(Action::Retry(retry));
            }
            let label = if running.is_empty() { "Cancel".to_owned() } else { format!("Cancel {}", running.len()) };
            if ui.add_enabled(!running.is_empty(), egui::Button::new(label)).on_hover_text("Stop the rows that are queued or running").clicked() {
                actions.push(Action::Cancel(running));
            }
        });
    }

    /// The rows "Retry failed" runs (`sheet_cmds::batch::retry_failed`), the rows queued or
    /// running, and how many failed rows it leaves for the person: a held paid result, or a job
    /// that may already have been charged.
    fn retry_and_running(&self, rows: &Rows) -> (Vec<NodeId>, Vec<NodeId>, usize) {
        let Some(runs) = &self.runs else { return (Vec::new(), Vec::new(), 0) };
        // One pass over the jobs and held results, not one per row.
        let holds = nodecraft_engine::sheet_cmds::batch::RetryHolds::of(self.canvas, runs.jobs(), runs.held());
        let running: Vec<NodeId> = rows.rows.iter().filter(|row| holds.running.contains(&row.id)).map(|row| row.id).collect();
        let failed: BTreeSet<NodeId> = rows.rows.iter().filter(|row| runs.failure(self.canvas, row.id).is_some()).map(|row| row.id).collect();
        let left = failed.iter().filter(|row| !holds.running.contains(row) && (holds.held.contains(row) || holds.maybe_charged.contains(row))).count();
        // Only look through the sheet when there is something to retry.
        if failed.is_empty() && !rows.rows.iter().any(|row| row.result == "partial") {
            return (Vec::new(), running, left);
        }
        let retry = self
            .session
            .document()
            .canvases()
            .get(&self.canvas)
            .map(|sheet| nodecraft_engine::sheet_cmds::batch::retry_failed(sheet, &failed, &holds))
            .unwrap_or_default();
        (retry, running, left)
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
                // Checked on the first target it isn't wired into yet: rows mostly share a model.
                let next = targets.iter().find(|row| !reached.contains(row)).and_then(|row| sheet.nodes().get(row));
                Reference {
                    id: node.id,
                    title: line(&card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor()))),
                    all: !targets.is_empty() && reached.len() == targets.len(),
                    any: !reached.is_empty(),
                    misfit: next.and_then(|row| self.reference_misfit(sheet, node, row)),
                }
            })
            .collect()
    }

    /// Why wiring `reference` into `row` would leave the row's model no mode to run in: its
    /// input roles and counts, checked as a quote checks them (#162), on the input the wire would
    /// take (the row's first input of the reference's modality).
    fn reference_misfit(&self, sheet: &Canvas, reference: &Node, row: &Node) -> Option<String> {
        let [output] = reference.outputs.as_slice() else { return None };
        let Some(port) = row.inputs.iter().find(|port| port.port_type.modality == output.port_type.modality) else {
            return Some(format!("it has no {:?} input", output.port_type.modality));
        };
        let contract = row_contract(&self.session, row)?;
        let port = sheet.nodes().get(&row.id)?.inputs.iter().find(|input| input.name == port.name)?.name.as_str();
        crate::app::generate::misfit(&self.session, sheet, &contract, row, Some(port))
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

#[cfg(test)]
thread_local! {
    /// How many row contracts the sheet has read, so tests can check it reads one per model.
    pub(in crate::app) static CONTRACTS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A row's parameter contract (#162), which rows of one kind and model share.
pub(super) fn row_contract(session: &Session, node: &Node) -> Option<Arc<nodecraft_nodes::NodeContract>> {
    #[cfg(test)]
    CONTRACTS_READ.with(|read| read.set(read.get().saturating_add(1)));
    session.kinds().get(&node.kind).ok()?.instance_contract(node).ok().map(Arc::new)
}

/// The aspect-ratio setting in `contract`, found by meaning (#162): what the card's toolbar offers
/// as Aspect. `None` when the model has none. [`Aspect::of`] reads a row's own value.
pub(super) fn aspect_in(contract: &nodecraft_nodes::NodeContract) -> Option<Aspect> {
    let name = crate::card::generate::Roles::of(contract).aspect?;
    let spec = contract.parameter_groups().flat_map(|group| group.schema.params.iter()).find(|spec| spec.name == name)?;
    let nodecraft_types::ParamControl::Choice { options } = &spec.control else { return None };
    let default = match spec.default.as_ref() {
        Some(Value::Text(value)) => value.clone(),
        _ => String::new(),
    };
    Some(Aspect { name, options: options.iter().map(|option| (option.value.clone(), option.label.clone())).collect(), current: default.clone(), default })
}

/// Why no mode of the row's model takes what is wired into it, if none does: references are
/// checked against the model's input roles and counts as they would be when it is quoted.
pub(super) fn wiring_problem(session: &Session, sheet: &Canvas, contract: &nodecraft_nodes::NodeContract, node: &Node) -> Option<String> {
    crate::app::generate::misfit(session, sheet, contract, node, None)
}
