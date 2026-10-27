//! Choosing the visible fields of an existing ComfyUI workflow card (#947).
use super::NodeCraftApp;
use crate::theme::Tokens;
use egui::{Context, Ui};
use nodecraft_engine::{Session, session::DocumentLineage};
use nodecraft_nodes::comfy_workflow::{self, OutputName};
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Media,
    Parameters,
    Results,
}
#[derive(Deserialize)]
struct Input {
    node: String,
    input: String,
    name: String,
    label: String,
    kind: String,
    media: Option<Modality>,
    selected: bool,
}
#[derive(Deserialize)]
struct Output {
    node: String,
    label: String,
    media: Modality,
    selected: bool,
}
#[derive(Deserialize)]
struct Fields {
    binding: String,
    inputs: Vec<Input>,
    outputs: Vec<Output>,
    input_order: Vec<[String; 2]>,
    output_order: Vec<String>,
    output_names: Vec<OutputName>,
    ports: Vec<nodecraft_graph::Port>,
    history: usize,
}
pub(super) struct Editor {
    canvas: CanvasId,
    node: NodeId,
    lineage: DocumentLineage,
    fields: Option<Fields>,
    error: Option<String>,
    tab: Tab,
    filter: String,
    disconnect: bool,
    clear_history: bool,
}
fn invalid() -> Error {
    Error::new(ErrorKind::Internal, "workflow fields could not be read")
}
impl Editor {
    fn load(&mut self, session: &mut Session) {
        let loaded = session
            .dispatch("comfy.workflow.mapping", &json!({"canvas":self.canvas,"id":self.node}))
            .and_then(|value| serde_json::from_value::<Fields>(value).map_err(|_| invalid()));
        match loaded {
            Err(error) => self.error = Some(error.message),
            Ok(mut fields) => {
                fields
                    .inputs
                    .sort_by_key(|i| fields.input_order.iter().position(|k| k.first() == Some(&i.node) && k.get(1) == Some(&i.input)).unwrap_or(usize::MAX));
                fields.outputs.sort_by_key(|o| fields.output_order.iter().position(|id| id == &o.node).unwrap_or(usize::MAX));
                for output in &fields.outputs {
                    if !fields.output_names.iter().any(|p| p.modality == output.media) {
                        let name = fields
                            .ports
                            .iter()
                            .find(|p| p.port_type.modality == output.media)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| default_port(output.media).into());
                        fields.output_names.push(OutputName { modality: output.media, name });
                    }
                }
                self.fields = Some(fields);
                self.error = None;
            }
        }
    }
    fn request(&self) -> Result<Value, Error> {
        let f = self.fields.as_ref().ok_or_else(invalid)?;
        Ok(json!({"canvas":self.canvas,"id":self.node,"binding":f.binding,
            "inputs":f.inputs.iter().filter(|i|i.selected).map(|i|json!({"node":i.node,"input":i.input,"name":i.name})).collect::<Vec<_>>(),
            "outputs":f.outputs.iter().filter(|o|o.selected).map(|o|json!({"node":o.node,"label":o.label})).collect::<Vec<_>>(),
            "output_names":f.output_names.iter().filter(|p|p.name!=default_port(p.modality)).collect::<Vec<_>>(),
            "disconnect":self.disconnect,"clear_history":self.clear_history}))
    }
}
fn default_port(media: Modality) -> &'static str {
    match media {
        Modality::Video => "video",
        Modality::Audio => "audio",
        Modality::Mesh => "mesh",
        _ => "image",
    }
}
fn media_label(media: Modality) -> &'static str {
    match media {
        Modality::Video => "Video",
        Modality::Audio => "Audio",
        Modality::Mesh => "3D",
        _ => "Image",
    }
}
fn matches_filter(filter: &str, texts: &[&str]) -> bool {
    filter.is_empty() || texts.iter().any(|s| s.to_lowercase().contains(filter))
}
fn move_row<T>(rows: &mut [T], from: usize, to: usize) {
    if from < to {
        if let Some(slice) = rows.get_mut(from..=to) {
            slice.rotate_left(1);
        }
    } else if let Some(slice) = rows.get_mut(to..=from) {
        slice.rotate_right(1);
    }
}
fn arrows(ui: &mut Ui, key: &str, up: bool, down: bool) -> Option<bool> {
    let mut moved = None;
    let button = ui.add_enabled(up, egui::Button::new("Up").small());
    button.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, up, format!("Move {key} up")));
    if button.clicked() {
        moved = Some(true);
    }
    let button = ui.add_enabled(down, egui::Button::new("Down").small());
    button.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, down, format!("Move {key} down")));
    if button.clicked() {
        moved = Some(false);
    }
    moved
}
fn inputs(ui: &mut Ui, f: &mut Fields, tab: Tab, filter: &str, t: &Tokens) {
    let indices = f
        .inputs
        .iter()
        .enumerate()
        .filter(|(_, i)| (i.media.is_some()) == (tab == Tab::Media) && matches_filter(filter, &[&i.node, &i.input, &i.label, &i.name]))
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let row_height = ui.spacing().interact_size.y * 2.0 + ui.text_style_height(&egui::TextStyle::Small) + t.label_gap * 3.0;
    let mut moved = None;
    egui::ScrollArea::vertical().id_salt("workflow_inputs").max_height((ui.ctx().content_rect().height() * 0.45).min(t.card_max_height * 0.6)).show_rows(
        ui,
        row_height,
        indices.len(),
        |ui, range| {
            for place in range {
                let Some(index) = indices.get(place).copied() else { continue };
                let Some(row) = f.inputs.get_mut(index) else { continue };
                let key = format!("{}.{}", row.node, row.input);
                ui.push_id(&key, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut row.selected, format!("Show {key}"));
                        if let Some(up) = arrows(ui, &key, place > 0, place.saturating_add(1) < indices.len()) {
                            let destination = if up { place.checked_sub(1) } else { place.checked_add(1) }.and_then(|i| indices.get(i)).copied();
                            if let Some(to) = destination {
                                moved = Some((index, to));
                            }
                        }
                    });
                    let detail = format!("{} · {}", row.label, row.kind);
                    let label = ui.add(egui::Label::new(egui::RichText::new(&detail).small()).truncate()).on_hover_text(&detail);
                    ui.add(egui::TextEdit::singleline(&mut row.name).char_limit(256).desired_width(f32::INFINITY)).labelled_by(label.id);
                });
            }
        },
    );
    if let Some((from, to)) = moved {
        move_row(&mut f.inputs, from, to);
    }
}
fn outputs(ui: &mut Ui, f: &mut Fields, filter: &str, t: &Tokens) {
    ui.label("Results of each media type share a list output. Their order follows this list.");
    for port in &mut f.output_names {
        if !f.outputs.iter().any(|o| o.selected && o.media == port.modality) {
            continue;
        }
        ui.horizontal(|ui| {
            let label = ui.label(format!("{} list port", media_label(port.modality)));
            ui.add(egui::TextEdit::singleline(&mut port.name).char_limit(128).desired_width(ui.available_width())).labelled_by(label.id);
        });
    }
    let indices = f.outputs.iter().enumerate().filter(|(_, o)| matches_filter(filter, &[&o.node, &o.label])).map(|(i, _)| i).collect::<Vec<_>>();
    let row_height = ui.spacing().interact_size.y * 2.0 + t.label_gap * 2.0;
    let mut moved = None;
    egui::ScrollArea::vertical().id_salt("workflow_outputs").max_height((ui.ctx().content_rect().height() * 0.45).min(t.card_max_height * 0.6)).show_rows(
        ui,
        row_height,
        indices.len(),
        |ui, range| {
            for place in range {
                let Some(index) = indices.get(place).copied() else { continue };
                let Some(row) = f.outputs.get_mut(index) else { continue };
                ui.push_id(&row.node, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut row.selected, format!("Show result {}", row.node));
                        if let Some(up) = arrows(ui, &format!("result {}", row.node), place > 0, place.saturating_add(1) < indices.len()) {
                            let destination = if up { place.checked_sub(1) } else { place.checked_add(1) }.and_then(|i| indices.get(i)).copied();
                            if let Some(to) = destination {
                                moved = Some((index, to));
                            }
                        }
                    });
                    let label = ui.label(format!("{} result", media_label(row.media)));
                    ui.add(egui::TextEdit::singleline(&mut row.label).char_limit(256).desired_width(f32::INFINITY)).labelled_by(label.id);
                });
            }
        },
    );
    if let Some((from, to)) = moved {
        move_row(&mut f.outputs, from, to);
    }
}
impl NodeCraftApp {
    pub(super) fn open_comfy_mapping(&mut self, node: NodeId) {
        let mut editor = Editor {
            canvas: self.canvas,
            node,
            lineage: self.session.document_lineage(),
            fields: None,
            error: None,
            tab: Tab::Parameters,
            filter: String::new(),
            disconnect: false,
            clear_history: false,
        };
        editor.load(&mut self.session);
        self.comfy_mapping = Some(editor);
    }
    pub(super) fn comfy_mapping_window(&mut self, ctx: &Context, t: &Tokens) {
        let Some(mut editor) = self.comfy_mapping.take() else { return };
        if !self.session.is_current_document(&editor.lineage)
            || !self.session.document().canvases().get(&editor.canvas).and_then(|c| c.nodes().get(&editor.node)).is_some_and(|n| n.kind == comfy_workflow::ID)
        {
            return;
        }
        if let Some(fields) = &mut editor.fields {
            fields.history = self.session.document().canvases().get(&editor.canvas).and_then(|c| c.nodes().get(&editor.node)).map_or(0, |n| n.history.len());
        }
        let mut open = true;
        let (mut apply, mut reload, mut read, mut cancel) = (false, false, false, false);
        egui::Window::new("Workflow fields")
            .id(egui::Id::new(("comfy_mapping", editor.canvas, editor.node)))
            .open(&mut open)
            .collapsible(false)
            .order(egui::Order::Foreground)
            .default_pos(ctx.content_rect().center() - egui::vec2(t.dialog_width * 0.75, t.card_max_height * 0.3))
            .resizable(true)
            .default_width(t.dialog_width * 1.5)
            .show(ctx, |ui| {
                ui.label("Choose what appears on this card.");
                ui.horizontal_wrapped(|ui| {
                    for (tab, label) in [(Tab::Media, "Media inputs"), (Tab::Parameters, "Parameters"), (Tab::Results, "Results")] {
                        if ui.selectable_label(editor.tab == tab, label).clicked() {
                            editor.tab = tab;
                        }
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut editor.filter).hint_text("Filter fields").desired_width(f32::INFINITY));
                let filter = editor.filter.trim().to_lowercase();
                if let Some(fields) = &mut editor.fields {
                    match editor.tab {
                        Tab::Results => outputs(ui, fields, &filter, t),
                        tab => inputs(ui, fields, tab, &filter, t),
                    }
                    ui.checkbox(&mut editor.disconnect, "Disconnect wires on hidden fields");
                    if fields.history > 0 {
                        ui.checkbox(&mut editor.clear_history, "Clear history if result media types change (Undo restores it)");
                    }
                }
                if let Some(error) = &editor.error {
                    ui.colored_label(t.danger, error);
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(editor.fields.is_some(), egui::Button::new("Apply fields")).clicked() {
                        apply = true;
                    }
                    if ui.button("Reload fields").clicked() {
                        reload = true;
                    }
                    if (editor.fields.is_none() || editor.error.is_some()) && ui.button("Read server nodes").clicked() {
                        read = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            open = false;
        }
        if read {
            let server = self
                .session
                .document()
                .canvases()
                .get(&editor.canvas)
                .and_then(|c| c.nodes().get(&editor.node))
                .and_then(|n| n.config.get(nodecraft_engine::provider_setup::COMFY_SERVER_CONFIG));
            let request = match server {
                Some(nodecraft_types::Value::Text(name)) => json!({"server":name}),
                _ => json!({}),
            };
            match self.session.dispatch("comfy.nodes.refresh", &request) {
                Ok(_) => editor.error = Some("Reading server nodes. Reload these fields when the check finishes.".into()),
                Err(error) => editor.error = Some(error.message),
            }
        }
        if reload {
            editor.load(&mut self.session);
        }
        if apply {
            match editor.request().and_then(|request| self.session.dispatch("comfy.workflow.mapping_set", &request)) {
                Ok(_) => {
                    open = false;
                    self.notice = Some("Workflow fields saved. Undo restores the previous mapping.".into());
                }
                Err(error) => editor.error = Some(error.message),
            }
        }
        if open {
            self.comfy_mapping = Some(editor);
        }
    }
}
#[cfg(test)]
mod tests;
