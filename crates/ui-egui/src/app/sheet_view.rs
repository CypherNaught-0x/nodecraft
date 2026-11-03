//! Batch Generate sheets (#163): a sheet canvas opens as a table, one row per Generate card, and
//! the "Sheets" panel switches between the project's canvases and makes new sheets. Every edit
//! dispatches a `sheet.*` or node command; running rows quotes them first like Generate does.
use super::*;
use serde_json::Value as Json;

/// The most characters of a title, model or reason shown on one line.
const LINE_CHARS: usize = 80;

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
    rows: Option<(CanvasId, u64, Arc<Rows>)>,
    /// The open canvas, when it was last seen in the document.
    present: Option<CanvasId>,
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
    /// The references' titles, read once per document revision.
    references: String,
    result: String,
    stale: bool,
}

impl Rows {
    fn read(value: &Json, label: impl Fn(&[(NodeId, bool)]) -> String) -> Self {
        let rows = value.get("rows").and_then(Json::as_array).into_iter().flatten().filter_map(|row| Row::read(row, &label)).collect();
        Self { rows, error: None }
    }
}

impl Row {
    fn read(value: &Json, label: impl Fn(&[(NodeId, bool)]) -> String) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Json::as_str).unwrap_or_default().to_owned();
        let references: Vec<(NodeId, bool)> = value
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
            references: label(&references),
            result: text("result"),
            stale: value.get("stale").and_then(Json::as_bool).unwrap_or(false),
        })
    }
}

/// The table's column widths: the checkbox and number, the prompt, and the rest.
#[derive(Clone, Copy)]
struct Columns {
    index: f32,
    prompt: f32,
    other: f32,
}

/// The canvas a project opens on: its first plain canvas, or its first sheet if it has no other.
/// A published technique is never opened as a working canvas.
pub(super) fn home_canvas(project: &Project) -> Option<CanvasId> {
    let canvases = project.canvases();
    let first = |kind| canvases.values().find(|canvas| canvas.kind() == kind);
    first(nodecraft_graph::CanvasKind::Canvas).or_else(|| first(nodecraft_graph::CanvasKind::Sheet)).map(|canvas| canvas.id())
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

    /// Opens `canvas`, leaving the old one's selection, editors, drafts and menus behind.
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
        self.palette = None;
        self.turn_into = None;
        self.centred = false;
    }

    /// Goes back to the project's home canvas when the open one is removed, as by undoing the
    /// `sheet.create` that made it.
    pub(super) fn keep_canvas(&mut self) {
        let document = self.session.document();
        if document.canvases().contains_key(&self.canvas) {
            self.sheet.present = Some(self.canvas);
        } else if self.sheet.present == Some(self.canvas)
            && let Some(home) = home_canvas(document)
        {
            self.open_canvas(home);
        }
    }

    /// Whether row `id` has a prompt typed but not yet committed.
    pub(super) fn sheet_draft_pending(&self, id: NodeId) -> bool {
        self.sheet.drafts.get(&id).is_some_and(|draft| self.committed_prompt(id) != Some(draft.as_str()))
    }

    /// Whether any row has a prompt typed but not yet committed.
    pub(super) fn sheet_drafts_pending(&self) -> bool {
        self.sheet.drafts.keys().any(|id| self.sheet_draft_pending(*id))
    }

    fn committed_prompt(&self, id: NodeId) -> Option<&str> {
        let node = self.session.document().canvases().get(&self.canvas)?.nodes().get(&id)?;
        match node.params.get("prompt") {
            Some(Value::Text(text)) => Some(text),
            _ => Some(""),
        }
    }

    /// Commits every typed prompt, so saving or opening keeps it; false, with the error shown, if
    /// one could not be.
    fn commit_sheet_drafts(&mut self) -> bool {
        let drafts = std::mem::take(&mut self.sheet.drafts);
        let canvas = self.canvas;
        let changed: Vec<(NodeId, String)> =
            drafts.into_iter().filter(|(id, draft)| self.committed_prompt(*id).is_some_and(|prompt| prompt != draft)).collect();
        changed.into_iter().all(|(id, text)| self.sheet_action(Action::Prompt(canvas, id, text)))
    }

    /// A frame on a sheet: the table and the floating controls, without the node graph's input.
    pub(super) fn sheet_frame(&mut self, ui: &mut egui::Ui, viewport: Rect, blocked: bool) {
        let t = self.theme.tokens();
        if !blocked {
            self.sheet_keys(ui);
        }
        self.keep_canvas();
        if !self.on_sheet() {
            // An undo took the sheet away; the next frame draws the canvas it went back to.
            ui.ctx().request_repaint();
            return;
        }
        // More must admit and preserve the editor before a row's lost_focus commit runs.
        // Capture its owner before constructing even the disabled chrome widget.
        let editor = (self.text_focused || canvas::text_input_focused(ui.ctx())).then(|| ui.ctx().memory(|m| m.focused()).or(self.last_text_focus)).flatten();
        let chrome = self.topbar(ui);
        if self.topbar.search.is_none()
            && chrome
            && !egui::Popup::is_any_open(ui.ctx())
            && ui.input(|i| i.pointer.any_pressed() || i.pointer.any_released())
            && let Some(owner) = editor
            && ui.ctx().memory(|m| m.focused()).is_none_or(|id| id == owner)
        {
            ui.ctx().memory_mut(|m| m.request_focus(owner));
            ui.ctx().options_mut(|o| o.input_options.surrender_focus_on = egui::SurrenderFocusOn::Never);
        }
        self.sheet_table(ui, viewport, t, blocked);
        self.show_notices(ui, viewport, t);
        self.text_focused = canvas::text_input_focused(ui.ctx());
        self.last_text_focus = self.text_focused.then(|| ui.ctx().memory(|m| m.focused())).flatten();
        #[cfg(not(target_arch = "wasm32"))]
        self.file_button(ui, t);
        self.publish_floating_bounds(ui.ctx());
        let changes = self.keys.show(ui.ctx(), &mut self.session, &self.key_accounts, t);
        self.save_accounts(changes);
        self.queue_panel(ui.ctx(), t);
        #[cfg(not(target_arch = "wasm32"))]
        self.ownership_panel(ui.ctx(), t);
        self.sheets_panel(ui.ctx(), t);
        self.techniques_panel(ui.ctx(), t);
        self.elements_panel(ui.ctx(), t);
        self.budgets_panel(ui.ctx(), t);
        self.plugin_panel.show(ui.ctx(), &mut self.session, t);
        self.browser_providers_panel(ui.ctx(), t);
        #[cfg(not(target_arch = "wasm32"))]
        self.comfy_server_panel(ui.ctx(), t);
        self.share_panel(ui.ctx(), t);
    }

    /// The document keys a sheet keeps: save and open, and undo and redo outside a text field.
    fn sheet_keys(&mut self, ui: &mut egui::Ui) {
        let (save_as, save, open) =
            ui.input_mut(|i| (consume_shortcut(i, "Command+Shift+S"), consume_shortcut(i, "Command+S"), consume_shortcut(i, "Command+O")));
        if (save || save_as || open) && self.commit_sheet_drafts() {
            self.notice = self.file_shortcut(ui.ctx(), save, save_as);
        }
        if canvas::text_input_focused(ui.ctx()) {
            return;
        }
        let command = ui.input_mut(|input| {
            self.session.registry().specs().filter(|spec| matches!(spec.id, "edit.undo" | "edit.redo")).find_map(|spec| {
                spec.shortcut.into_iter().chain(spec.alternate_shortcuts.iter().copied()).any(|shortcut| consume_shortcut(input, shortcut)).then_some(spec.id)
            })
        });
        if let Some(command) = command {
            self.notice = self.session.dispatch(command, &json!({})).err().map(|error| error.message);
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
            .filter(|canvas| canvas.kind() != nodecraft_graph::CanvasKind::Technique)
            .map(|canvas| {
                let sheet = canvas.kind() == nodecraft_graph::CanvasKind::Sheet;
                let title = if canvas.title().trim().is_empty() { if sheet { "Untitled sheet" } else { "Canvas" }.to_owned() } else { line(canvas.title()) };
                (canvas.id(), title, sheet)
            })
            .collect();
        let mut open = true;
        let mut action = None;
        crate::floating::panel(ctx, "Sheets", egui::Id::new("nodecraft.sheets"), crate::floating::Home::RightTop, t).open(&mut open).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(t.sheet_list_height).show(ui, |ui| {
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
                let field =
                    ui.add(egui::TextEdit::singleline(&mut self.sheet.title).hint_text("New sheet").desired_width(t.sheet_title_width)).labelled_by(name.id);
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

    /// The open sheet as a table, drawn over the whole viewport in place of the node graph. Only
    /// the rows in view are laid out, so a full sheet costs what a screenful does.
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
                let gap = t.control_gap * 2.0;
                ui.spacing_mut().item_spacing = Vec2::new(gap, t.control_gap);
                // The prompt takes what the other columns leave, but never less than a card.
                let fixed = t.sheet_index_width * 2.0 + t.sheet_column_width * 5.0 + gap * 7.0;
                let prompt_width = (ui.available_width() - fixed).max(t.card_width);
                let widths = Columns { index: t.sheet_index_width, prompt: prompt_width, other: t.sheet_column_width };
                ui.horizontal(|ui| {
                    for (label, width) in [
                        ("", widths.index),
                        ("#", widths.index),
                        ("Prompt", widths.prompt),
                        ("Model", widths.other),
                        ("References", widths.other),
                        ("Result", widths.other),
                        ("Status", widths.other),
                    ] {
                        ui.add_sized([width, t.control_row_height * 0.5], egui::Label::new(egui::RichText::new(label).color(t.text_muted)));
                    }
                });
                let last = rows.rows.len().saturating_sub(1);
                egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, t.control_row_height, rows.rows.len(), |ui, shown| {
                    for index in shown {
                        if let Some(row) = rows.rows.get(index) {
                            self.sheet_row(ui, t, widths, index, last, row, &mut actions);
                        }
                    }
                });
            });
        });
        // In order, so a prompt committed by clicking Run is in that run; one that fails stops the
        // rest. A prompt is committed even while a dialog holds the input: the dialog opening is
        // what took its field's focus, and dropping it would lose what was typed.
        for action in actions {
            if blocked && !matches!(action, Action::Prompt(..)) {
                continue;
            }
            if !self.sheet_action(action) {
                break;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sheet_row(&mut self, ui: &mut egui::Ui, t: &Tokens, widths: Columns, index: usize, last: usize, row: &Row, actions: &mut Vec<Action>) {
        let height = t.control_row_height;
        let band = Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), height));
        if index % 2 == 1 {
            ui.painter().rect_filled(band, 0.0, ui.visuals().faint_bg_color);
        }
        ui.horizontal(|ui| {
            ui.set_height(height);
            let mut chosen = self.selection.contains(row.id);
            if ui.add_sized([widths.index, height], egui::Checkbox::without_text(&mut chosen)).on_hover_text("Select this row").changed() {
                self.selection.toggle(row.id);
            }
            ui.add_sized([widths.index, height], egui::Label::new(egui::RichText::new(format!("{}", index.saturating_add(1))).color(t.text_muted)));
            let mut text = self.sheet.drafts.get(&row.id).cloned().unwrap_or_else(|| row.prompt.clone());
            let field = ui.add_sized(
                [widths.prompt, height],
                egui::TextEdit::multiline(&mut text).id(egui::Id::new(("sheet_prompt", row.id))).desired_rows(1).hint_text("Prompt"),
            );
            if field.has_focus() {
                self.sheet.drafts.insert(row.id, text);
            } else {
                // Committed as the field loses focus, before any button clicked to leave it.
                // Escape leaves the field too, so it commits rather than cancels.
                self.sheet.drafts.remove(&row.id);
                if field.lost_focus() && text != row.prompt {
                    actions.push(Action::Prompt(self.canvas, row.id, text));
                }
            }
            let cell = |ui: &mut egui::Ui, text: egui::RichText| ui.add_sized([widths.other, height], egui::Label::new(text).truncate());
            cell(ui, egui::RichText::new(if row.model.is_empty() { "No model".to_owned() } else { line(&row.model) }));
            cell(ui, egui::RichText::new(&row.references)).on_hover_text("Shared references are wired into every row");
            let (result, color) = match row.result.as_str() {
                "complete" if row.stale => ("Out of date", t.stale),
                "complete" => ("Done", t.text),
                "partial" => ("Partial", t.stale),
                _ => ("Not run", t.text_muted),
            };
            cell(ui, egui::RichText::new(result).color(color));
            cell(ui, self.row_status(row.id, t));
            ui.allocate_ui_with_layout(Vec2::new(widths.other, height), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = t.control_gap;
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
        });
    }

    /// The titles of a row's references, marking those wired into every row.
    fn references_label(&self, canvas: CanvasId, references: &[(NodeId, bool)]) -> String {
        if references.is_empty() {
            return "None".into();
        }
        let canvas = self.session.document().canvases().get(&canvas);
        let names: Vec<String> = references
            .iter()
            .map(|(id, shared)| {
                let title = canvas
                    .and_then(|canvas| canvas.nodes().get(id))
                    .map_or_else(|| "Deleted".to_owned(), |node| card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor())));
                if *shared { format!("{} (shared)", line(&title)) } else { line(&title) }
            })
            .collect();
        names.join(", ")
    }

    /// The row's live run status, or the reason its last run failed.
    fn row_status(&self, id: NodeId, t: &Tokens) -> egui::RichText {
        if let Some(runs) = &self.runs {
            if let Some(status) = runs.card_status(self.canvas, id) {
                return egui::RichText::new(status).color(t.accent);
            }
            if let Some(reason) = runs.failure(self.canvas, id) {
                return egui::RichText::new(line(reason)).color(t.danger);
            }
        }
        egui::RichText::new("").color(t.text_muted)
    }

    /// The open sheet's rows, read again only when the document has changed.
    fn sheet_rows(&mut self) -> Arc<Rows> {
        let revision = self.session.revision();
        if let Some((canvas, seen, rows)) = &self.sheet.rows
            && *canvas == self.canvas
            && *seen == revision
        {
            return Arc::clone(rows);
        }
        let canvas = self.canvas;
        let rows = Arc::new(match self.session.dispatch("sheet.rows", &json!({"canvas": canvas})) {
            Ok(value) => Rows::read(&value, |references| self.references_label(canvas, references)),
            Err(error) => Rows { rows: Vec::new(), error: Some(error.message) },
        });
        self.sheet.rows = Some((canvas, revision, Arc::clone(&rows)));
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
pub(super) fn line(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let mut chars = first.chars();
    let mut out: String = chars.by_ref().take(LINE_CHARS).collect();
    if chars.next().is_some() || text.lines().nth(1).is_some() {
        out.push('…');
    }
    out
}
