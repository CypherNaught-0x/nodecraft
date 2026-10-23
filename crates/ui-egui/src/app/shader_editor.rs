//! The shader editor (#90): a window over a custom shader card where its WGSL and declared
//! parameters are edited, checked as they are typed, and stored with `node.shader_set`. A text
//! model can write or fix the shader: `shader.write` adds a request card, which is quoted and run
//! like any other, and once it has an answer `shader.apply` stores it, or says why not, so the
//! model can be asked to fix that too.
use super::NodeCraftApp;
use super::generate::RunKind;
use crate::ids;
use crate::theme::Tokens;
use nodecraft_engine::shader_assist_cmds::{MAX_ERROR_BYTES, MAX_REQUEST_BYTES};
use nodecraft_gpu::shader::{MAX_PARAMS, MAX_SOURCE, ShaderParam, ShaderSpec, validate};
use nodecraft_nodes::shader::{StoredParam, StoredShader, stored};
use nodecraft_types::{CanvasId, GenerationId, NodeId};
use serde_json::json;

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
    /// Why the last store or answer was refused.
    pub(super) error: Option<String>,
}

impl ShaderEditor {
    fn new(canvas: CanvasId, node: NodeId, shader: StoredShader) -> Self {
        let mut editor = Self { canvas, node, draft: shader, check: None, request: String::new(), pending: None, error: None };
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

    /// The compiler's message for the draft, cut to what a fix request carries.
    fn message(&self) -> Option<String> {
        match &self.check {
            Some(Err(message)) => Some(clip(message, MAX_ERROR_BYTES)),
            _ => self.error.as_ref().map(|message| clip(message, MAX_ERROR_BYTES)),
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
    Write { fix: bool },
}

impl NodeCraftApp {
    pub(super) fn open_shader_editor(&mut self, node: NodeId) {
        let Some(card) = self.session.document().canvases().get(&self.canvas).and_then(|c| c.nodes().get(&node)) else { return };
        match stored(card) {
            Ok(shader) => self.shader_editor = Some(ShaderEditor::new(self.canvas, node, shader)),
            Err(error) => self.notice = Some(error.message),
        }
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
        let Some(editor) = &mut self.shader_editor else { return };
        let mut open = true;
        let mut action = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        let top = t.navigation_margin + t.navigation_toolbar_height + t.navigation_gap;
        egui::Window::new("Custom shader")
            .id(egui::Id::new("nodecraft.shader_editor"))
            .open(&mut open)
            .constrain_to(self.viewport)
            // Opens at the right, below the toolbar, leaving the canvas and its card in view.
            .pivot(egui::Align2::RIGHT_TOP)
            .default_pos(self.viewport.right_top() + egui::Vec2::new(-t.navigation_margin, top))
            .default_width(t.shader_editor_width)
            .collapsible(false)
            .frame(frame)
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
                });
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
                let waiting = editor.pending.is_some();
                ui.horizontal(|ui| {
                    let asked = !editor.request.trim().is_empty();
                    if ui
                        .add_enabled(asked && !waiting && !busy, egui::Button::new("Write with a model"))
                        .on_hover_text("Add a text card asked for this shader; you see its price before it runs")
                        .clicked()
                    {
                        action = Some(Action::Write { fix: false });
                    }
                    if editor.message().is_some()
                        && ui
                            .add_enabled(!waiting && !busy, egui::Button::new("Fix with a model"))
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
            None => {}
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
        let mut params = json!({"canvas": canvas, "node": node});
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
                editor.error = None;
            }
            Err(error) => editor.error = Some(error.message),
        }
    }

    /// Once the pending request card has a new answer, stores it as the shader.
    fn shader_answered(&mut self) {
        let Some(editor) = &self.shader_editor else { return };
        let Some((card, before)) = editor.pending else { return };
        let (canvas, node) = (editor.canvas, editor.node);
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
        let result = self.session.dispatch("shader.apply", &json!({"canvas": canvas, "id": node, "from": card}));
        let shader = self.session.document().canvases().get(&canvas).and_then(|c| c.nodes().get(&node)).map(stored);
        let Some(editor) = &mut self.shader_editor else { return };
        editor.pending = None;
        match (result, shader) {
            (Ok(_), Some(Ok(shader))) => {
                editor.draft = shader;
                editor.recheck();
                editor.error = None;
            }
            (Err(error), _) => editor.error = Some(error.message),
            (Ok(_), _) => editor.error = Some("The answer was stored, but the card's shader can't be read back.".into()),
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
