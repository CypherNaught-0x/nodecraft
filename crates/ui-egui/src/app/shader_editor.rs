//! The shader editor (#90): a window over a custom shader card where its WGSL and declared
//! parameters are edited, checked as they are typed, and stored with `node.shader_set`. A text
//! model can write or fix the shader: `shader.write` adds a request card, which is quoted and run
//! like any other, and once it has an answer `shader.apply` stores it, or says why not, so the
//! model can be asked to fix that too. A stored shader can be saved to the shader library as the
//! next version of a name (`shader.save`, then `shader.update` records it on the card), and a card
//! from the library can be moved to a newer version.
use super::NodeCraftApp;
use super::generate::RunKind;
use crate::ids;
use crate::theme::Tokens;
use nodecraft_engine::shader_assist_cmds::{MAX_ERROR_BYTES, MAX_REQUEST_BYTES, answer, parse_reply};
use nodecraft_engine::shader_library::{MAX_NAME_CHARS, valid_name};
use nodecraft_gpu::shader::{MAX_PARAMS, MAX_SOURCE, ShaderParam, ShaderSpec, validate};
use nodecraft_nodes::shader::{Pin, StoredParam, StoredShader, pinned, stored};
use nodecraft_types::{CanvasId, GenerationId, NodeId};
use serde_json::json;

/// The shader library's names and latest versions as listed, or why it couldn't be read; `None`
/// where there is no shader library.
pub(super) type ShaderListing = Option<Result<Vec<(String, u32)>, String>>;

pub(super) struct ShaderEditor {
    canvas: CanvasId,
    pub(super) node: NodeId,
    /// The shader as edited, not yet stored.
    pub(super) draft: StoredShader,
    /// The draft's check: `Err` holds the compiler's message. `None` until checked.
    check: Option<Result<(), String>>,
    /// What to ask a model for.
    pub(super) request: String,
    /// A request card waiting for its answer, and the answer it had when asked.
    pub(super) pending: Option<(NodeId, Option<GenerationId>)>,
    /// Why a model's answer was refused, for a fix request to carry when the draft itself checks.
    pub(super) refusal: Option<String>,
    /// Why the last store or request failed.
    pub(super) error: Option<String>,
    /// The name to save the shader to the library as.
    pub(super) save_as: String,
    /// The library's shaders and their latest versions, as listed when the editor opened or
    /// last saved.
    library: ShaderListing,
    /// What the last save did.
    pub(super) saved: Option<String>,
}

impl ShaderEditor {
    fn new(canvas: CanvasId, node: NodeId, shader: StoredShader, from: Option<&Pin>, library: ShaderListing) -> Self {
        let save_as = from.map(|pin| pin.name.clone()).unwrap_or_default();
        let mut editor =
            Self { canvas, node, draft: shader, check: None, request: String::new(), pending: None, refusal: None, error: None, save_as, library, saved: None };
        editor.recheck();
        editor
    }

    fn recheck(&mut self) {
        let spec = ShaderSpec {
            source: self.draft.source.clone(),
            params: self
                .draft
                .params
                .iter()
                .map(|p| ShaderParam { name: p.name.clone(), label: p.label.clone(), min: p.min as f32, max: p.max as f32, default: p.default as f32 })
                .collect(),
        };
        self.check = Some(validate(&spec).map(|_| ()).map_err(|e| e.0));
    }

    /// What a fix request carries: the compiler's message for the draft, else why the model's
    /// last answer was refused. Cut to the request's limit.
    fn message(&self) -> Option<String> {
        match &self.check {
            Some(Err(message)) => Some(clip(message, MAX_ERROR_BYTES)),
            _ => self.refusal.as_ref().map(|message| clip(message, MAX_ERROR_BYTES)),
        }
    }
}

/// `text` cut at a char boundary to at most `limit` bytes.
fn clip(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

/// What the window asked for, applied after it is drawn.
enum Action {
    Store,
    Revert,
    Write {
        fix: bool,
    },
    /// Save the card's shader to the library under the typed name.
    Save,
    /// Give the card the latest version of the library shader it came from.
    Update,
}

impl NodeCraftApp {
    pub(super) fn open_shader_editor(&mut self, node: NodeId) {
        let Some(card) = self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&node)) else { return };
        let from = pinned(card);
        match stored(card) {
            Ok(shader) => {
                let library = self.library_shaders();
                self.shader_editor = Some(ShaderEditor::new(self.canvas, node, shader, from.as_ref(), library));
            }
            Err(error) => self.notice = Some(error.message),
        }
    }

    /// The shader library's names and latest versions, read afresh, or why they couldn't be.
    pub(super) fn library_shaders(&mut self) -> ShaderListing {
        // Disabled only where the host keeps no shader library.
        self.session.enabled("library.shaders", &json!({})).ok()?;
        let listed = match self.session.dispatch("library.shaders", &json!({})) {
            Ok(listed) => listed,
            Err(error) => return Some(Err(error.message)),
        };
        let Some(shaders) = listed.get("shaders").and_then(|shaders| shaders.as_array()) else {
            return Some(Err("The shader library's list couldn't be read.".to_owned()));
        };
        Some(Ok(shaders
            .iter()
            .filter_map(|shader| {
                let latest = u32::try_from(shader.get("latest")?.as_u64()?).ok()?;
                Some((shader.get("name")?.as_str()?.to_owned(), latest))
            })
            .collect()))
    }

    /// The editor's window, while it is open on a card that still exists.
    pub(super) fn shader_editor_window(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(editor) = &self.shader_editor else { return };
        let exists = self.session.document().canvases().get(&editor.canvas).is_some_and(|c| c.nodes().contains_key(&editor.node));
        if !exists || editor.canvas != self.canvas {
            self.shader_editor = None;
            return;
        }
        self.shader_answered();
        let busy = self.run_confirm.is_some();
        let Some(editor) = &self.shader_editor else { return };
        let card = self.session.document().canvases().get(&editor.canvas).and_then(|c| c.nodes().get(&editor.node));
        let on_card = card.and_then(|n| stored(n).ok());
        let from = card.and_then(pinned);
        let Some(editor) = &mut self.shader_editor else { return };
        let latest = from.as_ref().and_then(|pin| {
            let library = editor.library.as_ref()?.as_ref().ok()?;
            library.iter().find(|(name, _)| name.to_lowercase() == pin.name.to_lowercase()).map(|(_, latest)| *latest)
        });
        let applied = on_card.as_ref() == Some(&editor.draft);
        let mut open = true;
        let mut action = None;
        // Opens at the right, below the toolbar, leaving the canvas and its card in view.
        crate::floating::panel(ctx, "Custom shader", egui::Id::new("nodecraft.shader_editor"), crate::floating::Home::RightTop, t)
            .open(&mut open)
            .default_width(t.shader_editor_width)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32>, run once a pixel. Read a parameter as name(); pixel(x, y) and size() read the image.")
                        .color(t.text_muted),
                );
                let mut changed = false;
                let label = ui.label("WGSL source");
                egui::ScrollArea::vertical().id_salt("shader-source").max_height(t.shader_editor_height).show(ui, |ui| {
                    let source = egui::TextEdit::multiline(&mut editor.draft.source)
                        .code_editor()
                        .char_limit(MAX_SOURCE)
                        .desired_rows(12)
                        .desired_width(t.shader_editor_width)
                        .hint_text("WGSL");
                    changed |= ui.add(source).labelled_by(label.id).changed();
                });
                ui.separator();
                changed |= params(ui, &mut editor.draft.params, t);
                if changed {
                    editor.recheck();
                    editor.refusal = None;
                    editor.error = None;
                }
                match &editor.check {
                    Some(Err(message)) => {
                        // Compiler reports run long; they scroll rather than push the buttons away.
                        egui::ScrollArea::vertical().id_salt("shader-check").max_height(t.shader_message_height).show(ui, |ui| {
                            ui.colored_label(t.danger, message);
                        });
                    }
                    Some(Ok(())) => {
                        ui.label(egui::RichText::new("The shader is valid.").color(t.text_muted));
                    }
                    None => {}
                }
                if let Some(refusal) = &editor.refusal {
                    egui::ScrollArea::vertical().id_salt("shader-refusal").max_height(t.shader_message_height).show(ui, |ui| {
                        ui.colored_label(t.danger, refusal);
                    });
                }
                if let Some(error) = &editor.error {
                    egui::ScrollArea::vertical().id_salt("shader-error").max_height(t.shader_message_height).show(ui, |ui| {
                        ui.colored_label(t.danger, error);
                    });
                }
                ui.horizontal(|ui| {
                    let valid = matches!(editor.check, Some(Ok(())));
                    if ui.add_enabled(valid, egui::Button::new("Apply")).on_hover_text("Store this shader on the card; one undo step").clicked() {
                        action = Some(Action::Store);
                    }
                    if ui.button("Revert").on_hover_text("Go back to the card's shader").clicked() {
                        action = Some(Action::Revert);
                    }
                    if !applied {
                        ui.label(egui::RichText::new("Not applied yet").color(t.text_muted));
                    }
                });
                if let Some(Err(error)) = &editor.library {
                    ui.separator();
                    ui.label(egui::RichText::new(format!("The shader library can't be used: {error}")).color(t.text_muted));
                }
                if let Some(Ok(_)) = &editor.library {
                    ui.separator();
                    if let Some(pin) = &from {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("From the library: {} v{}", super::sheet_view::line(&pin.name), pin.version)).color(t.text_muted));
                            if let Some(latest) = latest.filter(|latest| *latest > pin.version)
                                && ui
                                    .add_enabled(applied, egui::Button::new(format!("Update to v{latest}")))
                                    .on_hover_text("Give the card the library's latest version; one undo step")
                                    .on_disabled_hover_text("Apply or revert the draft first")
                                    .clicked()
                            {
                                action = Some(Action::Update);
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        let label = ui.label("Save as");
                        ui.add(egui::TextEdit::singleline(&mut editor.save_as).char_limit(MAX_NAME_CHARS).desired_width(t.shader_name_width).hint_text("Name"))
                            .labelled_by(label.id);
                        let valid = matches!(editor.check, Some(Ok(())));
                        let named = valid_name(editor.save_as.trim());
                        if ui
                            .add_enabled(applied && valid && named, egui::Button::new("Save to library"))
                            .on_hover_text("Keep the card's shader as the next version of this name, for every project")
                            .on_disabled_hover_text("Name a valid, applied shader to save it")
                            .clicked()
                        {
                            action = Some(Action::Save);
                        }
                    });
                    if let Some(saved) = &editor.saved {
                        ui.label(egui::RichText::new(saved).color(t.text_muted));
                    }
                }
                ui.separator();
                let label = ui.label("Ask a model");
                ui.add(
                    egui::TextEdit::multiline(&mut editor.request)
                        .char_limit(MAX_REQUEST_BYTES)
                        .desired_rows(2)
                        .desired_width(t.shader_editor_width)
                        .hint_text("Describe the filter, or how to change it"),
                )
                .labelled_by(label.id);
                // Asking again isn't blocked by a request still waiting: its run may have been
                // declined or failed, and the newest request is the one waited for.
                let waiting = editor.pending.is_some();
                ui.horizontal(|ui| {
                    let asked = !editor.request.trim().is_empty();
                    if ui
                        .add_enabled(asked && !busy, egui::Button::new("Write with a model"))
                        .on_hover_text("Add a text card asked for this shader; you see its price before it runs")
                        .clicked()
                    {
                        action = Some(Action::Write { fix: false });
                    }
                    if editor.message().is_some()
                        && ui
                            .add_enabled(!busy, egui::Button::new("Fix with a model"))
                            .on_hover_text("Add a text card asked to fix the shader for this message; you see its price before it runs")
                            .clicked()
                    {
                        action = Some(Action::Write { fix: true });
                    }
                });
                if waiting {
                    ui.label(egui::RichText::new("Waiting for the model's answer; run its card to get one.").color(t.text_muted));
                }
            });
        if !open {
            self.shader_editor = None;
            return;
        }
        match action {
            Some(Action::Store) => self.store_shader(),
            Some(Action::Revert) => {
                let node = editor.node;
                self.open_shader_editor(node);
            }
            Some(Action::Write { fix }) => self.write_shader(fix),
            Some(Action::Save) => self.save_shader(),
            Some(Action::Update) => self.update_shader(),
            None => {}
        }
    }

    /// Saves the card's shader to the library as the next version of the typed name, and records
    /// that version on the card.
    fn save_shader(&mut self) {
        let Some(editor) = &self.shader_editor else { return };
        let (canvas, node, name) = (editor.canvas, editor.node, editor.save_as.trim().to_owned());
        let result = self.session.dispatch("shader.save", &json!({"canvas": canvas, "id": node, "name": name})).map(|saved| {
            let (name, version) = (saved.get("name").cloned().unwrap_or_default(), saved.get("version").cloned().unwrap_or_default());
            let line = format!("{} v{version}", name.as_str().unwrap_or_default());
            let line = if saved.get("saved") == Some(&json!(true)) { format!("Saved as {line}.") } else { format!("{line} is this shader already.") };
            // The library has it either way; say so even if the card can't record where it came from.
            match self.session.dispatch("shader.update", &json!({"canvas": canvas, "id": node, "name": name, "version": version})) {
                Ok(_) => line,
                Err(error) => format!("{line} The card couldn't record it: {}", error.message),
            }
        });
        let library = self.library_shaders();
        // The Techniques panel lists the library again, so it shows this save too.
        self.library.reset();
        let Some(editor) = &mut self.shader_editor else { return };
        editor.library = library;
        match result {
            Ok(saved) => {
                editor.saved = Some(saved);
                editor.error = None;
            }
            Err(error) => {
                editor.saved = None;
                editor.error = Some(error.message);
            }
        }
    }

    /// Gives the card the latest version of the library shader it came from, and edits that.
    fn update_shader(&mut self) {
        let Some(editor) = &self.shader_editor else { return };
        let (canvas, node) = (editor.canvas, editor.node);
        match self.session.dispatch("shader.update", &json!({"canvas": canvas, "id": node})) {
            Ok(_) => self.open_shader_editor(node),
            Err(error) => {
                if let Some(editor) = &mut self.shader_editor {
                    editor.error = Some(error.message);
                }
            }
        }
    }

    fn store_shader(&mut self) {
        let Some(editor) = &mut self.shader_editor else { return };
        let params = json!({"canvas": editor.canvas, "id": editor.node, "source": editor.draft.source, "params": editor.draft.params});
        editor.error = self.session.dispatch("node.shader_set", &params).err().map(|error| error.message);
    }

    /// Adds a request card for the shader, and asks to run it (the price shows first).
    fn write_shader(&mut self, fix: bool) {
        let Some(editor) = &self.shader_editor else { return };
        let (canvas, node) = (editor.canvas, editor.node);
        let request = editor.request.trim();
        // The model sees the draft, which is what the message is about, not the stored shader.
        let mut params = json!({"canvas": canvas, "node": node, "shader": {"source": editor.draft.source, "params": editor.draft.params}});
        if !request.is_empty() {
            params["request"] = json!(clip(request, MAX_REQUEST_BYTES));
        }
        if fix && let Some(message) = editor.message() {
            params["error"] = json!(message);
        }
        let Some(card) = ids::node_id(self.session.document(), ("shader-write", node, self.session.revision())) else {
            self.notice = Some("Couldn't allocate a node id; try again.".into());
            return;
        };
        params["id"] = json!(card);
        let result = self.session.dispatch("shader.write", &params).and_then(|_| self.request_runs(vec![card], RunKind::Generate));
        let Some(editor) = &mut self.shader_editor else { return };
        match result {
            Ok(()) => {
                editor.pending = Some((card, None));
                editor.refusal = None;
                editor.error = None;
            }
            Err(error) => editor.error = Some(error.message),
        }
    }

    /// Once the pending request card has a new answer, stores it as the shader.
    fn shader_answered(&mut self) {
        let Some(editor) = &self.shader_editor else { return };
        let Some((card, before)) = editor.pending else { return };
        let canvas = editor.canvas;
        let Some(answering) = self.session.document().canvases().get(&canvas).and_then(|c| c.nodes().get(&card)) else {
            // The card went (undone or deleted): nothing to wait for.
            if let Some(editor) = &mut self.shader_editor {
                editor.pending = None;
            }
            return;
        };
        if answering.active.is_none() || answering.active == before {
            return;
        }
        if let Some(editor) = &mut self.shader_editor {
            editor.pending = None;
        }
        self.take_answer(card);
    }

    /// Card menu → Use as shader: stores a request card's answer on the shader card it was
    /// written for, and opens the editor there to show the result or why it was refused.
    pub(super) fn use_shader_answer(&mut self, card: NodeId, target: NodeId) {
        self.open_shader_editor(target);
        if self.shader_editor.as_ref().is_some_and(|editor| editor.node == target) {
            self.take_answer(card);
        }
    }

    /// Stores `card`'s answer as the editor's shader. A refused answer that still reads as a
    /// shader becomes the draft, so the compiler's message and a fix are about what the model
    /// wrote.
    fn take_answer(&mut self, card: NodeId) {
        let Some(editor) = &self.shader_editor else { return };
        let (canvas, node) = (editor.canvas, editor.node);
        let result = self.session.dispatch("shader.apply", &json!({"canvas": canvas, "id": node, "from": card}));
        let nodes = self.session.document().canvases().get(&canvas).map(|c| c.nodes());
        let shader = nodes.and_then(|nodes| nodes.get(&node)).map(stored);
        let reply = nodes.and_then(|nodes| nodes.get(&card)).and_then(|card| answer(card).ok()).map(parse_reply);
        let Some(editor) = &mut self.shader_editor else { return };
        editor.error = None;
        editor.refusal = None;
        match (result, shader) {
            (Ok(_), Some(Ok(shader))) => {
                editor.draft = shader;
                editor.recheck();
            }
            (Ok(_), _) => editor.error = Some("The answer was stored, but the card's shader can't be read back.".into()),
            (Err(error), _) => {
                let parsed = matches!(reply, Some(Ok(_)));
                if let Some(Ok(written)) = reply {
                    editor.draft = written;
                    editor.recheck();
                }
                // A written draft that fails its check already shows the compiler's message.
                if !(parsed && matches!(editor.check, Some(Err(_)))) {
                    editor.refusal = Some(error.message);
                }
            }
        }
    }
}

/// The declared parameters' rows; whether any changed.
fn params(ui: &mut egui::Ui, params: &mut Vec<StoredParam>, t: &Tokens) -> bool {
    let mut changed = false;
    let mut remove = None;
    egui::Grid::new("shader-params").num_columns(6).spacing([t.control_gap, t.control_gap]).show(ui, |ui| {
        for heading in ["Name", "Label", "Min", "Max", "Default", ""] {
            ui.label(egui::RichText::new(heading).color(t.text_muted));
        }
        ui.end_row();
        for (n, param) in params.iter_mut().enumerate() {
            changed |= ui.add(egui::TextEdit::singleline(&mut param.name).char_limit(32).desired_width(t.shader_name_width)).changed();
            changed |= ui.add(egui::TextEdit::singleline(&mut param.label).char_limit(64).desired_width(t.shader_name_width)).changed();
            for value in [&mut param.min, &mut param.max, &mut param.default] {
                changed |= ui.add(egui::DragValue::new(value).speed(0.01).max_decimals(4)).changed();
            }
            if ui.button("Remove").on_hover_text("Stop declaring this parameter").clicked() {
                remove = Some(n);
            }
            ui.end_row();
        }
    });
    if let Some(n) = remove
        && n < params.len()
    {
        params.remove(n);
        changed = true;
    }
    let room = params.len() < MAX_PARAMS;
    if ui.add_enabled(room, egui::Button::new("Add parameter")).on_disabled_hover_text("A shader declares at most 16 parameters").clicked() {
        let name = (1..=MAX_PARAMS).map(|n| format!("value{n}")).find(|name| params.iter().all(|p| p.name != *name)).unwrap_or_default();
        params.push(StoredParam { label: "Value".into(), name, min: 0.0, max: 1.0, default: 0.5 });
        changed = true;
    }
    changed
}
