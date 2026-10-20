//! Batch Generate sheets (#163): a sheet canvas opens as a table, one row per Generate card, and
//! the "Sheets" panel switches between the project's canvases and makes new sheets. Every edit
//! dispatches a `sheet.*` or node command; running rows quotes them first like Generate does.
use super::*;
use serde_json::Value as Json;

/// The sheet view's own state: which panel is open, the new sheet's title, and unsaved prompts.
#[derive(Default)]
pub(super) struct SheetView {
    /// The canvas and sheets panel is open.
    pub(super) open: bool,
    /// The title typed for a new sheet.
    title: String,
    /// Prompts being edited, by row, committed when the field loses focus.
    drafts: BTreeMap<NodeId, String>,
    /// The last `sheet.rows` result, with the canvas and revision it describes.
    rows: Option<(CanvasId, u64, Rows)>,
}

/// One `sheet.rows` result, read into what the table shows.
#[derive(Clone, Debug, Default)]
struct Rows {
    rows: Vec<Row>,
    error: Option<String>,
}

#[derive(Clone, Debug)]
struct Row {
    id: NodeId,
    model: String,
    prompt: String,
    references: Vec<(NodeId, bool)>,
    result: String,
    stale: bool,
}

impl Rows {
    fn read(value: &Json) -> Self {
        let rows = value.get("rows").and_then(Json::as_array).into_iter().flatten().filter_map(Row::read).collect();
        Self { rows, error: None }
    }
}

impl Row {
    fn read(value: &Json) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Json::as_str).unwrap_or_default().to_owned();
        let references = value
            .get("references")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|wire| {
                let node = serde_json::from_value(wire.get("node")?.clone()).ok()?;
                Some((node, wire.get("shared").and_then(Json::as_bool).unwrap_or(false)))
            })
            .collect();
        Some(Self {
            id: serde_json::from_value(value.get("id")?.clone()).ok()?,
            model: text("model"),
            prompt: text("prompt"),
            references,
            result: text("result"),
            stale: value.get("stale").and_then(Json::as_bool).unwrap_or(false),
        })
    }
}

/// What a click asked for, applied after the frame is drawn.
enum Action {
    Open(CanvasId),
    Create(String),
    Add,
    Duplicate(Vec<NodeId>),
    Remove(Vec<NodeId>),
    Move(NodeId, usize),
    Prompt(CanvasId, NodeId, String),
    Run(Vec<NodeId>),
}

impl NodeCraftApp {
    /// Whether the open canvas is a sheet, which draws as a table.
    pub(super) fn on_sheet(&self) -> bool {
        self.session.document().canvases().get(&self.canvas).is_some_and(|canvas| canvas.kind() == nodecraft_graph::CanvasKind::Sheet)
    }

    /// Opens `canvas`, leaving the old one's selection, editors and drafts behind.
    pub(super) fn open_canvas(&mut self, canvas: CanvasId) {
        if canvas == self.canvas {
            return;
        }
        self.canvas = canvas;
        // Like a menu, the panel closes once it has done its job.
        self.sheet.open = false;
        self.selection.clear();
        self.editors.clear();
        self.sheet.drafts.clear();
        self.centred = false;
    }

    /// A frame on a sheet: the table and the floating controls, without the node graph's input.
    pub(super) fn sheet_frame(&mut self, ui: &mut egui::Ui, viewport: Rect, blocked: bool) {
        let t = self.theme.tokens();
        self.sheet_table(ui, viewport, t, blocked);
        self.show_notices(ui, viewport, t);
        self.text_focused = canvas::text_input_focused(ui.ctx());
        self.last_text_focus = self.text_focused.then(|| ui.ctx().memory(|m| m.focused())).flatten();
        self.keys_button(ui, viewport);
        #[cfg(not(target_arch = "wasm32"))]
        self.file_button(ui, t);
        let changes = self.keys.show(ui.ctx(), &mut self.session, &self.key_accounts, t);
        self.save_accounts(changes);
        #[cfg(not(target_arch = "wasm32"))]
        self.queue_panel(ui.ctx(), t);
        self.sheets_panel(ui.ctx(), t);
    }

    /// The toggle in the top-right row: "Sheets", which lists the canvases too.
    pub(super) fn sheets_button(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        if ui
            .add(egui::Button::new(egui::RichText::new("Sheets").color(t.text_muted)).frame(false))
            .on_hover_text("Switch canvas, or make a batch Generate sheet")
            .clicked()
        {
            self.sheet.open = !self.sheet.open;
        }
    }

    /// The panel listing the project's canvases and sheets, with a field to make a new sheet.
    pub(super) fn sheets_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        if !self.sheet.open {
            return;
        }
        let canvases: Vec<(CanvasId, String, bool)> = self
            .session
            .document()
            .canvases()
            .values()
            .map(|canvas| {
                let sheet = canvas.kind() == nodecraft_graph::CanvasKind::Sheet;
                let title = if canvas.title().trim().is_empty() { if sheet { "Untitled sheet" } else { "Canvas" }.to_owned() } else { line(canvas.title()) };
                (canvas.id(), title, sheet)
            })
            .collect();
        let mut open = true;
        let mut action = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Sheets")
            .id(egui::Id::new("nodecraft.sheets"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap])
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(t.minimap_height * 2.0).show(ui, |ui| {
                    for (id, title, sheet) in &canvases {
                        let label = if *sheet { format!("{title} (sheet)") } else { title.clone() };
                        if ui.selectable_label(*id == self.canvas, label).clicked() {
                            action = Some(Action::Open(*id));
                        }
                    }
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let name = ui.label("Title");
                    let field = ui
                        .add(egui::TextEdit::singleline(&mut self.sheet.title).hint_text("New sheet").desired_width(t.search_width * 0.6))
                        .labelled_by(name.id);
                    let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let can = !self.sheet.title.trim().is_empty();
                    if (ui.add_enabled(can, egui::Button::new("New sheet")).clicked() || entered) && can {
                        action = Some(Action::Create(self.sheet.title.trim().to_owned()));
                    }
                });
            });
        self.sheet.open = open;
        if let Some(action) = action {
            self.sheet_action(action);
        }
    }

    /// The open sheet as a table, drawn over the whole viewport in place of the node graph.
    pub(super) fn sheet_table(&mut self, ui: &mut egui::Ui, viewport: Rect, t: &Tokens, blocked: bool) {
        ui.painter().rect_filled(viewport, 0.0, t.canvas_bg);
        let rows = self.sheet_rows();
        let title = self.session.document().canvases().get(&self.canvas).map(|canvas| line(canvas.title())).unwrap_or_default();
        let top = t.card_padding * 4.0 + t.control_row_height * 2.0;
        let area = Rect::from_min_max(viewport.min + Vec2::new(t.card_padding * 2.0, top), viewport.max - Vec2::splat(t.card_padding * 2.0));
        let mut actions = Vec::new();
        let selected: Vec<NodeId> = rows.rows.iter().map(|row| row.id).filter(|id| self.selection.contains(*id)).collect();
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            ui.add_enabled_ui(!blocked, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(egui::RichText::new(title).color(t.text));
                    ui.label(egui::RichText::new(format!("{} rows", rows.rows.len())).color(t.text_muted));
                });
                ui.add_space(t.control_gap);
                ui.horizontal(|ui| {
                    if ui.button("Add row").on_hover_text("A new row after the last, with its model and shared references").clicked() {
                        actions.push(Action::Add);
                    }
                    let some = !selected.is_empty();
                    if ui.add_enabled(some, egui::Button::new("Duplicate")).clicked() {
                        actions.push(Action::Duplicate(selected.clone()));
                    }
                    if ui.add_enabled(some, egui::Button::new("Remove")).clicked() {
                        actions.push(Action::Remove(selected.clone()));
                    }
                    let run = if some { format!("Run {} selected", selected.len()) } else { "Run selected".into() };
                    if ui.add_enabled(some, egui::Button::new(run)).on_hover_text("Quote the selected rows, then run them").clicked() {
                        actions.push(Action::Run(selected.clone()));
                    }
                    if ui.add_enabled(!rows.rows.is_empty(), egui::Button::new("Run all")).on_hover_text("Quote every row, then run them").clicked() {
                        actions.push(Action::Run(rows.rows.iter().map(|row| row.id).collect()));
                    }
                });
                if let Some(error) = &rows.error {
                    ui.colored_label(t.danger, error);
                }
                ui.add_space(t.control_gap);
                if rows.rows.is_empty() {
                    ui.label(egui::RichText::new("No rows yet. Add one, then type its prompt.").color(t.text_muted));
                    return;
                }
                // The prompt takes what the other columns leave, but never less than a card.
                let prompt_width = (ui.available_width() - t.card_width * 2.5).max(t.card_width);
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("sheet_rows").num_columns(8).spacing([t.control_gap * 2.0, t.control_gap]).striped(true).show(ui, |ui| {
                        for label in ["", "#", "Prompt", "Model", "References", "Result", "Status", ""] {
                            ui.label(egui::RichText::new(label).color(t.text_muted));
                        }
                        ui.end_row();
                        let last = rows.rows.len().saturating_sub(1);
                        for (index, row) in rows.rows.iter().enumerate() {
                            self.sheet_row(ui, t, prompt_width, index, last, row, &mut actions);
                            ui.end_row();
                        }
                    });
                });
            });
        });
        if !blocked {
            // In order, so a prompt committed by clicking Run is in that run; one that fails
            // stops the rest.
            for action in actions {
                if !self.sheet_action(action) {
                    break;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sheet_row(&mut self, ui: &mut egui::Ui, t: &Tokens, prompt_width: f32, index: usize, last: usize, row: &Row, actions: &mut Vec<Action>) {
        let mut chosen = self.selection.contains(row.id);
        if ui.checkbox(&mut chosen, "").on_hover_text("Select this row").changed() {
            self.selection.toggle(row.id);
        }
        ui.label(egui::RichText::new(format!("{}", index.saturating_add(1))).color(t.text_muted));
        let mut text = self.sheet.drafts.get(&row.id).cloned().unwrap_or_else(|| row.prompt.clone());
        let field = ui.add_sized(
            [prompt_width, t.control_row_height],
            egui::TextEdit::multiline(&mut text).id(egui::Id::new(("sheet_prompt", row.id))).desired_rows(1).hint_text("Prompt"),
        );
        if field.has_focus() {
            self.sheet.drafts.insert(row.id, text);
        } else {
            // Committed as the field loses focus, before any button clicked to leave it.
            self.sheet.drafts.remove(&row.id);
            if field.lost_focus() && text != row.prompt {
                actions.push(Action::Prompt(self.canvas, row.id, text));
            }
        }
        ui.label(if row.model.is_empty() { "No model".to_owned() } else { line(&row.model) });
        ui.label(self.references_label(&row.references)).on_hover_text("Shared references are wired into every row");
        let (result, color) = match row.result.as_str() {
            "complete" if row.stale => ("Out of date", t.stale),
            "complete" => ("Done", t.text),
            "partial" => ("Partial", t.stale),
            _ => ("Not run", t.text_muted),
        };
        ui.label(egui::RichText::new(result).color(color));
        ui.label(self.row_status(row.id, t));
        ui.horizontal(|ui| {
            if ui.add_enabled(index > 0, egui::Button::new("Up")).on_hover_text("Move this row up").clicked() {
                actions.push(Action::Move(row.id, index.saturating_sub(1)));
            }
            if ui.add_enabled(index < last, egui::Button::new("Down")).on_hover_text("Move this row down").clicked() {
                actions.push(Action::Move(row.id, index.saturating_add(1)));
            }
            if ui.button("Run").on_hover_text("Quote this row, then run it").clicked() {
                actions.push(Action::Run(vec![row.id]));
            }
        });
    }

    fn references_label(&self, references: &[(NodeId, bool)]) -> String {
        if references.is_empty() {
            return "None".into();
        }
        let canvas = self.session.document().canvases().get(&self.canvas);
        let names: Vec<String> = references
            .iter()
            .map(|(id, shared)| {
                let title = canvas.and_then(|canvas| canvas.nodes().get(id)).map_or_else(
                    || "Deleted".to_owned(),
                    |node| card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor())),
                );
                if *shared { format!("{} (shared)", line(&title)) } else { line(&title) }
            })
            .collect();
        names.join(", ")
    }

    /// The row's live run status, or the reason its last run failed.
    fn row_status(&self, id: NodeId, t: &Tokens) -> egui::RichText {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(runs) = &self.runs {
            if let Some(status) = runs.card_status(self.canvas, id) {
                return egui::RichText::new(status).color(t.accent);
            }
            if let Some(reason) = runs.failure(self.canvas, id) {
                return egui::RichText::new(line(reason)).color(t.danger);
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = id;
        egui::RichText::new("").color(t.text_muted)
    }

    /// The open sheet's rows, read again only when the document has changed.
    fn sheet_rows(&mut self) -> Rows {
        let revision = self.session.revision();
        if let Some((canvas, seen, rows)) = &self.sheet.rows
            && *canvas == self.canvas
            && *seen == revision
        {
            return rows.clone();
        }
        let rows = match self.session.dispatch("sheet.rows", &json!({"canvas": self.canvas})) {
            Ok(value) => Rows::read(&value),
            Err(error) => Rows { rows: Vec::new(), error: Some(error.message) },
        };
        self.sheet.rows = Some((self.canvas, revision, rows.clone()));
        rows
    }

    /// Applies `action`; false, with the error shown, if it failed.
    fn sheet_action(&mut self, action: Action) -> bool {
        let canvas = self.canvas;
        let result = match action {
            Action::Open(id) => {
                self.open_canvas(id);
                Ok(())
            }
            Action::Create(title) => self.create_sheet(title),
            Action::Add => self.sheet_seed().and_then(|seed| self.dispatch_sheet("sheet.rows_add", json!({"canvas": canvas, "seed": seed, "prompts": [""]}))),
            Action::Duplicate(rows) => {
                self.sheet_seed().and_then(|seed| self.dispatch_sheet("sheet.rows_duplicate", json!({"canvas": canvas, "seed": seed, "rows": rows})))
            }
            Action::Remove(rows) => self.dispatch_sheet("sheet.rows_remove", json!({"canvas": canvas, "rows": rows})).map(|()| self.selection.clear()),
            Action::Move(row, index) => self.dispatch_sheet("sheet.row_move", json!({"canvas": canvas, "row": row, "index": index})),
            Action::Prompt(on, row, text) => {
                self.dispatch_sheet("node.param_set", json!({"canvas": on, "id": row, "name": "prompt", "value": Value::Text(text)}))
            }
            Action::Run(rows) => self.request_runs(rows, super::generate::RunKind::Generate),
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(error.message);
                false
            }
        }
    }

    fn dispatch_sheet(&mut self, id: &str, params: Json) -> Result<(), Error> {
        self.session.dispatch(id, &params)?;
        self.notice = None;
        Ok(())
    }

    /// A fresh seed for the ids a multi-node sheet command makes.
    fn sheet_seed(&self) -> Result<NodeId, Error> {
        ids::random_id(|_| false).map(NodeId::from_bytes)
    }

    fn create_sheet(&mut self, title: String) -> Result<(), Error> {
        let document = self.session.document();
        let id = ids::random_id(|bytes| document.canvases().contains_key(&CanvasId::from_bytes(*bytes))).map(CanvasId::from_bytes)?;
        self.dispatch_sheet("sheet.create", json!({"canvas": id, "title": title}))?;
        self.sheet.title.clear();
        self.open_canvas(id);
        Ok(())
    }
}

/// `text` on one line, cut to a readable length.
fn line(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let mut chars = first.chars();
    let mut out: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() || text.lines().nth(1).is_some() {
        out.push('…');
    }
    out
}
