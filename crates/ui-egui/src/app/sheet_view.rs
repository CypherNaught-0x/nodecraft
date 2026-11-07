//! Batch Generate sheets (#163): a sheet canvas opens as a table, one row per Generate card, and
//! the "Sheets" panel switches between the project's canvases and makes new sheets. Every edit
//! dispatches a `sheet.*` or node command; running rows quotes them first like Generate does.
use super::*;
use nodecraft_types::GenerationId;
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
    /// Only rows whose prompt holds this text are shown (ignoring case).
    pub(super) filter: String,
    /// Only rows in this state are shown.
    pub(super) status: StatusFilter,
    /// The template being expanded into rows, while its panel is open.
    pub(super) template: Option<TemplateDraft>,
    /// Whether the rows show as the table or as a grid of their results.
    pub(super) mode: ViewMode,
    /// The plain canvas open before the sheet: where results are sent unless one is picked.
    pub(super) last_plain: Option<CanvasId>,
    /// The plain canvas picked to send results to.
    pub(super) target: Option<CanvasId>,
    /// The open sheet's image sources an image variable can list, with the canvas and revision
    /// they were read at.
    sources: Option<(CanvasId, u64, Sources)>,
    /// The row settings panel is open for the selected rows.
    pub(super) settings: bool,
    /// The sheet's new name while it is being renamed.
    pub(super) renaming: Option<String>,
}

/// Image sources on a sheet with their titles, top to bottom.
type Sources = Arc<Vec<(NodeId, String)>>;

/// Most rows one export takes: the workflow-result save's limit on nodes (#966).
pub(super) const MAX_EXPORT_ROWS: usize = 256;

/// What the Export button offers: its label, the rows it saves, and why it can't, if it can't.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ExportChoice {
    pub(super) label: String,
    pub(super) rows: Vec<NodeId>,
    pub(super) problem: Option<String>,
}

/// The selected rows with a result when there are any, else every row with one (`done`, top to
/// bottom), refused past [`MAX_EXPORT_ROWS`].
pub(super) fn export_choice(chosen: Vec<NodeId>, done: Vec<NodeId>) -> ExportChoice {
    let (label, rows) = if chosen.is_empty() { ("Export all".to_owned(), done) } else { (format!("Export {}", chosen.len()), chosen) };
    let problem = if rows.is_empty() {
        Some("No row has a result to export yet".to_owned())
    } else if rows.len() > MAX_EXPORT_ROWS {
        Some(format!("One export saves at most {MAX_EXPORT_ROWS} rows; select fewer (the filter's Select shown helps)"))
    } else {
        None
    };
    ExportChoice { label, rows, problem }
}

/// How the sheet shows its rows: the table of cells, or a grid of their results to review.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ViewMode {
    #[default]
    Table,
    Grid,
}

/// Which rows the table shows by their state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum StatusFilter {
    #[default]
    All,
    NotRun,
    Running,
    Done,
    OutOfDate,
    Partial,
    Failed,
    /// A paid result is held for a decision (#369): place or discard it in the queue panel.
    Held,
    Cancelled,
}

impl StatusFilter {
    const ALL: [Self; 9] = [Self::All, Self::NotRun, Self::Running, Self::Done, Self::OutOfDate, Self::Partial, Self::Failed, Self::Held, Self::Cancelled];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All statuses",
            Self::NotRun => "Not run",
            Self::Running => "Running",
            Self::Done => "Done",
            Self::OutOfDate => "Out of date",
            Self::Partial => "Partial",
            Self::Failed => "Failed",
            Self::Held => "Held",
            Self::Cancelled => "Cancelled",
        }
    }
}

/// A prompt template with its variables, expanded by `sheet.expand`.
pub(super) struct TemplateDraft {
    pub(super) text: String,
    pub(super) variables: Vec<VariableDraft>,
    pub(super) copies: u32,
    /// The seed the expansion's ids derive from: the preview and the expansion use the same one.
    seed: NodeId,
    /// The last dry run: the params it was for, and its result or error.
    preview: Option<(u64, String, Result<Json, String>)>,
}

impl TemplateDraft {
    fn new(text: String, seed: NodeId) -> Self {
        Self { text, variables: vec![VariableDraft::default()], copies: 1, seed, preview: None }
    }

    /// The `sheet.expand` params this draft stands for. Blank lines are not values; a variable
    /// with no name and no values is left out.
    fn params(&self, canvas: CanvasId) -> Json {
        let variables: Vec<Json> = self
            .variables
            .iter()
            .filter(|variable| !variable.is_blank())
            .map(|variable| match &variable.images {
                Some(images) => json!({"name": variable.name.trim(), "images": images}),
                None => {
                    let mut values: Vec<&str> = variable.values.lines().map(str::trim).filter(|value| !value.is_empty()).collect();
                    if variable.empty {
                        values.push("");
                    }
                    json!({"name": variable.name.trim(), "text": values})
                }
            })
            .collect();
        json!({"canvas": canvas, "seed": self.seed, "template": self.text, "variables": variables, "copies": self.copies})
    }
}

/// One variable of a template being written.
#[derive(Default)]
pub(super) struct VariableDraft {
    pub(super) name: String,
    /// A text variable's values, one per line.
    pub(super) values: String,
    /// A text variable also has the empty value, which inserts nothing.
    pub(super) empty: bool,
    /// An image variable's image sources on the sheet, in the sheet's order; `None` for text.
    pub(super) images: Option<Vec<NodeId>>,
}

impl VariableDraft {
    /// Nothing typed or chosen: left out of the expansion.
    fn is_blank(&self) -> bool {
        self.name.trim().is_empty() && self.values.trim().is_empty() && !self.empty && self.images.as_ref().is_none_or(Vec::is_empty)
    }
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
    /// The prompt lowercased for the filter, once per document revision rather than per frame.
    folded: String,
    /// The references' titles, read once per document revision.
    references: String,
    result: String,
    stale: bool,
    /// The active result, how many results the row's history holds and the active one's place
    /// in it (1 is the oldest).
    generation: Option<GenerationId>,
    results: usize,
    position: Option<usize>,
    /// What the grid shows of the active result: its first image, or the start of its text.
    preview: Preview,
    /// The row's aspect-ratio setting, when its model's parameter contract has one (#162).
    aspect: Option<Aspect>,
    /// Something is wired into the row.
    wired: bool,
    /// Why no mode of the row's model takes what is wired into it.
    problem: Option<String>,
}

/// A row's aspect-ratio parameter, found by its meaning in the model's contract rather than by
/// name, with its choices and the value it has now.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Aspect {
    pub(super) name: String,
    pub(super) options: Vec<(String, String)>,
    pub(super) current: String,
    /// The schema default, shown while the row sets none.
    pub(super) default: String,
}

impl Aspect {
    /// This model's aspect setting as `node` has it.
    fn of(&self, node: &Node) -> Self {
        let current = match node.params.get(&self.name) {
            Some(Value::Text(value)) => value.clone(),
            _ => self.default.clone(),
        };
        Self { current, ..self.clone() }
    }
}

/// What a grid tile shows of a row's active result.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum Preview {
    #[default]
    Nothing,
    Image(nodecraft_types::AssetRef),
    Text(String),
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
            folded: text("prompt").to_lowercase(),
            references: label(&references),
            result: text("result"),
            stale: value.get("stale").and_then(Json::as_bool).unwrap_or(false),
            generation: value.get("generation").and_then(|id| serde_json::from_value(id.clone()).ok()),
            results: value.get("results").and_then(Json::as_u64).and_then(|n| usize::try_from(n).ok()).unwrap_or(0),
            position: value.get("position").and_then(Json::as_u64).and_then(|n| usize::try_from(n).ok()),
            preview: Preview::Nothing,
            aspect: None,
            wired: !references.is_empty(),
            problem: None,
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
    /// Opens the template panel, or closes it.
    Template,
    Expand(Json),
    Select(Vec<NodeId>),
    /// Selects the row, or unselects it.
    Toggle(NodeId),
    /// Places these rows' results on the home canvas (`sheet.place`).
    Place(Vec<NodeId>),
    /// Asks for a folder and saves these rows' results into it.
    Export(Vec<NodeId>),
    /// Makes the row's newer (true) or older result active.
    Step(NodeId, bool),
    /// Opens the model picker for these rows.
    Model(Vec<NodeId>),
    /// Sets one row's parameter.
    Param(NodeId, String, Value),
    /// Wires the reference into these rows, or every row (a shared reference).
    Wire(NodeId, Option<Vec<NodeId>>),
    /// Unwires the reference from these rows, or every row.
    Unwire(NodeId, Option<Vec<NodeId>>),
    /// Runs again the rows whose last run failed or left a partial result.
    Retry(Vec<NodeId>),
    /// Cancels these rows' queued and running jobs.
    Cancel(Vec<NodeId>),
    /// Renames the sheet.
    Rename(String),
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
        if self.session.document().canvases().get(&self.canvas).is_some_and(|open| open.kind() == nodecraft_graph::CanvasKind::Canvas) {
            self.sheet.last_plain = Some(self.canvas);
        }
        self.sheet.target = None;
        self.canvas = canvas;
        // Like a menu, the panel closes once it has done its job.
        self.sheet.open = false;
        self.selection.clear();
        self.editors.clear();
        self.sheet.drafts.clear();
        self.sheet.template = None;
        self.sheet.filter.clear();
        self.sheet.status = StatusFilter::All;
        self.sheet.mode = ViewMode::Table;
        self.sheet.settings = false;
        self.sheet.renaming = None;
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
        self.sheet_settings(ui, !blocked, t);
        self.bulk.finish_frame(ui.ctx());
        // Files dropped onto a sheet become sources on it, to wire into rows as references.
        self.poll_drop(ui, t);
        if !blocked {
            self.drop_files(ui, viewport, t);
        }
        self.show_notices(ui, viewport, t);
        self.text_focused = canvas::text_input_focused(ui.ctx());
        self.last_text_focus = self.text_focused.then(|| ui.ctx().memory(|m| m.focused())).flatten();
        #[cfg(not(target_arch = "wasm32"))]
        self.file_button(ui, t);
        self.publish_floating_bounds(ui.ctx());
        self.keys_panel(ui.ctx(), t);
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
                let mut renamed_unchanged = false;
                ui.horizontal(|ui| {
                    match self.sheet.renaming.as_mut() {
                        Some(name) => {
                            let label = ui.label("Name");
                            let field = ui
                                .add(egui::TextEdit::singleline(name).desired_width(t.sheet_title_width).id(egui::Id::new("sheet_rename")))
                                .labelled_by(label.id);
                            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            let can = !name.trim().is_empty();
                            if (ui.add_enabled(can, egui::Button::new("Save name")).clicked() || entered) && can {
                                if name.trim() == title {
                                    // Nothing to rename: just stop editing.
                                    renamed_unchanged = true;
                                } else {
                                    actions.push(Action::Rename(name.trim().to_owned()));
                                }
                            }
                            if ui.button("Cancel rename").clicked() {
                                self.sheet.renaming = None;
                            }
                        }
                        None => {
                            ui.heading(egui::RichText::new(&title).color(t.text));
                            if ui.small_button("Rename").on_hover_text("Name this batch; the Sheets panel lists it by name").clicked() {
                                self.sheet.renaming = Some(title.clone());
                            }
                        }
                    }
                    ui.label(egui::RichText::new(format!("{} rows", rows.rows.len())).color(t.text_muted));
                });
                if renamed_unchanged {
                    self.sheet.renaming = None;
                }
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
                    let open = self.sheet.template.is_some();
                    if ui
                        .selectable_label(open, "From template")
                        .on_hover_text("Write a prompt with {{variables}} and make one row per combination of their values")
                        .clicked()
                    {
                        actions.push(Action::Template);
                    }
                    ui.separator();
                    for (mode, label, hint) in
                        [(ViewMode::Table, "Table", "Edit the rows' prompts and settings"), (ViewMode::Grid, "Grid", "Review the rows' results as thumbnails")]
                    {
                        if ui.selectable_label(self.sheet.mode == mode, label).on_hover_text(hint).clicked() {
                            self.sheet.mode = mode;
                        }
                    }
                    ui.separator();
                    let done: Vec<NodeId> = rows.rows.iter().filter(|row| row.generation.is_some()).map(|row| row.id).collect();
                    let chosen: Vec<NodeId> = done.iter().copied().filter(|id| self.selection.contains(*id)).collect();
                    let place = if chosen.is_empty() { "Send to canvas".to_owned() } else { format!("Send {} to canvas", chosen.len()) };
                    if ui
                        .add_enabled(!chosen.is_empty(), egui::Button::new(place))
                        .on_hover_text("Put the selected rows' results on the canvas as source cards")
                        .clicked()
                    {
                        actions.push(Action::Place(chosen.clone()));
                    }
                    self.target_picker(ui);
                    let export = export_choice(chosen, done);
                    let hint = match &export.problem {
                        Some(problem) => problem.clone(),
                        None if some && export.label == "Export all" => {
                            "None of the selected rows has a result: save every row's result into a folder you choose".to_owned()
                        }
                        None => "Save these rows' results into a folder you choose".to_owned(),
                    };
                    if ui
                        .add_enabled(export.problem.is_none(), egui::Button::new(export.label))
                        .on_disabled_hover_text(hint.as_str())
                        .on_hover_text(hint.as_str())
                        .clicked()
                    {
                        actions.push(Action::Export(export.rows));
                    }
                });
                ui.add_space(t.control_gap);
                self.sheet_tools(ui, &rows, &selected, &mut actions);
                if self.sheet.template.is_some() {
                    ui.add_space(t.control_gap);
                    self.template_panel(ui, t, rows.rows.len(), &mut actions);
                }
                if let Some(error) = &rows.error {
                    ui.colored_label(t.danger, error);
                }
                ui.add_space(t.control_gap);
                if rows.rows.is_empty() {
                    ui.label(egui::RichText::new("No rows yet. Add one, then type its prompt.").color(t.text_muted));
                    return;
                }
                let shown = self.shown_rows(&rows);
                ui.horizontal(|ui| {
                    let name = ui.label("Filter");
                    ui.add(egui::TextEdit::singleline(&mut self.sheet.filter).hint_text("Filter prompts").desired_width(t.sheet_title_width))
                        .labelled_by(name.id);
                    let status = egui::ComboBox::from_id_salt("sheet_status_filter").selected_text(self.sheet.status.label()).show_ui(ui, |ui| {
                        for status in StatusFilter::ALL {
                            ui.selectable_value(&mut self.sheet.status, status, status.label());
                        }
                    });
                    status.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Show rows by status"));
                    if shown.len() != rows.rows.len() {
                        ui.label(egui::RichText::new(format!("Showing {} of {}", shown.len(), rows.rows.len())).color(t.text_muted));
                        let ids: Vec<NodeId> = shown.iter().filter_map(|index| rows.rows.get(*index)).map(|row| row.id).collect();
                        if ui
                            .add_enabled(!ids.is_empty(), egui::Button::new("Select shown"))
                            .on_hover_text("Select exactly the rows the filter shows")
                            .clicked()
                        {
                            actions.push(Action::Select(ids));
                        }
                    }
                });
                ui.add_space(t.control_gap);
                if self.sheet.mode == ViewMode::Grid {
                    self.sheet_grid(ui, t, &rows, &shown, &mut actions);
                    return;
                }
                let gap = t.control_gap * 2.0;
                ui.spacing_mut().item_spacing = Vec2::new(gap, t.control_gap);
                // The prompt takes what the other columns leave, but never less than two of them.
                let fixed = t.sheet_index_width * 2.0 + t.sheet_column_width * 6.0 + gap * 8.0;
                let prompt_width = (ui.available_width() - fixed).max(t.sheet_column_width * 2.0);
                let widths = Columns { index: t.sheet_index_width, prompt: prompt_width, other: t.sheet_column_width };
                ui.horizontal(|ui| {
                    for (label, width) in [
                        ("", widths.index),
                        ("#", widths.index),
                        ("Prompt", widths.prompt),
                        ("Model", widths.other),
                        ("Aspect", widths.other),
                        ("References", widths.other),
                        ("Result", widths.other),
                        ("Status", widths.other),
                    ] {
                        ui.add_sized([width, t.control_row_height * 0.5], egui::Label::new(egui::RichText::new(label).color(t.text_muted)));
                    }
                });
                let last = rows.rows.len().saturating_sub(1);
                if shown.is_empty() {
                    ui.label(egui::RichText::new("No rows match the filter.").color(t.text_muted));
                }
                egui::ScrollArea::both().auto_shrink([false, false]).show_rows(ui, t.control_row_height, shown.len(), |ui, range| {
                    for at in range {
                        if let Some(index) = shown.get(at).copied()
                            && let Some(row) = rows.rows.get(index)
                        {
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
            let pick = ui.add_sized([widths.index, height], egui::Checkbox::without_text(&mut chosen));
            // Named for screen readers: a bare checkbox is announced as nothing (#1118).
            let number = index.saturating_add(1);
            pick.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, chosen, format!("Select row {number}")));
            if pick.on_hover_text("Select this row").changed() {
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
            let model = if row.model.is_empty() { "No model".to_owned() } else { line(&row.model) };
            let picked = ui.add_sized([widths.other, height], egui::Button::new(&model).truncate()).on_hover_text("Change this row's model");
            picked.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Model of row {}: {model}", index.saturating_add(1))));
            if picked.clicked() {
                // A row in the selection changes with the rest of it; any other row alone.
                let rows = if self.selection.contains(row.id) { self.selection.nodes().iter().copied().collect() } else { vec![row.id] };
                actions.push(Action::Model(rows));
            }
            match &row.aspect {
                Some(aspect) => {
                    let mut chosen = aspect.current.clone();
                    let shown = aspect.options.iter().find(|(value, _)| *value == aspect.current).map_or("Default", |(_, label)| label.as_str());
                    let combo = egui::ComboBox::from_id_salt(("sheet_aspect", row.id)).width(widths.other).selected_text(shown).show_ui(ui, |ui| {
                        for (value, label) in &aspect.options {
                            ui.selectable_value(&mut chosen, value.clone(), label);
                        }
                    });
                    let number = index.saturating_add(1);
                    combo.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, format!("Aspect of row {number}")));
                    if chosen != aspect.current {
                        actions.push(Action::Param(row.id, aspect.name.clone(), Value::Text(chosen)));
                    }
                }
                None => {
                    cell(ui, egui::RichText::new("Not offered").color(t.text_muted)).on_hover_text("This row's model has no aspect-ratio setting");
                }
            }
            match &row.problem {
                Some(why) => {
                    cell(ui, egui::RichText::new(format!("No mode fits: {}", row.references)).color(t.danger))
                        .on_hover_text(format!("This row can't run as wired: {why}"));
                }
                None => {
                    cell(ui, egui::RichText::new(&row.references)).on_hover_text("Shared references are wired into every row");
                }
            }
            let (result, color) = match row.result.as_str() {
                "complete" if row.stale => ("Out of date", t.stale),
                "complete" => ("Done", t.text),
                "partial" => ("Partial", t.stale),
                _ => ("Not run", t.text_muted),
            };
            // A row run more than once keeps its older results: which one is active, of how many.
            let result = match row.position {
                Some(position) if row.results > 1 => format!("{result} {position}/{}", row.results),
                _ => result.to_owned(),
            };
            cell(ui, egui::RichText::new(result).color(color)).on_hover_text("Each run adds a result; older ones stay in the row's history");
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

    /// The rows the filter shows that have a result, as a grid of thumbnails with their status.
    /// Clicking a tile selects its row; a row run more than once steps through its results.
    fn sheet_grid(&mut self, ui: &mut egui::Ui, t: &Tokens, rows: &Rows, shown: &[usize], actions: &mut Vec<Action>) {
        let tiles: Vec<(usize, &Row)> =
            shown.iter().filter_map(|index| rows.rows.get(*index).map(|row| (*index, row))).filter(|(_, row)| row.generation.is_some()).collect();
        let waiting = shown.len().saturating_sub(tiles.len());
        if waiting > 0 {
            let rows = if waiting == 1 { "1 row shown has" } else { "rows shown have" };
            let text = if waiting == 1 { format!("{rows} no result yet.") } else { format!("{waiting} {rows} no result yet.") };
            ui.label(egui::RichText::new(text).color(t.text_muted));
        }
        if tiles.is_empty() {
            return;
        }
        let gap = t.control_gap * 2.0;
        let side = t.sheet_thumbnail;
        let columns = (((ui.available_width() + gap) / (side + gap)).floor() as usize).max(1);
        let height = side + t.control_row_height + gap;
        let lines = tiles.len().div_ceil(columns);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, height, lines, |ui, range| {
            for line in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(gap, gap);
                    for (index, row) in tiles.iter().skip(line.saturating_mul(columns)).take(columns) {
                        self.sheet_tile(ui, t, *index, row, actions);
                    }
                });
            }
        });
    }

    fn sheet_tile(&mut self, ui: &mut egui::Ui, t: &Tokens, index: usize, row: &Row, actions: &mut Vec<Action>) {
        let side = t.sheet_thumbnail;
        let number = index.saturating_add(1);
        ui.vertical(|ui| {
            ui.set_width(side);
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
            let chosen = self.selection.contains(row.id);
            response.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, chosen, format!("Row {number}")));
            let radius = f32::from(t.panel_radius) / 2.0;
            ui.painter().rect_filled(rect, radius, t.canvas_bg);
            match &row.preview {
                Preview::Image(asset) => {
                    let edge = side * ui.ctx().pixels_per_point();
                    let assets = self.session.assets();
                    let preview = self.previews.request_asset(ui.ctx(), asset.hash, edge, || assets.preview_read(asset));
                    crate::card::show_image(ui, rect.shrink(t.inset), &preview, None, t);
                }
                Preview::Text(text) => {
                    ui.put(rect.shrink(t.control_gap), egui::Label::new(egui::RichText::new(text).size(t.label_size).color(t.text)).wrap().selectable(false));
                }
                Preview::Nothing => {
                    ui.put(rect.shrink(t.control_gap), egui::Label::new(egui::RichText::new("Result").color(t.text_muted)).selectable(false));
                }
            }
            // The status badge, over the image's top-left corner.
            let (label, color) = state_badge(self.row_state(row), t);
            let text = egui::RichText::new(label).size(t.label_size).color(color);
            let size = ui.painter().layout_no_wrap(label.to_owned(), egui::FontId::proportional(t.label_size), color).size();
            let badge = Rect::from_min_size(rect.min + Vec2::splat(t.control_gap), size + Vec2::splat(t.control_gap));
            ui.painter().rect_filled(badge, radius, t.panel_fill);
            ui.put(badge, egui::Label::new(text).selectable(false));
            let stroke = if chosen { egui::Stroke::new(t.stroke_strong, t.accent) } else { egui::Stroke::new(t.stroke_hairline, t.panel_stroke) };
            ui.painter().rect_stroke(rect, radius, stroke, egui::StrokeKind::Inside);
            let response = response.on_hover_text(line(&row.prompt));
            if response.clicked() {
                actions.push(Action::Toggle(row.id));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = t.control_gap;
                ui.label(egui::RichText::new(format!("{number}")).color(t.text_muted));
                if let Some(position) = row.position
                    && row.results > 1
                {
                    if crate::history::named(ui.add_enabled(position > 1, egui::Button::new("‹").small()), &format!("Previous result, row {number}"))
                        .on_hover_text("Show this row's older result")
                        .clicked()
                    {
                        actions.push(Action::Step(row.id, false));
                    }
                    ui.label(egui::RichText::new(format!("{position} of {}", row.results)).color(t.text_muted));
                    if crate::history::named(ui.add_enabled(position < row.results, egui::Button::new("›").small()), &format!("Next result, row {number}"))
                        .on_hover_text("Show this row's newer result")
                        .clicked()
                    {
                        actions.push(Action::Step(row.id, true));
                    }
                }
            });
        });
    }

    /// The indices of the rows the filter shows, top to bottom.
    fn shown_rows(&self, rows: &Rows) -> Vec<usize> {
        let needle = self.sheet.filter.trim().to_lowercase();
        rows.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| needle.is_empty() || row.folded.contains(&needle))
            .filter(|(_, row)| self.sheet.status == StatusFilter::All || self.row_state(row) == self.sheet.status)
            .map(|(index, _)| index)
            .collect()
    }

    /// The state the status filter sorts `row` by: a live run first, then its last failure, then
    /// its result. A failure is forgotten when the row's next run starts, is recorded, cancelled
    /// or placed (`Runs::tick`), so a later success is never filtered as failed.
    fn row_state(&self, row: &Row) -> StatusFilter {
        if let Some(runs) = &self.runs {
            // Map lookups only: this runs for every row each frame while a filter is on.
            let card = runs.card(self.canvas, row.id);
            if card.live {
                return StatusFilter::Running;
            }
            if card.held {
                return StatusFilter::Held;
            }
            if runs.failure(self.canvas, row.id).is_some() {
                return StatusFilter::Failed;
            }
            if card.cancelled {
                return StatusFilter::Cancelled;
            }
        }
        match row.result.as_str() {
            "complete" if row.stale => StatusFilter::OutOfDate,
            "complete" => StatusFilter::Done,
            "partial" => StatusFilter::Partial,
            _ => StatusFilter::NotRun,
        }
    }

    /// Opens the template panel on the last row's prompt, or closes it.
    fn toggle_template(&mut self) -> Result<(), Error> {
        if self.sheet.template.take().is_some() {
            return Ok(());
        }
        let text = self.sheet_rows().rows.last().map(|row| row.prompt.clone()).unwrap_or_default();
        self.sheet.template = Some(TemplateDraft::new(text, self.sheet_seed()?));
        Ok(())
    }

    /// The template, its variables and copies, and a live count of the rows it would make.
    fn template_panel(&mut self, ui: &mut egui::Ui, t: &Tokens, existing: usize, actions: &mut Vec<Action>) {
        let canvas = self.canvas;
        let revision = self.session.revision();
        let sources = self.image_sources(revision);
        let Some(draft) = self.sheet.template.as_mut() else { return };
        let frame = crate::theme::panel_frame(t);
        frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                let name = ui.label("Template");
                ui.add(
                    egui::TextEdit::singleline(&mut draft.text)
                        .hint_text("a {{colour}} mug on {{surface}}")
                        .desired_width(t.sheet_title_width * 2.0)
                        .id(egui::Id::new("sheet_template")),
                )
                .labelled_by(name.id)
                .on_hover_text("{{name}} is a variable's value; {{{{ and }}}} are literal braces");
                let copies = ui.label("Copies");
                ui.add(egui::DragValue::new(&mut draft.copies).range(1..=100)).labelled_by(copies.id).on_hover_text("Rows made of each combination");
            });
            let mut remove = None;
            for (n, variable) in draft.variables.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let label = ui.label("Variable");
                    ui.add(
                        egui::TextEdit::singleline(&mut variable.name)
                            .hint_text("colour")
                            .desired_width(t.sheet_column_width)
                            .id(egui::Id::new(("sheet_variable", n))),
                    )
                    .labelled_by(label.id);
                    let images = variable.images.is_some();
                    if ui.selectable_label(!images, "Text").on_hover_text("Values written into the prompt where {{name}} is").clicked() && images {
                        variable.images = None;
                    }
                    if ui.selectable_label(images, "Images").on_hover_text("Image sources on the sheet, one wired into each row").clicked() && !images {
                        variable.images = Some(Vec::new());
                    }
                    match variable.images.as_mut() {
                        None => {
                            let label = ui.label("Values");
                            ui.add(
                                egui::TextEdit::multiline(&mut variable.values)
                                    .hint_text("One value per line")
                                    .desired_rows(2)
                                    .desired_width(t.sheet_title_width)
                                    .id(egui::Id::new(("sheet_values", n))),
                            )
                            .labelled_by(label.id);
                            ui.checkbox(&mut variable.empty, "Empty value").on_hover_text("Also make rows where this variable inserts nothing");
                        }
                        Some(chosen) => {
                            if sources.is_empty() {
                                ui.label(egui::RichText::new("The sheet has no image sources to choose").color(t.text_muted));
                            }
                            for (id, title) in sources.iter() {
                                let mut on = chosen.contains(id);
                                if ui.checkbox(&mut on, title.as_str()).changed() {
                                    if on {
                                        chosen.push(*id);
                                        // In the sheet's order, whatever order they were ticked in.
                                        chosen.sort_by_key(|id| sources.iter().position(|(source, _)| source == id));
                                    } else {
                                        chosen.retain(|other| other != id);
                                    }
                                }
                            }
                        }
                    }
                    if ui.button("Remove variable").clicked() {
                        remove = Some(n);
                    }
                });
            }
            if let Some(n) = remove
                && n < draft.variables.len()
            {
                draft.variables.remove(n);
            }
            let params = draft.params(canvas);
            let key = params.to_string();
            let fresh = draft.preview.as_ref().is_some_and(|(seen, params, _)| *seen == revision && *params == key);
            if !fresh {
                let mut dry = params.clone();
                dry["dry_run"] = json!(true);
                let result = self.session.dispatch("sheet.expand", &dry).map_err(|error| error.message);
                draft.preview = Some((revision, key, result));
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(draft.variables.len() < 8, egui::Button::new("Add variable")).clicked() {
                    draft.variables.push(VariableDraft::default());
                }
                let preview = draft.preview.as_ref().map(|(_, _, result)| result);
                let fits = matches!(preview, Some(Ok(result)) if result["fits"] == true);
                let count = preview.and_then(|result| result.as_ref().ok()).and_then(|result| result["count"].as_u64()).unwrap_or(0);
                let label = if fits { format!("Expand into {count} rows") } else { "Expand".to_owned() };
                if ui
                    .add_enabled(fits, egui::Button::new(label))
                    .on_hover_text("Add the rows after the last, copying its model and settings; nothing runs")
                    .clicked()
                {
                    actions.push(Action::Expand(params));
                }
                match preview {
                    Some(Ok(result)) if fits => {
                        let factors = result["factors"].as_str().unwrap_or_default();
                        ui.label(
                            egui::RichText::new(format!(
                                "{count} new rows ({factors}); the sheet will have {}",
                                existing.saturating_add(usize::try_from(count).unwrap_or(0))
                            ))
                            .color(t.text_muted),
                        );
                    }
                    Some(Ok(result)) => {
                        ui.colored_label(t.danger, line(result["problem"].as_str().unwrap_or_default()));
                    }
                    Some(Err(message)) => {
                        ui.colored_label(t.danger, line(message));
                    }
                    None => {}
                }
            });
            if let Some(Ok(result)) = draft.preview.as_ref().map(|(_, _, result)| result) {
                for prompt in result["prompts"].as_array().into_iter().flatten().filter_map(Json::as_str) {
                    ui.label(egui::RichText::new(line(prompt)).color(t.text_muted));
                }
            }
        });
    }

    /// The image sources on the open sheet that aren't rows, top to bottom, with their titles:
    /// what an image variable can list.
    fn image_sources(&mut self, revision: u64) -> Sources {
        if let Some((canvas, seen, sources)) = &self.sheet.sources
            && *canvas == self.canvas
            && *seen == revision
        {
            return Arc::clone(sources);
        }
        let sources = Arc::new(self.read_image_sources());
        self.sheet.sources = Some((self.canvas, revision, Arc::clone(&sources)));
        sources
    }

    fn read_image_sources(&self) -> Vec<(NodeId, String)> {
        let Some(sheet) = self.session.document().canvases().get(&self.canvas) else { return Vec::new() };
        let mut found: Vec<&Node> = sheet
            .nodes()
            .values()
            .filter(|node| !nodecraft_nodes::generation::is_generation(&node.kind))
            .filter(|node| matches!(node.outputs.as_slice(), [port] if port.port_type.modality == Modality::Image))
            .collect();
        found.sort_by(|a, b| a.pos.y.total_cmp(&b.pos.y).then(a.id.cmp(&b.id)));
        found.into_iter().map(|node| (node.id, line(&card::title(node, self.session.kinds().get(&node.kind).ok().map(|kind| kind.descriptor()))))).collect()
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
                return egui::RichText::new(status).color(t.accent_text);
            }
            let card = runs.card(self.canvas, id);
            if card.held {
                return egui::RichText::new("Held: place or discard it in the queue").color(t.stale);
            }
            if let Some(reason) = runs.failure(self.canvas, id) {
                let text = if card.maybe_charged { format!("{} (may already have been charged)", line(reason)) } else { line(reason) };
                return egui::RichText::new(text).color(t.danger);
            }
            if card.cancelled {
                return egui::RichText::new(if card.maybe_charged { "Cancelled after it was sent (cost unresolved)" } else { "Cancelled" }).color(t.text_muted);
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
        let mut rows = match self.session.dispatch("sheet.rows", &json!({"canvas": canvas})) {
            Ok(value) => Rows::read(&value, |references| self.references_label(canvas, references)),
            Err(error) => Rows { rows: Vec::new(), error: Some(error.message) },
        };
        if let Some(sheet) = self.session.document().canvases().get(&canvas) {
            let chars = self.theme.tokens().preview_chars;
            // Rows mostly share a few models: read each model's contract once per revision.
            let mut contracts = BTreeMap::new();
            for row in &mut rows.rows {
                if let Some(node) = sheet.nodes().get(&row.id) {
                    row.preview = preview_of(node, chars);
                    let model = match node.params.get(nodecraft_nodes::generation::MODEL) {
                        Some(Value::Text(model)) => model.as_str(),
                        _ => "",
                    };
                    // An instance's contract follows its kind and model, and its ports: a row whose
                    // saved model is unavailable keeps the ports it had, so two rows naming one
                    // model with different ports don't share a contract.
                    let ports: Vec<&str> = node.inputs.iter().map(|port| port.name.as_str()).collect();
                    let key = (node.kind.as_str(), model, ports);
                    let found = contracts.entry(key).or_insert_with(|| {
                        tools::row_contract(&self.session, node).map(|contract| {
                            let aspect = tools::aspect_in(&contract);
                            (contract, aspect)
                        })
                    });
                    if let Some((contract, aspect)) = found {
                        row.aspect = aspect.as_ref().map(|aspect| aspect.of(node));
                        // References are checked with the model's input roles and counts here,
                        // as a quote would check them, so a row that can't run says so first.
                        if row.wired {
                            row.problem = tools::wiring_problem(&self.session, sheet, contract, node);
                        }
                    }
                }
            }
        }
        let rows = Arc::new(rows);
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
            Action::Template => self.toggle_template(),
            Action::Expand(params) => self.dispatch_sheet("sheet.expand", params).map(|()| self.sheet.template = None),
            Action::Select(rows) => {
                self.selection.clear();
                for row in rows {
                    self.selection.toggle(row);
                }
                Ok(())
            }
            Action::Toggle(row) => {
                self.selection.toggle(row);
                Ok(())
            }
            Action::Place(rows) => self.place_results(rows),
            Action::Export(rows) => {
                self.start_results_save_nodes(rows, true);
                Ok(())
            }
            Action::Step(row, newer) => self.dispatch_sheet(if newer { "history.next" } else { "history.previous" }, json!({"canvas": canvas, "id": row})),
            Action::Model(rows) => {
                // The picker belongs to exactly the rows it was opened for.
                self.selection.clear();
                for row in &rows {
                    self.selection.toggle(*row);
                }
                let revision = self.session.revision();
                self.open_model_picker(canvas, rows, revision)
            }
            Action::Param(row, name, value) => self.dispatch_sheet("node.param_set", json!({"canvas": canvas, "id": row, "name": name, "value": value})),
            Action::Wire(reference, rows) => self.sheet_seed().and_then(|seed| {
                let mut params = json!({"canvas": canvas, "seed": seed, "reference": reference});
                if let Some(rows) = rows {
                    params["rows"] = json!(rows);
                }
                self.dispatch_sheet("sheet.reference_wire", params)
            }),
            Action::Unwire(reference, rows) => {
                let mut params = json!({"canvas": canvas, "reference": reference});
                if let Some(rows) = rows {
                    params["rows"] = json!(rows);
                }
                self.dispatch_sheet("sheet.reference_unwire", params)
            }
            Action::Retry(rows) => self.request_runs(rows, super::generate::RunKind::Generate),
            Action::Cancel(rows) => self.cancel_rows(&rows),
            Action::Rename(title) => self.dispatch_sheet("sheet.rename", json!({"canvas": canvas, "title": title})).map(|()| self.sheet.renaming = None),
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(error.message);
                false
            }
        }
    }

    /// The project's plain canvases, with their titles.
    fn plain_canvases(&self) -> Vec<(CanvasId, String)> {
        let canvases = self.session.document().canvases().values();
        canvases
            .filter(|canvas| canvas.kind() == nodecraft_graph::CanvasKind::Canvas)
            .map(|canvas| (canvas.id(), if canvas.title().trim().is_empty() { "Canvas".to_owned() } else { line(canvas.title()) }))
            .collect()
    }

    /// Where results are sent: the canvas picked, else the plain canvas open before the sheet,
    /// else the project's first plain canvas.
    pub(super) fn place_target(&self) -> Option<CanvasId> {
        let canvases = self.session.document().canvases();
        let plain = |id: &CanvasId| canvases.get(id).is_some_and(|canvas| canvas.kind() == nodecraft_graph::CanvasKind::Canvas);
        self.sheet.target.filter(plain).or(self.sheet.last_plain.filter(plain)).or_else(|| home_canvas(self.session.document()).filter(plain))
    }

    /// Which canvas Send puts results on, when the project has more than one.
    fn target_picker(&mut self, ui: &mut egui::Ui) {
        let canvases = self.plain_canvases();
        if canvases.len() < 2 {
            return;
        }
        let current = self.place_target();
        let shown = canvases.iter().find(|(id, _)| Some(*id) == current).map(|(_, title)| title.clone()).unwrap_or_default();
        egui::ComboBox::from_id_salt("sheet_place_target")
            .selected_text(format!("to {shown}"))
            .show_ui(ui, |ui| {
                for (id, title) in &canvases {
                    if ui.selectable_label(Some(*id) == current, title).clicked() {
                        self.sheet.target = Some(*id);
                    }
                }
            })
            .response
            .on_hover_text("The canvas results are sent to");
    }

    /// Places `rows`' results on the target canvas, right of what it holds, and says so.
    fn place_results(&mut self, rows: Vec<NodeId>) -> Result<(), Error> {
        let target = self.place_target().ok_or_else(|| Error::new(ErrorKind::Unsupported, "the project has no canvas to send results to"))?;
        let document = self.session.document();
        let title = document.canvases().get(&target).map(|canvas| line(canvas.title())).unwrap_or_default();
        let seed = self.sheet_seed()?;
        let placed = self.session.dispatch("sheet.place", &json!({"canvas": self.canvas, "seed": seed, "rows": rows, "target": target}))?;
        let count = placed["placed"].as_array().map_or(0, Vec::len);
        let skipped = placed["skipped"].as_array().map_or(0, Vec::len);
        let cards = if count == 1 { "1 result".to_owned() } else { format!("{count} results") };
        let title = if title.trim().is_empty() { "the canvas".to_owned() } else { format!("\"{title}\"") };
        self.notice = Some(match skipped {
            0 => format!("Sent {cards} to {title}."),
            1 => format!("Sent {cards} to {title}; 1 had nothing to send."),
            n => format!("Sent {cards} to {title}; {n} had nothing to send."),
        });
        Ok(())
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

mod tools;
#[cfg(test)]
pub(super) use tools::CONTRACTS_READ;

/// A grid tile's badge for a row in `state`.
fn state_badge(state: StatusFilter, t: &Tokens) -> (&'static str, egui::Color32) {
    match state {
        StatusFilter::Running => ("Running", t.accent_text),
        StatusFilter::Failed => ("Failed", t.danger),
        StatusFilter::Held => ("Held", t.stale),
        StatusFilter::Cancelled => ("Cancelled", t.text_muted),
        StatusFilter::OutOfDate => ("Out of date", t.stale),
        StatusFilter::Partial => ("Partial", t.stale),
        StatusFilter::Done => ("Done", t.text),
        StatusFilter::NotRun | StatusFilter::All => ("Not run", t.text_muted),
    }
}

/// What a grid tile shows of `node`'s active result: its first image, else the start of its text.
pub(super) fn preview_of(node: &Node, chars: usize) -> Preview {
    let (images, _) = nodecraft_engine::split::image_rows(node);
    if let Some(asset) = images.first().and_then(|row| row.first()) {
        return Preview::Image((*asset).clone());
    }
    let active = node.active.and_then(|id| node.history.iter().find(|g| g.id == id));
    match active.and_then(|generation| generation.outputs.first()) {
        Some(Value::Text(text)) => Preview::Text(text.chars().take(chars).collect()),
        _ => Preview::Nothing,
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
