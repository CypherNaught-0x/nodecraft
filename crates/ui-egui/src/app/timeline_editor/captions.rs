//! The Timeline editor's captions (#574): a list to add, pick, enable and delete them, the picked
//! one's fields, and the ones the playhead is in drawn over the preview. Each change is one
//! `timeline.caption_*` command, so one undo step.
use super::{Commands, TimelineEditor, duration, micros, seconds};
use crate::app::layer_editor::{colour_field, number, text_field};
use crate::theme::Tokens;
use egui::{Color32, Pos2, Rect};
use nodecraft_nodes::timeline::caption::{self, Align, Caption, Position};
use nodecraft_nodes::timeline::{MAX_CAPTIONS, MAX_US};
use serde_json::{Value as Json, json};

/// A new caption shows this long from the playhead.
const NEW_US: u64 = 2_000_000;

fn on(editor: &TimelineEditor, id: &str) -> Json {
    json!({"canvas": editor.canvas, "id": editor.node, "caption": id})
}

fn update(editor: &TimelineEditor, id: &str, set: Json) -> (&'static str, Json) {
    let mut params = on(editor, id);
    params["set"] = set;
    ("timeline.caption_update", params)
}

/// The draft key for a caption's field; `held` reads the caption back from one.
fn key(id: &str, field: &str) -> String {
    format!("caption:{id}.{field}")
}

pub(super) fn held(draft: Option<&(String, String)>) -> Option<String> {
    draft.and_then(|(key, _)| key.strip_prefix("caption:")?.split_once('.').map(|(id, _)| id.to_owned()))
}

/// The caption's first line, for lists and labels.
fn title(c: &Caption) -> &str {
    c.text.lines().next().unwrap_or("")
}

/// The Captions section of the clips panel.
pub(super) fn list(ui: &mut egui::Ui, editor: &mut TimelineEditor, captions: &[Caption], t: &Tokens, commands: &mut Commands) {
    ui.separator();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Captions").color(t.text_muted));
        let room = captions.len() < MAX_CAPTIONS;
        let add = ui.add_enabled(room, egui::Button::new("Add caption").small());
        let add = if room { add } else { add.on_disabled_hover_text("A timeline has at most 64 captions") };
        if add.clicked() {
            let id = Caption::fresh_id(captions);
            let start = editor.playhead_us.min(MAX_US.saturating_sub(NEW_US));
            let mut params = on(editor, &id);
            for (field, value) in [("text", json!("Caption")), ("start_us", json!(start)), ("end_us", json!(start.saturating_add(NEW_US)))] {
                params[field] = value;
            }
            commands.push(("timeline.caption_add", params));
            editor.selected = None;
            editor.caption = Some(id);
        }
    });
    for c in captions {
        ui.horizontal(|ui| {
            let label = title(c);
            let mut enabled = c.enabled;
            let r = ui.checkbox(&mut enabled, "").on_hover_text("Draw in the output");
            r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, enabled, format!("Show caption {label}")));
            if r.changed() {
                commands.push(update(editor, &c.id, json!({"enabled": enabled})));
            }
            let text = if c.enabled { egui::RichText::new(label) } else { egui::RichText::new(label).color(t.text_muted) };
            if ui.selectable_label(editor.caption.as_ref() == Some(&c.id), text).clicked() {
                editor.selected = None;
                editor.caption = Some(c.id.clone());
            }
            let when = format!("{} – {}", duration(c.start_us), duration(c.end_us));
            ui.label(egui::RichText::new(when).color(t.text_muted).small());
        });
    }
}

/// The picked caption's fields; false when it isn't there.
pub(super) fn properties(ui: &mut egui::Ui, editor: &mut TimelineEditor, captions: &[Caption], id: &str, t: &Tokens, commands: &mut Commands) -> bool {
    let Some(c) = captions.iter().find(|c| c.id == id) else {
        // Its caption went (an undo, an agent): the edit has nothing left to apply to.
        if held(editor.draft.as_ref()).as_deref() == Some(id) {
            editor.draft = None;
        }
        return false;
    };
    ui.label(egui::RichText::new(format!("Caption · {}", title(c))).strong());
    if let Some(text) = text_field(ui, &mut editor.draft, &key(id, "text"), "Text", &c.text, true)
        && !text.trim().is_empty()
    {
        commands.push(update(editor, id, json!({"text": text})));
    }
    let most = seconds(MAX_US);
    let step = seconds(super::MIN_US);
    egui::Grid::new("timeline_caption").num_columns(2).show(ui, |ui| {
        if let Some(v) = number(ui, &mut editor.draft, &key(id, "start"), "Starts at (s)", seconds(c.start_us), 0.0..=seconds(c.end_us) - step, 0.05, 2) {
            commands.push(update(editor, id, json!({"start_us": micros(v).min(c.end_us.saturating_sub(1))})));
        }
        ui.end_row();
        if let Some(v) = number(ui, &mut editor.draft, &key(id, "end"), "Ends at (s)", seconds(c.end_us), seconds(c.start_us) + step..=most, 0.05, 2) {
            commands.push(update(editor, id, json!({"end_us": micros(v).max(c.start_us.saturating_add(1)).min(MAX_US)})));
        }
        ui.end_row();
        let (low, high) = (f64::from(caption::MIN_CAPTION_SIZE), f64::from(caption::MAX_CAPTION_SIZE));
        if let Some(v) = number(ui, &mut editor.draft, &key(id, "size"), "Size (px)", f64::from(c.size), low..=high, 1.0, 0) {
            commands.push(update(editor, id, json!({"size": v.round().clamp(low, high) as u32})));
        }
        ui.end_row();
        ui.label("Across");
        ui.horizontal(|ui| {
            for (choice, word, label) in [(Align::Left, "left", "Left"), (Align::Center, "center", "Centre"), (Align::Right, "right", "Right")] {
                if ui.selectable_label(c.align == choice, label).clicked() && c.align != choice {
                    commands.push(update(editor, id, json!({"align": word})));
                }
            }
        });
        ui.end_row();
        ui.label("Up and down");
        ui.horizontal(|ui| {
            for (choice, word, label) in [(Position::Top, "top", "Top"), (Position::Middle, "middle", "Middle"), (Position::Bottom, "bottom", "Bottom")] {
                if ui.selectable_label(c.position == choice, label).clicked() && c.position != choice {
                    commands.push(update(editor, id, json!({"position": word})));
                }
            }
        });
        ui.end_row();
        let widest = f64::from(caption::MAX_CAPTION_STROKE);
        if let Some(v) = number(ui, &mut editor.draft, &key(id, "stroke_width"), "Outline (px)", f64::from(c.stroke_width), 0.0..=widest, 0.5, 0) {
            commands.push(update(editor, id, json!({"stroke_width": v.round().clamp(0.0, widest) as u32})));
        }
        ui.end_row();
    });
    for (field, label, value) in [("color", "Colour", &c.color), ("stroke", "Outline colour", &c.stroke)] {
        if let Some(Some(v)) = colour_field(ui, &mut editor.draft, &key(id, field), label, Some(value), t)
            && caption::valid_color(&v)
        {
            commands.push(update(editor, id, json!({field: v})));
        }
    }
    if let Some(font) = text_field(ui, &mut editor.draft, &key(id, "font"), "Font", &c.font, false)
        && !font.trim().is_empty()
    {
        commands.push(update(editor, id, json!({"font": font.trim()})));
    }
    ui.horizontal_wrapped(|ui| {
        if ui.button("Start at playhead").clicked() && editor.playhead_us < c.end_us {
            commands.push(update(editor, id, json!({"start_us": editor.playhead_us})));
        }
        if ui.button("End at playhead").clicked() && editor.playhead_us > c.start_us {
            commands.push(update(editor, id, json!({"end_us": editor.playhead_us.min(MAX_US)})));
        }
        if ui.button("Delete caption").clicked() {
            commands.push(("timeline.caption_remove", on(editor, id)));
            editor.caption = None;
        }
    });
    let help = "Drawn over the picture from its start to its end, a twentieth of the frame from the edge it sits by; past the output's end it is cut. Lato draws any font NodeCraft doesn't have.";
    ui.label(egui::RichText::new(help).color(t.text_muted).small());
    true
}

/// A rough look at the captions the playhead is in, over a `size` output frame fitted in
/// `area`: the render draws them exactly.
pub(super) fn preview(painter: &egui::Painter, area: Rect, size: [f32; 2], captions: &[Caption], playhead_us: u64) {
    let scale = (area.width() / size[0].max(1.0)).min(area.height() / size[1].max(1.0));
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let frame = Rect::from_center_size(area.center(), egui::vec2(size[0], size[1]) * scale);
    // Lines break only at newlines, as in the render, and are cut at the frame's edge.
    let painter = painter.with_clip_rect(frame);
    for c in captions.iter().filter(|c| c.enabled && c.start_us <= playhead_us && playhead_us < c.end_us) {
        let colour = |text: &str, fallback: Color32| match nodecraft_actions::layers::parse_color(text) {
            Ok([r, g, b, a]) => Color32::from_rgba_unmultiplied(r, g, b, a),
            Err(_) => fallback,
        };
        let size = (c.size as f32 * scale).max(1.0);
        let margin = egui::vec2(frame.width(), frame.height()) / 20.0;
        let (ax, x) = match c.align {
            Align::Left => (egui::Align::Min, frame.min.x + margin.x),
            Align::Center => (egui::Align::Center, frame.center().x),
            Align::Right => (egui::Align::Max, frame.max.x - margin.x),
        };
        let (ay, y) = match c.position {
            Position::Top => (egui::Align::Min, frame.min.y + margin.y),
            Position::Middle => (egui::Align::Center, frame.center().y),
            Position::Bottom => (egui::Align::Max, frame.max.y - margin.y),
        };
        let fill = colour(&c.color, Color32::WHITE);
        let mut job = egui::text::LayoutJob::simple(c.text.clone(), egui::FontId::proportional(size), fill, f32::INFINITY);
        job.halign = ax;
        let galley = painter.layout_job(job);
        let top_left = egui::Align2([ax, ay]).anchor_size(Pos2::new(x, y), galley.size()).min;
        // The galley lays out from its anchor: left, middle or right of its own width.
        let origin = Pos2::new(
            match ax {
                egui::Align::Min => top_left.x,
                egui::Align::Center => top_left.x + galley.size().x / 2.0,
                egui::Align::Max => top_left.x + galley.size().x,
            },
            top_left.y,
        );
        if c.stroke_width > 0 {
            let outline = colour(&c.stroke, Color32::BLACK);
            let reach = (c.stroke_width as f32 * scale).max(1.0);
            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0), (-0.7, -0.7), (0.7, -0.7), (-0.7, 0.7), (0.7, 0.7)] {
                painter.galley_with_override_text_color(origin + egui::vec2(dx, dy) * reach, galley.clone(), outline);
            }
        }
        painter.galley(origin, galley, fill);
    }
}
