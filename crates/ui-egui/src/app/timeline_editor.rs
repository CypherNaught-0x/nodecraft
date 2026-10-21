//! The Timeline editor (#109): a full-screen view of a Timeline card's clips on one track, where
//! they are trimmed, reordered, split, faded and muted. It changes the document only through
//! commands: each finished drag is one `timeline.update` or `timeline.move`, each field one
//! `timeline.update`, so every gesture is one undo step. Lengths and thumbnails come from the
//! shell's video player, which probes each wired video with the settings' tools; a clip whose
//! length isn't known yet takes a stand-in width. Rendering is the card's run, as for any node.
use super::NodeCraftApp;
use super::layer_editor::{colour_field, number, param_json};
use crate::theme::Tokens;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use nodecraft_nodes::timeline::{self, Clip};
use nodecraft_types::{AssetRef, CanvasId, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::HashMap;

/// A clip is trimmed no shorter than this.
const MIN_US: u64 = 100_000;
/// The track's stand-in for a clip whose length isn't known yet.
const UNKNOWN_US: u64 = 5_000_000;
/// Trims land on whole milliseconds.
const STEP_US: i128 = 1000;

pub(super) struct TimelineEditor {
    canvas: CanvasId,
    pub(super) node: NodeId,
    pub(super) selected: Option<String>,
    drag: Option<Drag>,
    /// `timeline.list` as of a document revision.
    listing: Option<(u64, Listing)>,
    /// A field being typed or dragged, applied when it lets go: (field key, text).
    draft: Option<(String, String)>,
    /// A field had focus last frame (see the Layer Editor's `typing`).
    typing: bool,
    /// Where the playhead is, in microseconds of output.
    pub(super) playhead_us: u64,
    /// Screen points per second of output, and where the track's time 0 is, as last drawn.
    pub(super) scale: f32,
    pub(super) origin: Pos2,
}

impl TimelineEditor {
    pub(super) fn new(canvas: CanvasId, node: NodeId) -> Self {
        Self { canvas, node, selected: None, drag: None, listing: None, draft: None, typing: false, playhead_us: 0, scale: 1.0, origin: Pos2::ZERO }
    }
}

/// One listed clip and what the editor knows of its video.
#[derive(Clone, Debug)]
struct Shown {
    clip: Clip,
    /// Its video is wired in now.
    wired: bool,
    /// The video's length, from its asset or the player's probe.
    video_us: Option<u64>,
    video: Option<AssetRef>,
}

impl Shown {
    fn playing(&self) -> bool {
        self.clip.enabled && self.wired
    }

    /// Where the clip ends in its video, when known.
    fn end_us(&self) -> Option<u64> {
        self.clip.out_us.or(self.video_us)
    }
}

#[derive(Deserialize)]
struct Input {
    origin: String,
    video: Option<AssetRef>,
}

struct Listing {
    clips: Vec<Shown>,
    /// The nodes videos are wired in from, in wire order.
    origins: Vec<String>,
}

/// `timeline.list`'s answer: each clip with its video, if it is wired in.
fn parse(listed: Json) -> Option<Listing> {
    let inputs: Vec<Input> = serde_json::from_value(listed.get("inputs")?.clone()).ok()?;
    let mut out = Vec::new();
    for entry in listed.get("clips")?.as_array()? {
        let mut entry = entry.clone();
        let fields = entry.as_object_mut()?;
        let wired = fields.remove("wired").and_then(|w| w.as_bool()).unwrap_or(false);
        let video_us = fields.remove("video_us").and_then(|v| v.as_u64());
        let clip: Clip = serde_json::from_value(entry).ok()?;
        let video = usize::try_from(clip.nth)
            .ok()
            .and_then(|nth| inputs.iter().filter(|i| i.origin == clip.origin).nth(nth))
            .and_then(|i| i.video.clone())
            .filter(|_| wired);
        out.push(Shown { clip, wired, video_us, video });
    }
    let mut origins: Vec<String> = Vec::new();
    for input in inputs {
        if !origins.contains(&input.origin) {
            origins.push(input.origin);
        }
    }
    Some(Listing { clips: out, origins })
}

/// A playing clip's place on the track.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    /// Its index among the listed clips.
    index: usize,
    start_us: u64,
    length_us: u64,
    /// False while the length is the stand-in.
    known: bool,
    /// How much of its end the next clip crosses into.
    overlap_us: u64,
}

/// The playing clips one after another, each crossing `transition_us` into the next (no more
/// than either is long), and the output's length.
fn place(clips: &[Shown]) -> (Vec<Placed>, u64) {
    let mut placed: Vec<Placed> = clips
        .iter()
        .enumerate()
        .filter(|(_, s)| s.playing())
        .map(|(index, s)| {
            let length = s.end_us().map(|end| end.saturating_sub(s.clip.in_us));
            Placed { index, start_us: 0, length_us: length.unwrap_or(UNKNOWN_US).max(1), known: length.is_some(), overlap_us: 0 }
        })
        .collect();
    let mut at = 0u64;
    for k in 0..placed.len() {
        let next = placed.get(k.saturating_add(1)).map(|p| p.length_us);
        let Some(p) = placed.get_mut(k) else { break };
        p.start_us = at;
        let transition = clips.get(p.index).map_or(0, |s| s.clip.transition_us);
        p.overlap_us = next.map_or(0, |next| transition.min(p.length_us).min(next));
        at = at.saturating_add(p.length_us).saturating_sub(p.overlap_us);
    }
    (placed, at)
}

/// The clip under output time `us` and how far into its video that is, when strictly inside it.
fn cut_at(clips: &[Shown], placed: &[Placed], us: u64) -> Option<(usize, u64)> {
    placed.iter().rev().find(|p| p.start_us < us && us < p.start_us.saturating_add(p.length_us)).and_then(|p| {
        let clip = &clips.get(p.index)?.clip;
        Some((p.index, clip.in_us.saturating_add(us.saturating_sub(p.start_us))))
    })
}

fn seconds(us: u64) -> f64 {
    us as f64 / 1e6
}

/// Seconds a person typed or dragged, as microseconds within an hour.
fn micros(s: f64) -> u64 {
    if s.is_finite() && s > 0.0 { (s * 1e6).round().min(timeline::MAX_US as f64) as u64 } else { 0 }
}

/// `1.5 s`, or `1:02.5` from a minute up.
fn duration(us: u64) -> String {
    let tenths = us / 100_000;
    let (minutes, s, tenth) = (tenths / 600, tenths / 10 % 60, tenths % 10);
    if minutes > 0 { format!("{minutes}:{s:02}.{tenth}") } else { format!("{s}.{tenth} s") }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grip {
    Move,
    Start,
    End,
}

struct Drag {
    clip: String,
    grip: Grip,
    /// Where the pointer went down, in screen points.
    from_x: f32,
    before: Clip,
    video_us: Option<u64>,
    now: Clip,
    /// For a move: the index in playing order it would land at.
    to: Option<usize>,
}

impl Drag {
    /// Trims by the pointer's travel since it went down, `dx` points at `scale` points a second.
    fn trim(&mut self, dx: f32, scale: f32) {
        self.now = self.before.clone();
        let travel = if scale > 0.0 && dx.is_finite() { f64::from(dx) / f64::from(scale) * 1e6 } else { 0.0 };
        let dt = (travel.round() as i128 / STEP_US) * STEP_US;
        let limit = i128::from(self.video_us.unwrap_or(timeline::MAX_US));
        let at = |t: u64| i128::from(t).saturating_add(dt);
        let fit = |v: i128, lo: i128, hi: i128| u64::try_from(v.clamp(lo, hi.max(lo))).unwrap_or(0);
        match self.grip {
            Grip::Start => {
                let end = i128::from(self.before.out_us.or(self.video_us).unwrap_or(timeline::MAX_US));
                self.now.in_us = fit(at(self.before.in_us), 0, end - i128::from(MIN_US));
            }
            Grip::End => {
                if let Some(end) = self.before.out_us.or(self.video_us) {
                    let out = fit(at(end), i128::from(self.before.in_us.saturating_add(MIN_US)), limit);
                    // Back to the video's end plays to its end, wherever that is.
                    self.now.out_us = (Some(out) != self.video_us).then_some(out);
                }
            }
            Grip::Move => {}
        }
        let length = self.now.out_us.or(self.video_us).map(|end| end.saturating_sub(self.now.in_us));
        // Fades shrink with the clip, as `Clip::check` requires.
        if let Some(length) = length
            && self.now.fade_in_us.saturating_add(self.now.fade_out_us) > length
        {
            self.now.fade_in_us = self.now.fade_in_us.min(length / 2);
            self.now.fade_out_us = self.now.fade_out_us.min(length.saturating_sub(self.now.fade_in_us));
        }
    }
}

type Commands = Vec<(&'static str, Json)>;

impl NodeCraftApp {
    pub(super) fn open_timeline_editor(&mut self, node: NodeId) {
        self.selection.only(node);
        self.timeline_editor = Some(TimelineEditor::new(self.canvas, node));
    }

    /// The editor, while one is open. True when it owns the frame.
    pub(super) fn timeline_overlay(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(mut editor) = self.timeline_editor.take() else {
            return false;
        };
        let canvas = (editor.canvas == self.canvas).then(|| self.session.document().canvases().get(&self.canvas)).flatten();
        let Some(node) = canvas.and_then(|c| c.nodes().get(&editor.node)).filter(|n| n.kind == timeline::ID).cloned() else {
            // The node went (undo, another canvas): nothing left to edit.
            return false;
        };
        let revision = self.session.revision();
        if editor.listing.as_ref().is_none_or(|(r, _)| *r != revision) {
            let listed = self.session.dispatch("timeline.list", &json!({"canvas": editor.canvas, "id": editor.node})).map_err(|e| e.message);
            match listed.and_then(|v| parse(v).ok_or_else(|| "The clips can't be read.".to_owned())) {
                Ok(listing) => editor.listing = Some((revision, listing)),
                Err(message) => {
                    self.notice = Some(message);
                    return false;
                }
            }
        }
        let Some((_, mut listing)) = editor.listing.take() else {
            return false;
        };
        let titles = titles(self.session.document().canvases().get(&self.canvas), self.session.kinds(), &listing);
        let clips = &mut listing.clips;
        let t = *self.theme.tokens();
        let ctx = ui.ctx().clone();
        if editor.typing && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            editor.draft = None;
        }
        // Lengths the assets don't say, and a thumbnail per video, from the player's probe.
        let edge = t.timeline_track_height * ctx.pixels_per_point();
        let mut posters = HashMap::new();
        let mut shown = clips.clone();
        for s in &mut shown {
            if let Some(asset) = &s.video {
                let assets = self.session.assets();
                let (length, poster) = self.videos.still(&ctx, asset, edge, || assets.preview_read(asset));
                s.video_us = s.video_us.or(length);
                if let Some(poster) = poster {
                    posters.insert(s.clip.id.clone(), poster);
                }
            }
        }
        if let Some(drag) = &editor.drag
            && let Some(s) = shown.iter_mut().find(|s| s.clip.id == drag.clip)
        {
            s.clip = drag.now.clone();
        }
        editor.selected = editor.selected.take().filter(|id| shown.iter().any(|s| &s.clip.id == id));
        let (placed, total) = place(&shown);
        editor.playhead_us = editor.playhead_us.min(total);

        let mut commands: Commands = Vec::new();
        let mut close = false;
        let notice = self.notice.clone();
        let content = ctx.content_rect();
        let gap = t.control_gap * 2.0;
        let shell = egui::Frame::NONE.fill(t.panel_fill).inner_margin(gap);
        egui::Modal::new(egui::Id::new("timeline_editor")).frame(shell).backdrop_color(t.canvas_bg).show(&ctx, |ui| {
            let size = (content.size() - Vec2::splat(gap * 2.0)).max(Vec2::splat(1.0));
            ui.set_min_size(size);
            ui.set_max_size(size);
            ui.horizontal(|ui| {
                ui.heading("Timeline");
                ui.label(egui::RichText::new(&node.title).color(t.text_muted));
                let unknown = placed.iter().any(|p| !p.known);
                ui.label(egui::RichText::new(format!("{}{}", if unknown { "at least " } else { "" }, duration(total))).color(t.text_muted));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close |= ui.button("Done").clicked();
                    if ui.button("Redo").clicked() {
                        commands.push(("edit.redo", json!({})));
                    }
                    if ui.button("Undo").clicked() {
                        commands.push(("edit.undo", json!({})));
                    }
                    if let Some(notice) = &notice {
                        ui.label(egui::RichText::new(notice).color(t.danger));
                    }
                });
            });
            let body = ui.available_rect_before_wrap();
            let side = t.layers_panel_width.min(body.width() / 4.0);
            let left = Rect::from_min_size(body.min, Vec2::new(side, body.height()));
            let right = Rect::from_min_max(Pos2::new(body.max.x - side, body.min.y), body.max);
            let centre = Rect::from_min_max(Pos2::new(left.max.x + gap, body.min.y), Pos2::new(right.min.x - gap, body.max.y));
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| clips_panel(ui, &mut editor, &shown, &placed, &titles, &t, &mut commands));
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("timeline_properties")
                    .show(ui, |ui| properties(ui, &mut editor, &shown, &node, &titles, &t, &mut commands));
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(centre), |ui| {
                track(ui, &mut editor, &shown, &placed, total, &posters, &titles, &t, &mut commands);
            });
            ui.allocate_rect(body, Sense::hover());
        });
        close |= keys(ui, &mut editor, &mut commands);
        editor.typing = crate::canvas::text_input_focused(&ctx);
        // Keep what the list said, not the drag's preview, for the next frame.
        if let Some(drag) = &editor.drag
            && let Some(s) = clips.iter_mut().find(|s| s.clip.id == drag.clip)
        {
            s.clip = drag.before.clone();
        }
        editor.listing = Some((revision, listing));
        for (command, params) in commands {
            if !self.layer_dispatch(command, &params) {
                break;
            }
        }
        if !close {
            self.timeline_editor = Some(editor);
        }
        true
    }
}

fn on(editor: &TimelineEditor, clip: &str) -> Json {
    json!({"canvas": editor.canvas, "id": editor.node, "clip": clip})
}

fn update(editor: &TimelineEditor, clip: &str, set: Json) -> (&'static str, Json) {
    let mut params = on(editor, clip);
    params["set"] = set;
    ("timeline.update", params)
}

/// Each video card's title, by node id, numbered in wire order where titles repeat.
fn titles(canvas: Option<&nodecraft_graph::Canvas>, kinds: &nodecraft_nodes::Registry, listing: &Listing) -> HashMap<String, String> {
    let mut origins: Vec<&str> = listing.origins.iter().map(String::as_str).collect();
    for s in &listing.clips {
        if !origins.contains(&s.clip.origin.as_str()) {
            origins.push(&s.clip.origin);
        }
    }
    let base: Vec<String> = origins
        .iter()
        .map(|origin| {
            canvas
                .and_then(|c| c.nodes().values().find(|n| n.id.to_string() == *origin))
                .map_or_else(|| "Missing video".to_owned(), |n| crate::card::title(n, kinds.get(&n.kind).ok().map(|k| k.descriptor())))
        })
        .collect();
    let mut out = HashMap::new();
    for (i, (origin, title)) in origins.iter().zip(&base).enumerate() {
        let same: Vec<usize> = base.iter().enumerate().filter(|(_, b)| *b == title).map(|(j, _)| j).collect();
        let named = match same.iter().position(|j| *j == i) {
            Some(k) if same.len() > 1 => format!("{title} {}", k.saturating_add(1)),
            _ => title.clone(),
        };
        out.insert((*origin).to_owned(), named);
    }
    out
}

/// What the clip is called: the title of the card its video comes from, and which of its videos.
fn name(titles: &HashMap<String, String>, shown: &[Shown], s: &Shown) -> String {
    let title = titles.get(&s.clip.origin).map_or("Missing video", String::as_str);
    let title = if s.clip.nth > 0 { format!("{title} · {}", s.clip.nth.saturating_add(1)) } else { title.to_owned() };
    // Parts of a split video are numbered in playing order.
    let parts: Vec<&Shown> = shown.iter().filter(|o| o.clip.origin == s.clip.origin && o.clip.nth == s.clip.nth).collect();
    match parts.iter().position(|o| o.clip.id == s.clip.id) {
        Some(at) if parts.len() > 1 => format!("{title} (part {})", at.saturating_add(1)),
        _ => title,
    }
}

/// Keys the editor answers while no field has focus: Delete removes the selected clip, Escape
/// cancels a drag, then clears the selection, then closes; undo and redo work as on the canvas.
/// True to close.
fn keys(ui: &egui::Ui, editor: &mut TimelineEditor, commands: &mut Commands) -> bool {
    use egui::{Key, Modifiers};
    if crate::canvas::text_input_focused(ui.ctx()) {
        return false;
    }
    let mut close = false;
    ui.input_mut(|i| {
        for (id, shortcut) in [("edit.redo", "Command+Shift+Z"), ("edit.redo", "Command+Y"), ("edit.undo", "Command+Z")] {
            if super::consume_shortcut(i, shortcut) {
                commands.push((id, json!({})));
            }
        }
        if i.consume_key(Modifiers::NONE, Key::Escape) && editor.drag.take().is_none() {
            close = editor.selected.is_none();
            editor.selected = None;
        }
        if editor.drag.is_none()
            && (i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace))
            && let Some(id) = editor.selected.take()
        {
            commands.push(("timeline.remove", on(editor, &id)));
        }
    });
    close
}

/// The clips in playing order, with buttons to reorder, split and delete.
fn clips_panel(
    ui: &mut egui::Ui,
    editor: &mut TimelineEditor,
    shown: &[Shown],
    placed: &[Placed],
    titles: &HashMap<String, String>,
    t: &Tokens,
    commands: &mut Commands,
) {
    let index = editor.selected.as_ref().and_then(|id| shown.iter().position(|s| &s.clip.id == id));
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(index.is_some_and(|i| i > 0), egui::Button::new("Earlier")).clicked()
            && let (Some(id), Some(i)) = (&editor.selected, index)
        {
            let mut params = on(editor, id);
            params["index"] = json!(i.saturating_sub(1));
            commands.push(("timeline.move", params));
        }
        if ui.add_enabled(index.is_some_and(|i| i.saturating_add(1) < shown.len()), egui::Button::new("Later")).clicked()
            && let (Some(id), Some(i)) = (&editor.selected, index)
        {
            let mut params = on(editor, id);
            params["index"] = json!(i.saturating_add(1));
            commands.push(("timeline.move", params));
        }
        let cut = cut_at(shown, placed, editor.playhead_us);
        let split = ui.add_enabled(cut.is_some(), egui::Button::new("Split at playhead"));
        let split = if cut.is_some() { split } else { split.on_disabled_hover_text("Put the playhead inside a clip") };
        if split.clicked()
            && let Some((at, video_us)) = cut
            && let Some(s) = shown.get(at)
        {
            let mut params = on(editor, &s.clip.id);
            params["at_us"] = json!(video_us);
            commands.push(("timeline.split", params));
        }
        if ui.add_enabled(editor.selected.is_some(), egui::Button::new("Delete")).clicked()
            && let Some(id) = editor.selected.take()
        {
            commands.push(("timeline.remove", on(editor, &id)));
        }
    });
    ui.separator();
    egui::ScrollArea::vertical().id_salt("timeline_clips").show(ui, |ui| {
        if shown.is_empty() {
            ui.label(egui::RichText::new("Wire videos into the card's clips input.").color(t.text_muted));
        }
        for s in shown {
            let label = name(titles, shown, s);
            ui.horizontal(|ui| {
                let mut enabled = s.clip.enabled;
                let r = ui.checkbox(&mut enabled, "").on_hover_text("Play in the output");
                r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, enabled, format!("Play {label}")));
                if r.changed() {
                    commands.push(update(editor, &s.clip.id, json!({"enabled": enabled})));
                }
                let mut text = egui::RichText::new(&label);
                if !s.playing() {
                    text = text.color(t.text_muted);
                }
                if ui.selectable_label(editor.selected.as_ref() == Some(&s.clip.id), text).clicked() {
                    editor.selected = Some(s.clip.id.clone());
                }
                let note = if !s.wired {
                    "not wired".to_owned()
                } else {
                    s.end_us().map_or_else(|| "length unknown".to_owned(), |end| duration(end.saturating_sub(s.clip.in_us)))
                };
                ui.label(egui::RichText::new(note).color(t.text_muted).small());
            });
        }
    });
}

/// The selected clip's fields, or (with none selected) the output frame's.
fn properties(
    ui: &mut egui::Ui,
    editor: &mut TimelineEditor,
    shown: &[Shown],
    node: &nodecraft_graph::Node,
    titles: &HashMap<String, String>,
    t: &Tokens,
    commands: &mut Commands,
) {
    // A field still holding an edit stays on screen until it applies (see the Layer Editor).
    let held = editor.draft.as_ref().and_then(|(key, _)| key.split_once('.').map(|(clip, _)| clip.to_owned()));
    let chosen = held.filter(|h| h != "frame").or_else(|| editor.selected.clone());
    let Some(s) = chosen.and_then(|id| shown.iter().find(|s| s.clip.id == id)) else {
        frame_properties(ui, editor, node, t, commands);
        return;
    };
    let clip = &s.clip;
    let key = |field: &str| format!("{}.{field}", clip.id);
    ui.label(egui::RichText::new(name(titles, shown, s)).strong());
    if !s.wired {
        ui.label(egui::RichText::new("Its video isn't wired in, so it doesn't play. Wire it in again, or delete the clip.").color(t.text_muted).small());
    }
    let end = s.end_us();
    let limit = seconds(s.video_us.unwrap_or(timeline::MAX_US));
    egui::Grid::new("timeline_clip").num_columns(2).show(ui, |ui| {
        let top = end.map_or(limit, |e| seconds(e.saturating_sub(MIN_US)));
        if let Some(v) = number(ui, &mut editor.draft, &key("in"), "Start (s)", seconds(clip.in_us), 0.0..=top, 0.05, 2) {
            commands.push(update(editor, &clip.id, json!({"in_us": micros(v)})));
        }
        ui.end_row();
        match end {
            Some(e) => {
                let bottom = seconds(clip.in_us.saturating_add(MIN_US));
                if let Some(v) = number(ui, &mut editor.draft, &key("out"), "End (s)", seconds(e), bottom..=limit, 0.05, 2) {
                    let out = micros(v);
                    commands.push(update(editor, &clip.id, json!({"out_us": (Some(out) != s.video_us).then_some(out)})));
                }
            }
            None => {
                ui.label("End");
                ui.label(egui::RichText::new("the video's end").color(t.text_muted));
            }
        }
        ui.end_row();
        let length = end.map_or(limit, |e| seconds(e.saturating_sub(clip.in_us)));
        for (field, label, value) in [("fade_in_us", "Fade in (s)", clip.fade_in_us), ("fade_out_us", "Fade out (s)", clip.fade_out_us)] {
            if let Some(v) = number(ui, &mut editor.draft, &key(field), label, seconds(value), 0.0..=length, 0.05, 2) {
                commands.push(update(editor, &clip.id, json!({field: micros(v)})));
            }
            ui.end_row();
        }
        if let Some(v) = number(ui, &mut editor.draft, &key("transition"), "Into next (s)", seconds(clip.transition_us), 0.0..=length, 0.05, 2) {
            // Under the shortest transition is a cut.
            let us = micros(v);
            commands.push(update(editor, &clip.id, json!({"transition_us": if us < timeline::MIN_TRANSITION_US { 0 } else { us }})));
        }
        ui.end_row();
        let most = f64::from(timeline::MAX_VOLUME) / 10.0;
        if let Some(v) = number(ui, &mut editor.draft, &key("volume"), "Volume (%)", f64::from(clip.volume) / 10.0, 0.0..=most, 1.0, 0) {
            let volume = (v * 10.0).round().clamp(0.0, f64::from(timeline::MAX_VOLUME)) as u16;
            commands.push(update(editor, &clip.id, json!({"volume": volume})));
        }
        ui.end_row();
    });
    let mut muted = clip.muted;
    if ui.checkbox(&mut muted, "Mute").changed() {
        commands.push(update(editor, &clip.id, json!({"muted": muted})));
    }
    if end.is_some() && clip.out_us.is_some() && ui.button("Play to the video's end").clicked() {
        commands.push(update(editor, &clip.id, json!({"out_us": null})));
    }
    ui.label(
        egui::RichText::new("Times are seconds into the clip's own video. Into next cross-fades into the clip after it; the last clip's is ignored.")
            .color(t.text_muted)
            .small(),
    );
}

fn frame_properties(ui: &mut egui::Ui, editor: &mut TimelineEditor, node: &nodecraft_graph::Node, t: &Tokens, commands: &mut Commands) {
    let (canvas, id) = (editor.canvas, editor.node);
    let param = |name: &str, value: Value| ("node.param_set", json!({"canvas": canvas, "id": id, "name": name, "value": param_json(value)}));
    ui.label(egui::RichText::new("Output frame").color(t.text_muted));
    let (min, max) = (timeline::MIN_FRAME as f64, timeline::MAX_FRAME as f64);
    egui::Grid::new("timeline_frame").num_columns(2).show(ui, |ui| {
        for (name, label, default) in [("width", "Width", timeline::DEFAULT_WIDTH), ("height", "Height", timeline::DEFAULT_HEIGHT)] {
            let value = match node.params.get(name) {
                Some(Value::Unsigned(n)) => n.0,
                _ => default,
            };
            if let Some(v) = number(ui, &mut editor.draft, &format!("frame.{name}"), label, value as f64, min..=max, 4.0, 0) {
                commands.push(param(name, Value::Unsigned(nodecraft_types::Unsigned(v.round() as u64))));
            }
            ui.end_row();
        }
        let rate = match node.params.get("fps") {
            Some(Value::Text(text)) => text.clone(),
            _ => "30".into(),
        };
        ui.label("Frame rate");
        egui::ComboBox::from_id_salt("timeline_fps").selected_text(format!("{rate} fps")).show_ui(ui, |ui| {
            for option in timeline::RATES {
                if ui.selectable_label(rate == option, format!("{option} fps")).clicked() && rate != option {
                    commands.push(param("fps", Value::Text(option.into())));
                }
            }
        });
        ui.end_row();
    });
    let colour = match node.params.get("background") {
        Some(Value::Text(text)) => text.clone(),
        _ => "#000000".into(),
    };
    if let Some(Some(v)) = colour_field(ui, &mut editor.draft, "frame.background", "Background", Some(&colour), t) {
        commands.push(param("background", Value::Text(v)));
    }
    ui.label(egui::RichText::new("Pixels, 16 to 4096. Each clip is fitted inside the frame; the background fills the rest.").color(t.text_muted).small());
}

/// Seconds between ruler ticks: the first step at least `min` points apart.
fn tick_us(scale: f32, min: f32) -> u64 {
    const STEPS: [u64; 10] = [100_000, 250_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000, 30_000_000, 60_000_000, 300_000_000];
    STEPS.into_iter().find(|step| seconds(*step) as f32 * scale >= min).unwrap_or(600_000_000)
}

/// The ruler with its playhead, and the playing clips laid along it: drag a clip to reorder it,
/// its edges to trim it, and the ruler to move the playhead.
#[allow(clippy::too_many_arguments)]
fn track(
    ui: &mut egui::Ui,
    editor: &mut TimelineEditor,
    shown: &[Shown],
    placed: &[Placed],
    total: u64,
    posters: &HashMap<String, (egui::TextureId, [u32; 2])>,
    titles: &HashMap<String, String>,
    t: &Tokens,
    commands: &mut Commands,
) {
    let area = ui.available_rect_before_wrap();
    let height = t.timeline_ruler_height + t.timeline_track_height;
    let preview = Rect::from_min_max(area.min, Pos2::new(area.max.x, (area.max.y - height - t.control_gap * 2.0).max(area.min.y)));
    let ruler = Rect::from_min_size(Pos2::new(area.min.x, preview.max.y + t.control_gap), Vec2::new(area.width(), t.timeline_ruler_height));
    let lane = Rect::from_min_size(Pos2::new(area.min.x, ruler.max.y), Vec2::new(area.width(), t.timeline_track_height));
    let painter = ui.painter_at(area);

    // What the playhead is over: the clip's thumbnail, as a still.
    let under = cut_at(shown, placed, editor.playhead_us.max(1)).or_else(|| placed.first().map(|p| (p.index, 0)));
    if let Some((texture, size)) = under.and_then(|(i, _)| shown.get(i)).and_then(|s| posters.get(&s.clip.id)) {
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        let fit = Vec2::new(preview.height() * aspect, preview.height());
        let fit = if fit.x > preview.width() { Vec2::new(preview.width(), preview.width() / aspect.max(0.01)) } else { fit };
        painter.image(*texture, Rect::from_center_size(preview.center(), fit), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    } else {
        let why = if placed.is_empty() { "Nothing plays yet: wire videos in and enable a clip." } else { "Run the card to render the timeline." };
        painter.text(preview.center(), egui::Align2::CENTER_CENTER, why, egui::FontId::proportional(t.body_size), t.text_muted);
    }

    let span = total.max(UNKNOWN_US);
    // Held while a drag lasts, so a trim that changes the total doesn't rescale under the pointer.
    if editor.drag.is_none() {
        editor.scale = (lane.width() / seconds(span) as f32).max(0.01);
        editor.origin = lane.left_top();
    }
    let x = |us: u64| lane.min.x + seconds(us) as f32 * editor.scale;
    let time_at = |px: f32, scale: f32| micros(f64::from((px - lane.min.x) / scale));

    // The ruler: ticks with times, and a click or drag there moves the playhead.
    let ruled = ui.interact(ruler, ui.id().with("timeline_ruler"), Sense::click_and_drag());
    ruled.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "Playhead"));
    if (ruled.clicked() || ruled.dragged())
        && let Some(p) = ruled.interact_pointer_pos()
    {
        editor.playhead_us = time_at(p.x, editor.scale).min(total);
    }
    let step = tick_us(editor.scale, 60.0);
    let mut tick = 0u64;
    while tick <= span {
        let at = x(tick);
        painter.line_segment([Pos2::new(at, ruler.max.y - t.control_gap), Pos2::new(at, ruler.max.y)], egui::Stroke::new(1.0, t.panel_stroke));
        painter.text(Pos2::new(at + 2.0, ruler.min.y), egui::Align2::LEFT_TOP, duration(tick), egui::FontId::proportional(t.label_size), t.text_muted);
        tick = tick.saturating_add(step);
    }
    painter.rect_filled(lane, f32::from(t.card_radius) / 2.0, t.card_fill);

    // The clips, with grips at their edges. The dragged one draws where it is now.
    let grip = t.timeline_trim_grip;
    for p in placed {
        let Some(s) = shown.get(p.index) else { continue };
        let label = name(titles, shown, s);
        let block = Rect::from_min_max(Pos2::new(x(p.start_us), lane.min.y + 2.0), Pos2::new(x(p.start_us.saturating_add(p.length_us)), lane.max.y - 2.0));
        let selected = editor.selected.as_ref() == Some(&s.clip.id);
        let id = ui.id().with(("timeline_clip", &s.clip.id));
        let r = ui.interact(block, id, Sense::click_and_drag());
        r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, format!("{label} on the track")));
        let mut grips = vec![(Grip::Move, r)];
        let edges = [(Grip::Start, block.min.x, "Trim start of"), (Grip::End, block.max.x, "Trim end of")];
        for (kind, at, what) in edges {
            // An end only trims once it is known.
            if kind == Grip::End && s.end_us().is_none() {
                continue;
            }
            // Inside the clip, so where two clips meet each grip trims its own.
            let inner = if kind == Grip::Start { at + grip } else { at - grip };
            let zone = Rect::from_center_size(Pos2::new(inner, block.center().y), Vec2::new(grip * 2.0, block.height()));
            let r = ui.interact(zone, id.with(what), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, format!("{what} {label}")));
            grips.push((kind, r));
        }
        for (kind, r) in &grips {
            // Measured from where the press began, not where the drag was recognised.
            if r.drag_started()
                && editor.drag.is_none()
                && let Some(p) = ui.input(|i| i.pointer.press_origin()).or_else(|| r.interact_pointer_pos())
            {
                let before = s.clip.clone();
                editor.drag = Some(Drag { clip: s.clip.id.clone(), grip: *kind, from_x: p.x, now: before.clone(), before, video_us: s.video_us, to: None });
                editor.selected = Some(s.clip.id.clone());
            }
        }
        if grips.first().is_some_and(|(_, r)| r.clicked()) {
            editor.selected = Some(s.clip.id.clone());
        }

        let stroke = if selected { egui::Stroke::new(2.0, t.card_stroke_selected) } else { egui::Stroke::new(1.0, t.wire_video) };
        painter.rect_filled(block, f32::from(t.card_radius) / 3.0, t.panel_fill);
        if let Some((texture, size)) = posters.get(&s.clip.id) {
            let aspect = size[0] as f32 / size[1].max(1) as f32;
            let thumb = Rect::from_min_size(block.min, Vec2::new((block.height() * aspect).min(block.width()), block.height()));
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new((thumb.width() / (block.height() * aspect).max(1.0)).min(1.0), 1.0));
            painter.image(*texture, thumb, uv, Color32::WHITE);
        }
        // Fades as ramps over the top edge, the crossing into the next clip as a band.
        let ramp = |from: u64, to: u64| [Pos2::new(x(from), block.max.y), Pos2::new(x(to), block.min.y)];
        if s.clip.fade_in_us > 0 {
            painter.line_segment(ramp(p.start_us, p.start_us.saturating_add(s.clip.fade_in_us)), egui::Stroke::new(1.0, t.accent));
        }
        let end = p.start_us.saturating_add(p.length_us);
        if s.clip.fade_out_us > 0 {
            painter.line_segment(ramp(end, end.saturating_sub(s.clip.fade_out_us)), egui::Stroke::new(1.0, t.accent));
        }
        if p.overlap_us > 0 {
            let band = Rect::from_min_max(Pos2::new(x(end.saturating_sub(p.overlap_us)), block.min.y), Pos2::new(block.max.x, block.min.y + grip));
            painter.rect_filled(band, 0.0, t.accent);
        }
        painter.rect_stroke(block, f32::from(t.card_radius) / 3.0, stroke, egui::StrokeKind::Inside);
        let mut text = label;
        if s.clip.muted {
            text.push_str(" · muted");
        }
        if !p.known {
            text.push_str(" · length unknown");
        }
        let text_at = Pos2::new(block.min.x + t.control_gap, block.max.y - t.control_gap);
        painter.with_clip_rect(block).text(text_at, egui::Align2::LEFT_BOTTOM, text, egui::FontId::proportional(t.label_size), t.text);
    }

    // The drag: follow the pointer, and on release apply it as one command.
    if let Some(drag) = &mut editor.drag {
        let pointer = ui.input(|i| i.pointer.interact_pos());
        if let Some(p) = pointer {
            drag.trim(p.x - drag.from_x, editor.scale);
            if drag.grip == Grip::Move {
                // Lands before the first other playing clip whose middle is right of the pointer.
                let others: Vec<&Placed> = placed.iter().filter(|q| shown.get(q.index).is_some_and(|s| s.clip.id != drag.clip)).collect();
                let before = others.iter().find(|q| x(q.start_us.saturating_add(q.length_us / 2)) > p.x).map(|q| q.index);
                let order: Vec<usize> = (0..shown.len()).filter(|i| shown.get(*i).is_some_and(|s| s.clip.id != drag.clip)).collect();
                drag.to = Some(before.and_then(|b| order.iter().position(|i| *i == b)).unwrap_or(order.len()));
                let at = before.and_then(|b| placed.iter().find(|q| q.index == b)).map_or(x(total), |q| x(q.start_us));
                painter.line_segment([Pos2::new(at, lane.min.y), Pos2::new(at, lane.max.y)], egui::Stroke::new(2.0, t.accent));
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            let (id, before, now) = (drag.clip.clone(), drag.before.clone(), drag.now.clone());
            match drag.grip {
                Grip::Move => {
                    let from = shown.iter().position(|s| s.clip.id == id);
                    if let Some(to) = drag.to
                        && Some(to) != from
                    {
                        let mut params = on(editor, &id);
                        params["index"] = json!(to);
                        commands.push(("timeline.move", params));
                    }
                }
                _ if now != before => {
                    let mut set = serde_json::Map::new();
                    for (field, was, is) in [("fade_in_us", before.fade_in_us, now.fade_in_us), ("fade_out_us", before.fade_out_us, now.fade_out_us)] {
                        if was != is {
                            set.insert(field.into(), json!(is));
                        }
                    }
                    if now.in_us != before.in_us {
                        set.insert("in_us".into(), json!(now.in_us));
                    }
                    if now.out_us != before.out_us {
                        set.insert("out_us".into(), json!(now.out_us));
                    }
                    commands.push(update(editor, &id, Json::Object(set)));
                }
                _ => {}
            }
            editor.drag = None;
        }
    }

    let head = x(editor.playhead_us);
    painter.line_segment([Pos2::new(head, ruler.min.y), Pos2::new(head, lane.max.y)], egui::Stroke::new(1.5, t.danger));
    ui.allocate_rect(area, Sense::hover());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(id: &str, in_us: u64, out_us: Option<u64>, video_us: Option<u64>, transition_us: u64) -> Shown {
        let clip = Clip { in_us, out_us, transition_us, ..Clip::whole(id.into(), "n".into(), 0) };
        Shown { clip, wired: true, video_us, video: None }
    }

    #[test]
    fn clips_are_placed_one_after_another_and_cross_by_their_transition() {
        let mut clips = vec![shown("a", 500_000, None, Some(2_000_000), 300_000), shown("b", 0, None, Some(1_000_000), 0), shown("c", 0, None, None, 0)];
        let (placed, total) = place(&clips);
        let starts: Vec<(u64, u64, bool)> = placed.iter().map(|p| (p.start_us, p.length_us, p.known)).collect();
        assert_eq!(starts, [(0, 1_500_000, true), (1_200_000, 1_000_000, true), (2_200_000, UNKNOWN_US, false)]);
        assert_eq!(total, 2_200_000 + UNKNOWN_US);
        // A transition is no longer than either clip; disabled and unwired clips don't play.
        clips[0].clip.transition_us = 5_000_000;
        clips[1].clip.enabled = false;
        clips[2].wired = false;
        let (placed, total) = place(&clips);
        assert_eq!((placed.len(), total), (1, 1_500_000));
        assert_eq!(cut_at(&clips, &placed, 1_000_000), Some((0, 1_500_000)));
        assert_eq!(cut_at(&clips, &placed, 0), None, "a cut is strictly inside a clip");
    }

    #[test]
    fn trims_stay_inside_the_video_and_keep_the_clip_long_enough() {
        let clip = Clip { fade_in_us: 400_000, fade_out_us: 400_000, ..Clip::whole("a".into(), "n".into(), 0) };
        let drag = |grip| Drag { clip: "a".into(), grip, from_x: 0.0, before: clip.clone(), video_us: Some(1_000_000), now: clip.clone(), to: None };
        let mut start = drag(Grip::Start);
        start.trim(-50.0, 100.0);
        assert_eq!(start.now.in_us, 0, "not before the video starts");
        start.trim(30.0, 100.0);
        assert_eq!((start.now.in_us, start.now.fade_in_us, start.now.fade_out_us), (300_000, 350_000, 350_000), "fades shrink to fit");
        start.trim(500.0, 100.0);
        assert_eq!(start.now.in_us, 900_000, "at least MIN_US long");
        let mut end = drag(Grip::End);
        end.trim(-25.0, 100.0);
        assert_eq!(end.now.out_us, Some(750_000));
        end.trim(80.0, 100.0);
        assert_eq!(end.now.out_us, None, "back at the video's end plays to the end");
        end.trim(f32::NAN, 0.0);
        assert_eq!(end.now.out_us, None);
    }

    #[test]
    fn times_read_and_show_in_seconds() {
        assert_eq!((micros(1.25), micros(-3.0), micros(f64::NAN), micros(1e12)), (1_250_000, 0, 0, timeline::MAX_US));
        assert_eq!((duration(2_250_000), duration(62_500_000)), ("2.2 s".to_owned(), "1:02.5".to_owned()));
        assert_eq!((tick_us(100.0, 60.0), tick_us(1.0, 60.0)), (1_000_000, 60_000_000));
    }
}
