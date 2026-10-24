//! App mode (#95): a technique node shown as a simple form. Its exposed inputs are filled from
//! fields and chosen files, its exposed parameters are set, it runs through the same price
//! confirmation as the canvas, and its results are browsed in a gallery. The form is
//! `app.form`; every change is a command (`app.input_set`, `node.param_set`, `history.*`), so
//! each is one undo step and the canvas shows the same graph.
use super::NodeCraftApp;
use crate::card::{EditTarget, EditorState};
use crate::theme::Tokens;
use egui::{Rect, Sense, Vec2};
use nodecraft_graph::Generation;
use nodecraft_types::{CanvasId, GenerationId, Modality, NodeId, ParamControl, ParamSpec, PortType, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

/// A text input's field: as large as a technique's text source takes.
const TEXT: ParamControl = ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 };

pub(super) struct AppMode {
    canvas: CanvasId,
    pub(super) node: NodeId,
    /// `app.form` as of a document revision.
    form: Option<(u64, Form)>,
    pub(super) editor: EditorState,
    #[cfg(not(target_arch = "wasm32"))]
    loading: Option<files::Loading>,
}

impl AppMode {
    pub(super) fn new(canvas: CanvasId, node: NodeId) -> Self {
        Self {
            canvas,
            node,
            form: None,
            editor: EditorState::default(),
            #[cfg(not(target_arch = "wasm32"))]
            loading: None,
        }
    }
}

#[derive(Deserialize)]
struct Form {
    name: String,
    version: u32,
    #[serde(default)]
    description: String,
    inputs: Vec<Input>,
    params: Vec<Param>,
    ready: bool,
    blocked: Option<String>,
    #[serde(default)]
    issues: Vec<String>,
    /// What a run would cost now, from `run.quote`; none when it can't be priced here.
    #[serde(skip)]
    price: Option<String>,
}

#[derive(Deserialize)]
struct Input {
    port: String,
    name: String,
    port_type: PortType,
    fillable: bool,
    feed: String,
    values: Vec<Value>,
    example: Option<Value>,
}

impl Input {
    /// What the input shows: what the form filled it with, else the version's example.
    fn shown(&self) -> Vec<Value> {
        if self.feed == "form" { self.values.clone() } else { self.example.iter().cloned().collect() }
    }
}

#[derive(Deserialize)]
struct Param {
    spec: ParamSpec,
    value: Option<Value>,
}

/// What the form asked for this frame, applied once it is drawn.
enum Ask {
    Command(&'static str, Json, Option<EditTarget>),
    /// Fill input `port` with these values; the field that asked, if one did.
    Fill(String, Vec<Value>, Option<String>),
    /// Choose a file for input `port`: into slot `n`, or added when `None`.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Choose(String, Option<usize>),
    Run,
    History(GenerationId, bool),
    /// Save a result's first output (item `item` of a list) to a file named like `name`.
    Export(GenerationId, Option<usize>, String),
}

impl NodeCraftApp {
    /// Opens App mode on technique node `node` of the open canvas.
    pub(super) fn open_app_mode(&mut self, node: NodeId) {
        self.app_mode = Some(AppMode::new(self.canvas, node));
    }

    /// The one selected node, when it is a technique node.
    pub(super) fn selected_technique(&self) -> Option<NodeId> {
        let nodes = self.selection.nodes();
        let id = *nodes.first().filter(|_| nodes.len() == 1)?;
        let canvas = self.session.document().canvases().get(&self.canvas)?;
        canvas.nodes().get(&id).filter(|node| node.kind == nodecraft_nodes::technique::ID).map(|node| node.id)
    }

    /// The form, while it is open. True when it owns the frame.
    pub(super) fn app_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut app) = self.app_mode.take() else {
            return false;
        };
        let node = (app.canvas == self.canvas)
            .then(|| self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&app.node)))
            .flatten()
            .filter(|n| n.kind == nodecraft_nodes::technique::ID)
            .cloned();
        let Some(node) = node else {
            // The node went (undo, another canvas): nothing left to show.
            return false;
        };
        let revision = self.session.revision();
        if app.form.as_ref().is_none_or(|(r, _)| *r != revision) {
            let read = self.session.dispatch("app.form", &json!({"canvas": app.canvas, "id": app.node})).map_err(|e| e.message);
            match read.and_then(|v| serde_json::from_value::<Form>(v).map_err(|_| "The form can't be read.".to_owned())) {
                Ok(mut form) => {
                    // Priced as Run prices it: the node with whatever upstream is stale.
                    let quote = self
                        .session
                        .dispatch("run.quote", &json!({"canvas": app.canvas, "mode": super::generate::RunKind::Refresh.mode(), "nodes": [app.node]}));
                    form.price = quote.ok().as_ref().and_then(super::generate::Quote::read).map(|quote| quote.price());
                    app.form = Some((revision, form));
                }
                Err(message) => {
                    self.notice = Some(message);
                    return false;
                }
            }
        }
        let Some((_, form)) = app.form.take() else {
            return false;
        };
        let t = *self.theme.tokens();
        let ctx = ui.ctx().clone();
        // Files dropped while the form is open would land on the canvas behind it.
        ctx.input_mut(|i| {
            i.raw.dropped_files.clear();
            i.raw.hovered_files.clear();
        });
        #[cfg(not(target_arch = "wasm32"))]
        self.poll_app_file(&mut app, &form, &ctx);

        let edge = t.app_thumbnail * ctx.pixels_per_point();
        let mut thumbnails = std::collections::HashMap::new();
        let shown = form.inputs.iter().flat_map(Input::shown);
        let results = node.history.iter().rev().filter_map(|g| first_output(g).cloned());
        for value in shown.chain(results) {
            if let Value::Media(asset) = value
                && (asset.mime.essence().starts_with("image/") || asset.mime.essence() == "application/pdf")
            {
                let assets = self.session.assets();
                let preview = self.previews.request_asset(&ctx, asset.hash, edge, || assets.preview_read(&asset));
                thumbnails.insert(asset.hash, preview);
            }
        }
        let status = self.runs.as_ref().and_then(|runs| super::queue_panel::card_status(runs, app.canvas, app.node, &t));
        #[cfg(not(target_arch = "wasm32"))]
        let loading = app.loading.as_ref().map(|loading| loading.port().to_owned());
        #[cfg(target_arch = "wasm32")]
        let loading: Option<String> = None;

        let mut asks = Vec::new();
        let mut close = false;
        let notice = self.notice.clone();
        let content = ctx.content_rect();
        let gap = t.control_gap * 2.0;
        let shell = egui::Frame::NONE.fill(t.panel_fill).inner_margin(gap);
        egui::Modal::new(egui::Id::new("app_mode")).frame(shell).backdrop_color(t.canvas_bg).show(&ctx, |ui| {
            let size = (content.size() - Vec2::splat(gap * 2.0)).max(Vec2::splat(1.0));
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.horizontal(|ui| {
                ui.heading(&form.name);
                ui.label(egui::RichText::new(format!("v{} · {}", form.version, node.title)).color(t.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close |= ui.button("Done").on_hover_text("Back to the canvas").clicked();
                    if ui.button("Redo").clicked() {
                        asks.push(Ask::Command("edit.redo", json!({}), None));
                    }
                    if ui.button("Undo").clicked() {
                        asks.push(Ask::Command("edit.undo", json!({}), None));
                    }
                    if let Some(notice) = &notice {
                        ui.label(egui::RichText::new(notice).color(t.danger));
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().id_salt("app_form").auto_shrink([false, false]).show(ui, |ui| {
                let width = t.app_form_width.min(ui.available_width());
                let side = ((ui.available_width() - width) / 2.0).max(0.0);
                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        let view = View { app: &mut app, form: &form, thumbnails: &thumbnails, loading: loading.as_deref(), t: &t };
                        view.show(ui, &node.history, node.active, status.as_ref(), &mut asks);
                    });
                });
            });
        });
        // The price dialog sits on top: its Escape and the editor's undo keys are its own.
        if self.run_confirm.is_none() {
            close |= keys(ui, &mut asks);
        }
        app.form = Some((revision, form));
        for ask in asks {
            if !self.app_ask(&mut app, ask, &t) {
                break;
            }
        }
        if !close {
            self.app_mode = Some(app);
        }
        true
    }

    /// Applies one thing the form asked for. False when it failed, so nothing after it runs.
    fn app_ask(&mut self, app: &mut AppMode, ask: Ask, t: &Tokens) -> bool {
        let (command, params, target) = match ask {
            Ask::Command(command, params, target) => (command, params, target),
            Ask::Fill(port, values, field) => match fill(app, &port, values) {
                Ok(params) => ("app.input_set", params, field.map(EditTarget::Parameter)),
                Err(message) => {
                    self.notice = Some(message);
                    return false;
                }
            },
            Ask::Choose(port, slot) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    app.loading = Some(files::Loading::start(&port, slot));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = (port, slot);
                    self.notice = Some("Choosing a file works in the desktop app.".into());
                }
                return true;
            }
            Ask::Run => {
                return match self.request_runs(vec![app.node], super::generate::RunKind::Refresh) {
                    Ok(()) => true,
                    Err(error) => {
                        self.notice = Some(error.message);
                        false
                    }
                };
            }
            Ask::History(generation, extract) => {
                let intent =
                    if extract { super::HistoryIntent::Extract { generation, port: None, item: None } } else { super::HistoryIntent::Select(generation) };
                match self.history_command(app.node, intent, t) {
                    Some((command, params)) => (command, params, None),
                    None => return false,
                }
            }
            Ask::Export(generation, item, name) => {
                self.start_export(app.canvas, app.node, generation, None, item, &name);
                return true;
            }
        };
        match self.session.dispatch(command, &params) {
            Ok(_) => {
                self.notice = None;
                if let Some(target) = target {
                    app.editor.accepted(&target);
                }
                true
            }
            Err(error) => {
                if let Some(target) = target {
                    app.editor.failed(target, error.message.clone());
                }
                self.notice = Some(error.message);
                false
            }
        }
    }

    /// Puts a chosen file into input `port` of the open form, as if read from disk.
    #[cfg(test)]
    pub(super) fn app_file_chosen(&mut self, port: &str, slot: Option<usize>, name: &str, bytes: &[u8]) {
        let Some(mut app) = self.app_mode.take() else {
            return;
        };
        if let Some((revision, form)) = app.form.take() {
            self.app_fill_file(&mut app, &form, port, slot, name, bytes);
            app.form = Some((revision, form));
        }
        self.app_mode = Some(app);
    }

    /// Stores a chosen file as an asset and fills input `port` with it (`app.input_set`), into
    /// slot `slot` or added after the others.
    fn app_fill_file(&mut self, app: &mut AppMode, form: &Form, port: &str, slot: Option<usize>, name: &str, bytes: &[u8]) {
        self.app_fill_probed_file(app, form, port, slot, &nodecraft_engine::import::Incoming { name, bytes }, None);
    }

    fn app_fill_probed_file(
        &mut self,
        app: &mut AppMode,
        form: &Form,
        port: &str,
        slot: Option<usize>,
        file: &nodecraft_engine::import::Incoming<'_>,
        movie: Option<&nodecraft_engine::import::ProbedMovie>,
    ) {
        let Some(input) = form.inputs.iter().find(|i| i.port == port) else {
            return;
        };
        let filled = if input.feed == "form" { input.values.clone() } else { Vec::new() };
        let many = input.port_type.many;
        let t = *self.theme.tokens();
        match nodecraft_engine::import::import_media_with_movie(&mut self.session, file, movie) {
            Ok(asset) => {
                self.app_ask(app, Ask::Fill(port.to_owned(), placed(filled, many, slot, Value::Media(asset)), None), &t);
            }
            Err(error) => self.notice = Some(error.message),
        }
    }
}

/// `values` with `value` in slot `slot` (or added): the only value when the input takes one.
fn placed(mut values: Vec<Value>, many: bool, slot: Option<usize>, value: Value) -> Vec<Value> {
    if !many {
        return vec![value];
    }
    match slot.and_then(|n| values.get_mut(n)) {
        Some(at) => *at = value,
        None => values.push(value),
    }
    values
}

/// `app.input_set` for input `port`, under a fresh seed for any source it adds.
fn fill(app: &AppMode, port: &str, values: Vec<Value>) -> Result<Json, String> {
    let seed = crate::ids::random_id(|_| false).map(NodeId::from_bytes).map_err(|error| error.message)?;
    Ok(json!({"canvas": app.canvas, "id": app.node, "port": port, "seed": seed, "values": values}))
}

/// The editor key of slot `n` of input `port`; never a parameter's name, which has no colon.
fn input_key(port: &str, n: usize) -> String {
    format!("input:{port}:{n}")
}

/// A result's first output, or the first item of a list.
fn first_output(generation: &Generation) -> Option<&Value> {
    match generation.outputs.first()? {
        Value::List(items) => items.first(),
        value => Some(value),
    }
}

/// The media a result's first output exports (item `Some(0)` of a list), and its file name.
fn exportable(generation: &Generation) -> Option<(Option<usize>, String)> {
    let (item, value) = match generation.outputs.first()? {
        Value::List(items) => (Some(0), nodecraft_engine::history_cmds::list_items(items, !generation.items.is_empty()).next()?),
        value => (None, value),
    };
    match value {
        Value::Media(asset) => Some((item, crate::history::suggested_name(asset.mime.essence()))),
        _ => None,
    }
}

/// Undo and redo as on the canvas, and Escape to close; not while a field has focus.
fn keys(ui: &egui::Ui, asks: &mut Vec<Ask>) -> bool {
    if crate::canvas::text_input_focused(ui.ctx()) {
        return false;
    }
    ui.input_mut(|i| {
        for (id, shortcut) in [("edit.redo", "Command+Shift+Z"), ("edit.redo", "Command+Y"), ("edit.undo", "Command+Z")] {
            if super::consume_shortcut(i, shortcut) {
                asks.push(Ask::Command(id, json!({}), None));
            }
        }
        i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
    })
}

struct View<'a> {
    app: &'a mut AppMode,
    form: &'a Form,
    thumbnails: &'a std::collections::HashMap<nodecraft_types::Blake3, crate::preview::Preview>,
    /// The input a file is being chosen or read for.
    loading: Option<&'a str>,
    t: &'a Tokens,
}

impl View<'_> {
    fn show(
        mut self,
        ui: &mut egui::Ui,
        history: &[Generation],
        active: Option<GenerationId>,
        status: Option<&(String, egui::Color32, Option<String>)>,
        asks: &mut Vec<Ask>,
    ) {
        let t = self.t;
        if !self.form.description.is_empty() {
            ui.label(&self.form.description);
            ui.add_space(t.control_gap);
        }
        if !self.form.inputs.is_empty() {
            ui.heading("Inputs");
            for input in &self.form.inputs {
                ui.push_id(("input", &input.port), |ui| self.input(ui, input, asks));
                ui.add_space(t.control_gap);
            }
        }
        if !self.form.params.is_empty() {
            ui.heading("Settings");
            for param in &self.form.params {
                ui.push_id(("param", &param.spec.name), |ui| {
                    let mut label = ui.label(&param.spec.label);
                    if let Some(description) = &param.spec.description {
                        label = label.on_hover_text(description);
                    }
                    ui.horizontal(|ui| {
                        let value = param.value.as_ref();
                        let width = (ui.available_width() - t.card_width / 3.0).max(t.card_width / 2.0);
                        let intent = field(ui, width, t, |ui| {
                            crate::card::control_with_mode(ui, &param.spec.control, value, &param.spec.name, &mut self.app.editor, label.id, t, false)
                        });
                        let reset = ui.small_button("Reset").on_hover_text("Back to the technique's own value").clicked();
                        if let Some(value) = intent.or(reset.then_some(None)) {
                            let params = json!({"canvas": self.app.canvas, "id": self.app.node, "name": param.spec.name, "value": value});
                            asks.push(Ask::Command("node.param_set", params, Some(EditTarget::Parameter(param.spec.name.clone()))));
                        }
                    });
                });
                ui.add_space(t.control_gap);
            }
        }
        ui.separator();
        ui.horizontal(|ui| {
            let run = ui.add_enabled(
                self.form.ready,
                egui::Button::new(egui::RichText::new("Run").strong()).stroke(egui::Stroke::new(1.0, t.accent)).min_size(Vec2::new(t.app_thumbnail, 0.0)),
            );
            if run.on_hover_text("Shows the price, then runs").clicked() {
                asks.push(Ask::Run);
            }
            if let Some(price) = &self.form.price {
                ui.label(egui::RichText::new(price).color(t.text_muted)).on_hover_text("What a run costs now; shown again before it starts");
            }
            if let Some((label, colour, why)) = status {
                let shown = ui.label(egui::RichText::new(label).color(*colour));
                if let Some(why) = why {
                    shown.on_hover_text(why);
                }
            }
        });
        if let Some(blocked) = &self.form.blocked {
            ui.colored_label(t.danger, blocked);
        }
        for issue in &self.form.issues {
            ui.colored_label(t.danger, issue);
        }
        if let Some(error) = self.app.editor.error() {
            ui.colored_label(t.danger, error);
        }
        ui.add_space(t.control_gap);
        ui.heading(format!("Results ({})", history.len()));
        if history.is_empty() {
            ui.label(egui::RichText::new("Run it to see results here.").color(t.text_muted));
        }
        ui.horizontal_wrapped(|ui| {
            for generation in history.iter().rev() {
                ui.push_id(generation.id, |ui| {
                    ui.vertical(|ui| {
                        ui.set_width(t.app_thumbnail);
                        let shown = first_output(generation);
                        let response = self.thumbnail(ui, shown, active == Some(generation.id));
                        if response.on_hover_text("Show this result on the node").clicked() {
                            asks.push(Ask::History(generation.id, false));
                        }
                        if ui.small_button("Send to canvas").on_hover_text("Add this result to the canvas as its own node").clicked() {
                            asks.push(Ask::History(generation.id, true));
                        }
                        if let Some((item, name)) = exportable(generation)
                            && ui.small_button("Export…").on_hover_text("Save this result to a file").clicked()
                        {
                            asks.push(Ask::Export(generation.id, item, name));
                        }
                    });
                });
            }
        });
    }

    fn input(&mut self, ui: &mut egui::Ui, input: &Input, asks: &mut Vec<Ask>) {
        let t = self.t;
        let label = ui.label(egui::RichText::new(&input.name).strong());
        if input.feed == "wired" {
            label.on_hover_text("Remove its wires on the canvas to fill it here");
            ui.label(egui::RichText::new("Wired on the canvas").color(t.text_muted));
            return;
        }
        if !input.fillable {
            ui.label(egui::RichText::new("Can't be filled here yet; wire it on the canvas").color(t.text_muted));
            return;
        }
        let example = input.feed != "form";
        if example {
            ui.label(egui::RichText::new("The technique's example, until you change it").color(t.text_muted));
        }
        // What the form filled it with; an example is replaced, not added to.
        let filled = if example { Vec::new() } else { input.values.clone() };
        let without = |n: usize| filled.iter().enumerate().filter(|(i, _)| *i != n).map(|(_, v)| v.clone()).collect::<Vec<_>>();
        let shown = input.shown();
        let many = input.port_type.many;
        if input.port_type.modality == Modality::Text {
            let empty = Value::Text(String::new());
            for (n, value) in shown.iter().enumerate().chain(shown.is_empty().then_some((0, &empty))) {
                ui.push_id(n, |ui| {
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - t.card_width / 3.0).max(t.card_width / 2.0);
                        let key = input_key(&input.port, n);
                        let edited =
                            field(ui, width, t, |ui| crate::card::control_with_mode(ui, &TEXT, Some(value), &key, &mut self.app.editor, label.id, t, false));
                        if let Some(Some(edited)) = edited {
                            asks.push(Ask::Fill(input.port.clone(), placed(filled.clone(), many, Some(n), edited), Some(key)));
                        }
                        if many && !example && ui.small_button("Remove").clicked() {
                            asks.push(Ask::Fill(input.port.clone(), without(n), None));
                        }
                    });
                });
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                for (n, value) in shown.iter().enumerate() {
                    ui.push_id(n, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(t.app_thumbnail);
                            self.thumbnail(ui, Some(value), false);
                            if !example && ui.small_button("Remove").clicked() {
                                asks.push(Ask::Fill(input.port.clone(), without(n), None));
                            }
                        });
                    });
                }
            });
        }
        ui.horizontal(|ui| {
            if input.port_type.modality != Modality::Text {
                if self.loading == Some(input.port.as_str()) {
                    ui.spinner();
                    ui.label("Reading the file…");
                } else {
                    let choose = ui.add_enabled(self.loading.is_none(), egui::Button::new(if many { "Add a file…" } else { "Choose a file…" }));
                    if choose.clicked() {
                        asks.push(Ask::Choose(input.port.clone(), None));
                    }
                }
            } else if many && ui.small_button("Add another").clicked() {
                asks.push(Ask::Fill(input.port.clone(), placed(filled.clone(), many, None, Value::Text(String::new())), None));
            }
            if !example && ui.small_button("Use the example").on_hover_text("Run with the technique's own value again").clicked() {
                asks.push(Ask::Fill(input.port.clone(), Vec::new(), None));
            }
        });
    }

    /// A square showing `value`: an image, the start of a text, or what kind of media it is.
    fn thumbnail(&self, ui: &mut egui::Ui, value: Option<&Value>, active: bool) -> egui::Response {
        let t = self.t;
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(t.app_thumbnail), Sense::click());
        let stroke = if active { egui::Stroke::new(2.0, t.accent) } else { egui::Stroke::new(1.0, t.panel_stroke) };
        ui.painter().rect_filled(rect, f32::from(t.panel_radius) / 2.0, t.canvas_bg);
        match value {
            Some(Value::Media(asset)) => match self.thumbnails.get(&asset.hash) {
                Some(preview) => crate::card::show_image(ui, rect.shrink(2.0), preview, None, t),
                None => caption(ui, rect, asset.mime.essence(), t),
            },
            Some(Value::Text(text)) => {
                let start: String = text.chars().take(t.preview_chars).collect();
                caption(ui, rect, &start, t);
            }
            Some(_) => caption(ui, rect, "Result", t),
            None => caption(ui, rect, "Empty", t),
        }
        ui.painter().rect_stroke(rect, f32::from(t.panel_radius) / 2.0, stroke, egui::StrokeKind::Inside);
        response
    }
}

/// A framed field `width` wide holding a control, its text from the left.
fn field<R>(ui: &mut egui::Ui, width: f32, t: &Tokens, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(t.prompt_fill)
        .stroke(egui::Stroke::new(1.0, t.panel_stroke))
        .corner_radius(t.card_radius / 2)
        .inner_margin(t.control_gap)
        .show(ui, |ui| {
            let inner = Vec2::new((width - 2.0 * t.control_gap).max(0.0), t.control_row_height);
            ui.allocate_ui_with_layout(inner, egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(inner.x);
                add(ui)
            })
            .inner
        })
        .inner
}

fn caption(ui: &mut egui::Ui, rect: Rect, text: &str, t: &Tokens) {
    let inner = rect.shrink(t.control_gap);
    ui.put(inner, egui::Label::new(egui::RichText::new(text).size(t.label_size).color(t.text_muted)).wrap().selectable(false));
}

#[cfg(not(target_arch = "wasm32"))]
mod files;
