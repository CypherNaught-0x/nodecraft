//! The Timeline editor (#109, #576): a full-screen view of a Timeline card's clips on their
//! tracks, where they are trimmed, reordered, moved, split, faded and muted. The main track plays
//! its clips one after another; every other track holds its clips at their own start time,
//! pictures over the main track and sounds under it. It changes the document only through
//! commands: each finished drag is one `timeline.update` or `timeline.move`, each field one
//! `timeline.update`, so every gesture is one undo step. Lengths and thumbnails come from the
//! shell's video player, which probes each wired video with the settings' tools; a clip whose
//! length isn't known yet takes a stand-in width. Rendering is the card's run, as for any node.
//! Captions (#574) are listed, edited and previewed in `captions`.
use super::NodeCraftApp;
use super::layer_editor::{colour_field, number, param_json};
use crate::theme::Tokens;
use egui::{Color32, Pos2, Rect, Sense, Vec2};
use nodecraft_nodes::timeline::{self, Caption, Clip, Input, TrackKind};
use nodecraft_types::{AssetRef, CanvasId, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::HashMap;

/// A clip is trimmed no shorter than this.
const MIN_US: u64 = 100_000;
/// The track's stand-in for a clip whose length isn't known yet.
const UNKNOWN_US: u64 = 5_000_000;

mod captions;

pub(super) struct TimelineEditor {
    canvas: CanvasId,
    pub(super) node: NodeId,
    pub(super) selected: Option<String>,
    /// The caption picked instead of a clip.
    pub(super) caption: Option<String>,
    drag: Option<Drag>,
    /// `timeline.list` as of a document revision.
    listing: Option<(u64, Listing)>,
    /// A field being typed or dragged, applied when it lets go: (field key, text).
    pub(super) draft: Option<(String, String)>,
    /// A field had focus last frame (see the Layer Editor's `typing`).
    typing: bool,
    /// Where the playhead is, in microseconds of output.
    pub(super) playhead_us: u64,
    /// Screen points per second of output, and where the track's time 0 is, as last drawn.
    pub(super) scale: f32,
    pub(super) origin: Pos2,
    /// Where the picture under the playhead is shown, as last drawn.
    preview: Rect,
}

impl TimelineEditor {
    pub(super) fn new(canvas: CanvasId, node: NodeId) -> Self {
        Self {
            canvas,
            node,
            selected: None,
            caption: None,
            drag: None,
            listing: None,
            draft: None,
            typing: false,
            playhead_us: 0,
            scale: 1.0,
            origin: Pos2::ZERO,
            preview: Rect::NOTHING,
        }
    }
}

/// One listed clip and what the editor knows of its media.
#[derive(Clone, Debug)]
struct Shown {
    clip: Clip,
    /// Its media is wired in now.
    wired: bool,
    /// The video's or sound's length, from its asset or the player's probe.
    media_us: Option<u64>,
    /// Its video, for the player to probe and show; none for sounds and stills.
    video: Option<AssetRef>,
    source_hash: Option<nodecraft_types::Blake3>,
    rendered_sources: Vec<nodecraft_graph::SourceVersion>,
}

impl Shown {
    fn playing(&self) -> bool {
        self.clip.enabled && self.wired
    }

    /// Where the clip ends in its media, when known.
    fn end_us(&self) -> Option<u64> {
        self.clip.out_us.or(self.media_us)
    }

    fn source_changed(&self) -> bool {
        self.source_hash.is_some_and(|hash| self.rendered_sources.iter().any(|source| source.hash != hash))
    }

    fn measured_length(&mut self, actual: Option<u64>) {
        self.media_us = actual.or(self.media_us);
    }

    fn trim_warning(&self) -> Option<String> {
        let length = self.media_us.filter(|_| self.playing() && self.clip.input != Input::Stills)?;
        (self.clip.in_us >= length || self.clip.out_us.is_some_and(|end| end > length))
            .then(|| format!("The current source is {} long, shorter than this clip's trim. Adjust its start or end before rendering.", duration(length)))
    }
}

#[derive(Deserialize)]
struct Wired {
    origin: String,
    input: Input,
    media: Option<AssetRef>,
}

struct Listing {
    clips: Vec<Shown>,
    /// The nodes media is wired in from, in wire order.
    origins: Vec<String>,
    /// What each track holds, the main track first.
    tracks: Vec<TrackKind>,
    captions: Vec<Caption>,
}

/// `timeline.list`'s answer: each clip with its media, if it is wired in, and the tracks.
fn parse(listed: Json) -> Option<Listing> {
    let inputs: Vec<Wired> = serde_json::from_value(listed.get("inputs")?.clone()).ok()?;
    let tracks: Vec<TrackKind> =
        listed.get("tracks")?.as_array()?.iter().map(|t| serde_json::from_value(t.get("kind")?.clone()).ok()).collect::<Option<_>>()?;
    let mut out = Vec::new();
    for entry in listed.get("clips")?.as_array()? {
        let mut entry = entry.clone();
        let fields = entry.as_object_mut()?;
        let wired = fields.remove("wired").and_then(|w| w.as_bool()).unwrap_or(false);
        let media_us = fields.remove("media_us").and_then(|v| v.as_u64());
        let source_hash = fields.remove("source_hash").and_then(|v| serde_json::from_value(v).ok());
        let rendered_sources = fields.remove("rendered_sources").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        fields.remove("rendered_generation");
        let clip: Clip = serde_json::from_value(entry).ok()?;
        let video = usize::try_from(clip.nth)
            .ok()
            .and_then(|nth| inputs.iter().filter(|i| i.input == clip.input && i.origin == clip.origin).nth(nth))
            .and_then(|i| i.media.clone())
            .filter(|_| wired && clip.input == Input::Clips);
        out.push(Shown { clip, wired, media_us, video, source_hash, rendered_sources });
    }
    let mut origins: Vec<String> = Vec::new();
    for input in inputs {
        if !origins.contains(&input.origin) {
            origins.push(input.origin);
        }
    }
    let captions = serde_json::from_value(listed.get("captions")?.clone()).ok()?;
    Some(Listing { clips: out, origins, tracks, captions })
}

/// Whether `track` (of `tracks`, the main one first) takes the clip's media: pictures go on
/// video tracks, sounds on audio tracks.
fn fits(tracks: &[TrackKind], track: u8, input: Input) -> bool {
    tracks.get(usize::from(track)).is_some_and(|kind| (*kind == TrackKind::Video) == input.visual())
}

/// What a track is called: the main one, then each by its number.
fn track_name(tracks: &[TrackKind], track: u8) -> String {
    match tracks.get(usize::from(track)) {
        _ if track == 0 => "Main track".to_owned(),
        Some(TrackKind::Audio) => format!("Sound track {track}"),
        _ => format!("Picture track {track}"),
    }
}

/// A playing clip's place on its track.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    /// Its index among the listed clips.
    index: usize,
    track: u8,
    start_us: u64,
    length_us: u64,
    /// False while the length is the stand-in.
    known: bool,
    /// How much of its end the next clip crosses into.
    overlap_us: u64,
}

/// The playing clips: the main track's one after another, each crossing `transition_us` into the
/// next (no more than either is long), the other tracks' at their own start; and the output's
/// length, to the last end on any track.
fn place(clips: &[Shown]) -> (Vec<Placed>, u64) {
    let at_place = |(index, s): (usize, &Shown)| {
        // On the track a clip lasts as long as it plays, at its speed.
        let length = s.end_us().map(|end| s.clip.played(end.saturating_sub(s.clip.in_us)));
        let start_us = if s.clip.track == 0 { 0 } else { s.clip.at_us };
        Placed { index, track: s.clip.track, start_us, length_us: length.unwrap_or(UNKNOWN_US).max(1), known: length.is_some(), overlap_us: 0 }
    };
    let mut placed: Vec<Placed> = clips.iter().enumerate().filter(|(_, s)| s.playing() && s.clip.track == 0).map(at_place).collect();
    let mut at = 0u64;
    for k in 0..placed.len() {
        let next = placed.get(k.saturating_add(1)).map(|p| p.length_us);
        let Some(p) = placed.get_mut(k) else { break };
        p.start_us = at;
        let transition = clips.get(p.index).map_or(0, |s| s.clip.transition_us);
        p.overlap_us = next.map_or(0, |next| transition.min(p.length_us).min(next));
        at = at.saturating_add(p.length_us).saturating_sub(p.overlap_us);
    }
    let mut total = at;
    for p in clips.iter().enumerate().filter(|(_, s)| s.playing() && s.clip.track > 0).map(at_place) {
        total = total.max(p.start_us.saturating_add(p.length_us));
        placed.push(p);
    }
    (placed, total)
}

fn inside(p: &Placed, us: u64) -> bool {
    p.start_us < us && us < p.start_us.saturating_add(p.length_us)
}

/// The clip to split at output time `us`, and how far into its media that is: the selected clip
/// when `us` is strictly inside it, else the main track's. Stills don't split.
fn cut_at(clips: &[Shown], placed: &[Placed], us: u64, selected: Option<&str>) -> Option<(usize, u64)> {
    let splits = |p: &&Placed| inside(p, us) && clips.get(p.index).is_some_and(|s| s.clip.input != Input::Stills);
    let chosen = placed.iter().filter(splits).find(|p| clips.get(p.index).is_some_and(|s| Some(s.clip.id.as_str()) == selected));
    chosen.or_else(|| placed.iter().rev().filter(|p| p.track == 0).find(splits)).and_then(|p| {
        let clip = &clips.get(p.index)?.clip;
        Some((p.index, clip.in_us.saturating_add(media(us.saturating_sub(p.start_us), clip.speed))))
    })
}

/// How much of its media a clip at `speed` plays in `played_us` of output.
fn media(played_us: u64, speed: u16) -> u64 {
    let us = u128::from(played_us) * u128::from(speed) / u128::from(timeline::UNIT);
    u64::try_from(us).unwrap_or(u64::MAX)
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
    /// Where on the output the clip started when the drag began.
    from_us: u64,
    before: Clip,
    media_us: Option<u64>,
    now: Clip,
    /// For a move along the main track: the index in playing order it would land at.
    to: Option<usize>,
}

/// Where a dragged edge catches: on another clip's edge, the playhead or the output's start when
/// within `reach_us` of one, else on a whole frame.
#[derive(Clone, Debug, Default, PartialEq)]
struct Snap {
    /// Output times to catch on.
    points: Vec<u64>,
    reach_us: u64,
    /// The output's frames a second; 0 snaps to no frame.
    fps: u64,
}

impl Snap {
    /// The travel `dt` (microseconds of output) adjusted so the nearest of `edges` (output times
    /// before the drag) to a point lands on it, or else the first edge on a whole frame.
    fn travel(&self, dt: i128, edges: &[u64]) -> i128 {
        let reach = i128::from(self.reach_us);
        let caught = edges
            .iter()
            .flat_map(|e| self.points.iter().map(move |p| i128::from(*p) - (i128::from(*e) + dt)))
            .filter(|off| off.abs() <= reach)
            .min_by_key(|off| off.abs());
        if let Some(off) = caught {
            return dt + off;
        }
        // The nearest frame's own time, rounded, so whole frames don't drift at 24 fps; no
        // rate (a test's) catches nowhere.
        let fps = i128::from(self.fps);
        if fps == 0 {
            return dt;
        }
        edges.first().map_or(dt, |e| {
            let to = i128::from(*e) + dt;
            let n = (to * fps + 500_000).div_euclid(1_000_000);
            (n * 1_000_000 + fps / 2).div_euclid(fps) - i128::from(*e)
        })
    }
}

impl Drag {
    /// Trims or moves by the pointer's travel since it went down, `dx` points at `scale` points a
    /// second, onto `track`, the moved edges catching as `snap` says. Off the main track a move
    /// shifts the clip's start, and a start trim keeps its end where it was. The travel is output
    /// time; a trim moves through the media at the clip's speed.
    fn trim(&mut self, dx: f32, scale: f32, track: u8, snap: &Snap) {
        self.now = self.before.clone();
        let travel = if scale > 0.0 && dx.is_finite() { f64::from(dx) / f64::from(scale) * 1e6 } else { 0.0 };
        let travel = travel.clamp(-1e15, 1e15).round() as i128;
        let speed = i128::from(self.before.speed.max(1));
        let unit = i128::from(timeline::UNIT);
        let end = self.before.out_us.or(self.media_us);
        let played = end.map(|e| self.before.played(e.saturating_sub(self.before.in_us)));
        let ends = played.map(|p| self.from_us.saturating_add(p));
        let dt = match self.grip {
            // On the main track a start trim leaves the clip's start where it is: only whole frames.
            Grip::Start if self.before.track == 0 => Snap { points: Vec::new(), ..snap.clone() }.travel(travel, &[self.from_us]),
            Grip::Start => snap.travel(travel, &[self.from_us]),
            Grip::End => ends.map_or(travel, |e| snap.travel(travel, &[e])),
            Grip::Move if track > 0 => snap.travel(travel, &[Some(self.from_us), ends].into_iter().flatten().collect::<Vec<_>>()),
            Grip::Move => travel,
        };
        // The same travel through the clip's media.
        let dm = dt.saturating_mul(speed) / unit;
        let limit = i128::from(self.media_us.unwrap_or(timeline::MAX_US));
        let fit = |v: i128, lo: i128, hi: i128| u64::try_from(v.clamp(lo, hi.max(lo))).unwrap_or(0);
        let most = i128::from(timeline::MAX_US);
        match self.grip {
            Grip::Start => {
                let end = i128::from(end.unwrap_or(timeline::MAX_US));
                // Off the main track the start can't trim to before the output does.
                let earliest =
                    if self.before.track == 0 { 0 } else { i128::from(self.before.in_us) - i128::from(self.before.at_us).saturating_mul(speed) / unit };
                self.now.in_us = fit(i128::from(self.before.in_us).saturating_add(dm), earliest.max(0), end - i128::from(MIN_US));
                if self.before.track > 0 {
                    let shift = (i128::from(self.now.in_us) - i128::from(self.before.in_us)).saturating_mul(unit) / speed;
                    self.now.at_us = fit(i128::from(self.before.at_us) + shift, 0, most);
                }
            }
            Grip::End => {
                if let Some(end) = end {
                    let out = fit(i128::from(end).saturating_add(dm), i128::from(self.before.in_us.saturating_add(MIN_US)), limit);
                    // Back to the media's end plays to its end, wherever that is.
                    self.now.out_us = (Some(out) != self.media_us).then_some(out);
                }
            }
            Grip::Move => {
                self.now.track = track;
                if track > 0 {
                    self.now.at_us = fit(i128::from(self.from_us).saturating_add(dt), 0, most);
                    if self.before.track == 0 {
                        self.now.transition_us = 0;
                    }
                } else {
                    self.now.at_us = 0;
                }
            }
        }
        // Fades shrink with the clip, as `Clip::check` requires; they are output time.
        let length = self.now.out_us.or(self.media_us).map(|end| self.now.played(end.saturating_sub(self.now.in_us)));
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
        let tracks = listing.tracks.clone();
        let shown_captions = listing.captions.clone();
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
                s.measured_length(length);
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
        editor.caption = editor.caption.take().filter(|id| shown_captions.iter().any(|c| &c.id == id));
        let (placed, total) = place(&shown);
        editor.playhead_us = editor.playhead_us.min(total);

        let mut commands: Commands = Vec::new();
        let mut close = false;
        let mut export = false;
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
                    export |= ui
                        .button("Export for another editor…")
                        .on_hover_text("Save an FCPXML document and the media it plays as one archive, without rendering.")
                        .clicked();
                    let room = tracks.len() <= timeline::MAX_TRACKS;
                    for (kind, label) in [("audio", "Add sound track"), ("video", "Add picture track")] {
                        if ui.add_enabled(room, egui::Button::new(label)).clicked() {
                            commands.push(("timeline.track_add", json!({"canvas": editor.canvas, "id": editor.node, "kind": kind})));
                        }
                    }
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
            if shown.iter().any(|s| s.trim_warning().is_some()) {
                ui.label(egui::RichText::new("A clip extends beyond its current source. Adjust its trim before rendering.").color(t.danger));
            }
            let body = ui.available_rect_before_wrap();
            let side = t.layers_panel_width.min(body.width() / 4.0);
            let left = Rect::from_min_size(body.min, Vec2::new(side, body.height()));
            let right = Rect::from_min_max(Pos2::new(body.max.x - side, body.min.y), body.max);
            let centre = Rect::from_min_max(Pos2::new(left.max.x + gap, body.min.y), Pos2::new(right.min.x - gap, body.max.y));
            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                clips_panel(ui, &mut editor, &shown, &placed, &tracks, &titles, &shown_captions, &t, &mut commands)
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("timeline_properties")
                    .show(ui, |ui| properties(ui, &mut editor, &shown, &node, &tracks, &titles, &shown_captions, &t, &mut commands));
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(centre), |ui| {
                track(ui, &mut editor, &shown, &placed, &tracks, total, &posters, &titles, &node, &t, &mut commands);
                captions::preview(ui.painter(), editor.preview, frame_size(&node), &shown_captions, editor.playhead_us);
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
        if export {
            let stem: String = node.title.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_')).take(48).collect();
            let stem = if stem.trim().is_empty() { "Timeline".to_owned() } else { stem.trim().to_owned() };
            self.start_interchange_export(editor.canvas, editor.node, &format!("{stem}.zip"));
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

/// Each wired card's title, by node id, numbered in wire order where titles repeat.
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
                .map_or_else(|| "Missing input".to_owned(), |n| crate::card::title(n, kinds.get(&n.kind).ok().map(|k| k.descriptor())))
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

/// What the clip is called: the title of the card its media comes from, and which of its outputs.
fn name(titles: &HashMap<String, String>, shown: &[Shown], s: &Shown) -> String {
    let title = titles.get(&s.clip.origin).map_or("Missing input", String::as_str);
    let title = if s.clip.nth > 0 { format!("{title} · {}", s.clip.nth.saturating_add(1)) } else { title.to_owned() };
    // A card wired into two inputs says which one this is.
    let title = match s.clip.input {
        Input::Clips => title,
        Input::Audio if shown.iter().any(|o| o.clip.origin == s.clip.origin && o.clip.input != Input::Audio) => format!("{title} (sound)"),
        Input::Stills if shown.iter().any(|o| o.clip.origin == s.clip.origin && o.clip.input != Input::Stills) => format!("{title} (still)"),
        _ => title,
    };
    // Parts of a split clip are numbered in playing order.
    let parts: Vec<&Shown> = shown.iter().filter(|o| o.clip.origin == s.clip.origin && o.clip.input == s.clip.input && o.clip.nth == s.clip.nth).collect();
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
            close = editor.selected.is_none() && editor.caption.is_none();
            editor.selected = None;
            editor.caption = None;
        }
        if editor.drag.is_none() && (i.consume_key(Modifiers::NONE, Key::Delete) || i.consume_key(Modifiers::NONE, Key::Backspace)) {
            if let Some(id) = editor.selected.take() {
                commands.push(("timeline.remove", on(editor, &id)));
            } else if let Some(id) = editor.caption.take() {
                commands.push(("timeline.caption_remove", json!({"canvas": editor.canvas, "id": editor.node, "caption": id})));
            }
        }
    });
    close
}

/// The clips, the main track's in playing order, with buttons to reorder, split and delete, and
/// the tracks with a button to remove each empty one.
#[allow(clippy::too_many_arguments)]
fn clips_panel(
    ui: &mut egui::Ui,
    editor: &mut TimelineEditor,
    shown: &[Shown],
    placed: &[Placed],
    tracks: &[TrackKind],
    titles: &HashMap<String, String>,
    captions: &[Caption],
    t: &Tokens,
    commands: &mut Commands,
) {
    // Earlier and Later reorder the main track; elsewhere a clip's start says where it plays.
    let main: Vec<&str> = shown.iter().filter(|s| s.clip.track == 0).map(|s| s.clip.id.as_str()).collect();
    let index = editor.selected.as_ref().and_then(|id| main.iter().position(|m| *m == id));
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(index.is_some_and(|i| i > 0), egui::Button::new("Earlier")).clicked()
            && let (Some(id), Some(i)) = (&editor.selected, index)
        {
            let mut params = on(editor, id);
            params["index"] = json!(i.saturating_sub(1));
            commands.push(("timeline.move", params));
        }
        if ui.add_enabled(index.is_some_and(|i| i.saturating_add(1) < main.len()), egui::Button::new("Later")).clicked()
            && let (Some(id), Some(i)) = (&editor.selected, index)
        {
            let mut params = on(editor, id);
            params["index"] = json!(i.saturating_add(1));
            commands.push(("timeline.move", params));
        }
        let cut = cut_at(shown, placed, editor.playhead_us, editor.selected.as_deref());
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
            ui.label(egui::RichText::new("Wire videos, sounds or pictures into the card's inputs.").color(t.text_muted));
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
                    editor.caption = None;
                }
                let note = if !s.wired {
                    "not wired".to_owned()
                } else {
                    s.end_us().map_or_else(|| "length unknown".to_owned(), |end| duration(s.clip.played(end.saturating_sub(s.clip.in_us))))
                };
                let note = if s.clip.track == 0 { note } else { format!("{note} · {}", track_name(tracks, s.clip.track)) };
                ui.label(egui::RichText::new(note).color(t.text_muted).small());
                if let Some(warning) = s.trim_warning() {
                    ui.label(egui::RichText::new("Trim exceeds source").color(t.danger).small()).on_hover_text(warning);
                } else if s.source_changed() {
                    ui.label(egui::RichText::new("Source changed").color(t.danger).small());
                }
            });
        }
        ui.separator();
        ui.label(egui::RichText::new("Tracks").color(t.text_muted));
        for track in (0..tracks.len()).filter_map(|k| u8::try_from(k).ok()) {
            let label = track_name(tracks, track);
            ui.horizontal(|ui| {
                ui.label(&label);
                if track == 0 {
                    return;
                }
                let empty = !shown.iter().any(|s| s.clip.track == track);
                let remove = ui.add_enabled(empty, egui::Button::new("Remove").small());
                let remove = if empty { remove } else { remove.on_disabled_hover_text("Move or delete its clips first") };
                remove.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, empty, format!("Remove {label}")));
                if remove.clicked() {
                    commands.push(("timeline.track_remove", json!({"canvas": editor.canvas, "id": editor.node, "track": track})));
                }
            });
        }
        captions::list(ui, editor, captions, t, commands);
    });
}

/// The selected clip's fields, or (with none selected) the output frame's.
#[allow(clippy::too_many_arguments)]
fn properties(
    ui: &mut egui::Ui,
    editor: &mut TimelineEditor,
    shown: &[Shown],
    node: &nodecraft_graph::Node,
    tracks: &[TrackKind],
    titles: &HashMap<String, String>,
    captions: &[Caption],
    t: &Tokens,
    commands: &mut Commands,
) {
    // A field still holding an edit stays on screen until it applies (see the Layer Editor).
    let caption = captions::held(editor.draft.as_ref()).or_else(|| editor.caption.clone().filter(|_| editor.draft.is_none()));
    if let Some(id) = caption
        && captions::properties(ui, editor, captions, &id, t, commands)
    {
        return;
    }
    let held = editor.draft.as_ref().filter(|(key, _)| !key.starts_with("caption:")).and_then(|(key, _)| key.split_once('.').map(|(clip, _)| clip.to_owned()));
    let chosen = held.filter(|h| h != "frame").or_else(|| editor.selected.clone());
    let Some(s) = chosen.and_then(|id| shown.iter().find(|s| s.clip.id == id)) else {
        frame_properties(ui, editor, node, t, commands);
        return;
    };
    let clip = &s.clip;
    let key = |field: &str| format!("{}.{field}", clip.id);
    ui.label(egui::RichText::new(name(titles, shown, s)).strong());
    if let Some(warning) = s.trim_warning() {
        ui.label(egui::RichText::new(warning).color(t.danger));
    }
    if s.source_changed() {
        ui.label(egui::RichText::new("Source changed since this clip was rendered.").color(t.danger));
    }
    match s.rendered_sources.as_slice() {
        [] => {
            ui.label(egui::RichText::new("No recorded source for this clip in the selected history.").color(t.text_muted).small());
        }
        [source] => {
            ui.label(egui::RichText::new("Last rendered source").color(t.text_muted).small()).on_hover_text(source.hash.to_string());
        }
        sources => {
            ui.label(egui::RichText::new(format!("Last rendered sources: {} batch items", sources.len())).color(t.text_muted).small());
        }
    }
    if !s.wired {
        ui.label(egui::RichText::new("Its media isn't wired in, so it doesn't play. Wire it in again, or delete the clip.").color(t.text_muted).small());
    }
    let still = clip.input == Input::Stills;
    let end = s.end_us();
    let limit = seconds(s.media_us.unwrap_or(timeline::MAX_US));
    egui::Grid::new("timeline_clip").num_columns(2).show(ui, |ui| {
        ui.label("Track");
        let current = track_name(tracks, clip.track);
        egui::ComboBox::from_id_salt("timeline_clip_track").selected_text(&current).show_ui(ui, |ui| {
            for track in (0..tracks.len()).filter_map(|k| u8::try_from(k).ok()).filter(|k| fits(tracks, *k, clip.input)) {
                if ui.selectable_label(track == clip.track, track_name(tracks, track)).clicked() && track != clip.track {
                    commands.push(update(editor, &clip.id, json!({"track": track})));
                }
            }
        });
        ui.end_row();
        if clip.track > 0 {
            if let Some(v) = number(ui, &mut editor.draft, &key("at"), "Plays at (s)", seconds(clip.at_us), 0.0..=seconds(timeline::MAX_US), 0.05, 2) {
                commands.push(update(editor, &clip.id, json!({"at_us": micros(v)})));
            }
            ui.end_row();
        }
        if still {
            let shows = end.unwrap_or(timeline::DEFAULT_STILL_US);
            if let Some(v) = number(ui, &mut editor.draft, &key("out"), "Shows for (s)", seconds(shows), seconds(MIN_US)..=limit, 0.05, 2) {
                commands.push(update(editor, &clip.id, json!({"out_us": micros(v).max(MIN_US)})));
            }
            ui.end_row();
        } else {
            let top = end.map_or(limit, |e| seconds(e.saturating_sub(MIN_US))).min(seconds(s.media_us.unwrap_or(timeline::MAX_US).saturating_sub(MIN_US)));
            if let Some(v) = number(ui, &mut editor.draft, &key("in"), "Start (s)", seconds(clip.in_us), 0.0..=top, 0.05, 2) {
                commands.push(update(editor, &clip.id, json!({"in_us": micros(v)})));
            }
            ui.end_row();
            match end {
                Some(e) => {
                    let bottom = seconds(clip.in_us.saturating_add(MIN_US));
                    if let Some(v) = number(ui, &mut editor.draft, &key("out"), "End (s)", seconds(e), bottom..=limit.max(bottom), 0.05, 2) {
                        let out = micros(v);
                        commands.push(update(editor, &clip.id, json!({"out_us": (Some(out) != s.media_us).then_some(out)})));
                    }
                }
                None => {
                    ui.label("End");
                    ui.label(egui::RichText::new("the media's end").color(t.text_muted));
                }
            }
            ui.end_row();
        }
        // Fades and the crossing are output time: the media's length at the clip's speed.
        let length = end.map_or(limit, |e| seconds(clip.played(e.saturating_sub(clip.in_us))));
        for (field, label, value) in [("fade_in_us", "Fade in (s)", clip.fade_in_us), ("fade_out_us", "Fade out (s)", clip.fade_out_us)] {
            if let Some(v) = number(ui, &mut editor.draft, &key(field), label, seconds(value), 0.0..=length, 0.05, 2) {
                commands.push(update(editor, &clip.id, json!({field: micros(v)})));
            }
            ui.end_row();
        }
        // Only the main track crosses one clip into the next.
        if clip.track == 0 {
            if let Some(v) = number(ui, &mut editor.draft, &key("transition"), "Into next (s)", seconds(clip.transition_us), 0.0..=length, 0.05, 2) {
                // Under the shortest transition is a cut.
                let us = micros(v);
                commands.push(update(editor, &clip.id, json!({"transition_us": if us < timeline::MIN_TRANSITION_US { 0 } else { us }})));
            }
            ui.end_row();
        }
        if !still {
            let most = f64::from(timeline::MAX_VOLUME) / 10.0;
            if let Some(v) = number(ui, &mut editor.draft, &key("volume"), "Volume (%)", f64::from(clip.volume) / 10.0, 0.0..=most, 1.0, 0) {
                let volume = (v * 10.0).round().clamp(0.0, f64::from(timeline::MAX_VOLUME)) as u16;
                commands.push(update(editor, &clip.id, json!({"volume": volume})));
            }
            ui.end_row();
        }
        if !still {
            let (low, high) = (f64::from(timeline::MIN_SPEED) / 10.0, f64::from(timeline::MAX_SPEED) / 10.0);
            if let Some(v) = number(ui, &mut editor.draft, &key("speed"), "Speed (%)", f64::from(clip.speed) / 10.0, low..=high, 1.0, 0) {
                let speed = (v * 10.0).round().clamp(f64::from(timeline::MIN_SPEED), f64::from(timeline::MAX_SPEED)) as u16;
                // Faster is shorter: fades and the crossing shrink to fit in the same change.
                let mut change = json!({"speed": speed});
                if let Some(e) = end {
                    let length = timeline::played_us(e.saturating_sub(clip.in_us), speed);
                    let (mut fade_in, mut fade_out) = (clip.fade_in_us, clip.fade_out_us);
                    if fade_in.saturating_add(fade_out) > length {
                        fade_in = fade_in.min(length / 2);
                        fade_out = fade_out.min(length.saturating_sub(fade_in));
                    }
                    let transition = clip.transition_us.min(length);
                    if (fade_in, fade_out, transition) != (clip.fade_in_us, clip.fade_out_us, clip.transition_us) {
                        change = json!({"speed": speed, "fade_in_us": fade_in, "fade_out_us": fade_out, "transition_us": transition});
                    }
                }
                commands.push(update(editor, &clip.id, change));
            }
            ui.end_row();
        }
    });
    if clip.input.visual() {
        placement(ui, editor, clip, t, commands);
    }
    let mut muted = clip.muted;
    if !still && ui.checkbox(&mut muted, "Mute").changed() {
        commands.push(update(editor, &clip.id, json!({"muted": muted})));
    }
    if !still && end.is_some() && clip.out_us.is_some() && ui.button("Play to the media's end").clicked() {
        commands.push(update(editor, &clip.id, json!({"out_us": null})));
    }
    let help = if clip.track == 0 {
        "Start and End are seconds into the clip's own media; fades are seconds of output. Into next cross-fades into the clip after it; the last clip's is ignored."
    } else {
        "Start and End are seconds into the clip's own media; fades are seconds of output. It plays from its start time over the main track."
    };
    ui.label(egui::RichText::new(help).color(t.text_muted).small());
}

/// Where a picture sits in the frame: its centre, size, crop, turn and opacity, in percent and
/// degrees; each field one `timeline.update`.
fn placement(ui: &mut egui::Ui, editor: &mut TimelineEditor, clip: &Clip, t: &Tokens, commands: &mut Commands) {
    let key = |field: &str| format!("{}.{field}", clip.id);
    let percent = |v: f64| v / 10.0;
    let thousandths = |v: f64, low: i64, high: i64| ((v * 10.0).round() as i64).clamp(low, high);
    let (low, high) = (i64::from(timeline::MIN_POS), i64::from(timeline::MAX_POS));
    ui.separator();
    ui.label(egui::RichText::new("In the frame").color(t.text_muted));
    egui::Grid::new("timeline_clip_place").num_columns(2).show(ui, |ui| {
        for (field, label, value) in [("pos_x", "Across (%)", clip.pos_x), ("pos_y", "Down (%)", clip.pos_y)] {
            if let Some(v) = number(ui, &mut editor.draft, &key(field), label, percent(f64::from(value)), percent(low as f64)..=percent(high as f64), 0.5, 1) {
                commands.push(update(editor, &clip.id, json!({field: thousandths(v, low, high)})));
            }
            ui.end_row();
        }
        let (small, large) = (i64::from(timeline::MIN_SCALE), i64::from(timeline::MAX_SCALE));
        if let Some(v) =
            number(ui, &mut editor.draft, &key("scale"), "Size (%)", percent(f64::from(clip.scale)), percent(small as f64)..=percent(large as f64), 0.5, 1)
        {
            commands.push(update(editor, &clip.id, json!({"scale": thousandths(v, small, large)})));
        }
        ui.end_row();
        // Opposite crops leave at least a tenth of the picture.
        let crops = [
            ("crop_left", "Crop left (%)", clip.crop_left, clip.crop_right),
            ("crop_right", "Crop right (%)", clip.crop_right, clip.crop_left),
            ("crop_top", "Crop top (%)", clip.crop_top, clip.crop_bottom),
            ("crop_bottom", "Crop bottom (%)", clip.crop_bottom, clip.crop_top),
        ];
        for (field, label, value, opposite) in crops {
            let most = 899i64.saturating_sub(i64::from(opposite)).max(0);
            if let Some(v) = number(ui, &mut editor.draft, &key(field), label, percent(f64::from(value)), 0.0..=percent(most as f64), 0.5, 1) {
                commands.push(update(editor, &clip.id, json!({field: thousandths(v, 0, most)})));
            }
            ui.end_row();
        }
        if let Some(v) = number(ui, &mut editor.draft, &key("rotation"), "Turn (°)", f64::from(clip.rotation), -180.0..=180.0, 1.0, 0) {
            commands.push(update(editor, &clip.id, json!({"rotation": v.round().clamp(-180.0, 180.0) as i16})));
        }
        ui.end_row();
        let unit = i64::from(timeline::UNIT);
        if let Some(v) = number(ui, &mut editor.draft, &key("opacity"), "Opacity (%)", percent(f64::from(clip.opacity)), 0.0..=percent(unit as f64), 1.0, 0) {
            commands.push(update(editor, &clip.id, json!({"opacity": thousandths(v, 0, unit)})));
        }
        ui.end_row();
    });
    if clip.placement() != timeline::Placement::IDENTITY && ui.button("Fit to the frame").clicked() {
        let p = timeline::Placement::IDENTITY;
        let [left, top, right, bottom] = p.crop;
        let set = json!({"pos_x": p.pos_x, "pos_y": p.pos_y, "scale": p.scale, "crop_left": left, "crop_top": top, "crop_right": right, "crop_bottom": bottom, "rotation": p.rotation, "opacity": p.opacity});
        commands.push(update(editor, &clip.id, set));
    }
}

/// The output frame's width and height, as the card's parameters say.
fn frame_size(node: &nodecraft_graph::Node) -> [f32; 2] {
    let side = |name: &str, default: u64| match node.params.get(name) {
        Some(Value::Unsigned(n)) => n.0 as f32,
        _ => default as f32,
    };
    [side("width", timeline::DEFAULT_WIDTH), side("height", timeline::DEFAULT_HEIGHT)]
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
        let quality = match node.params.get("quality") {
            Some(Value::Text(text)) => text.clone(),
            _ => "standard".into(),
        };
        ui.label("Quality");
        let named = |q: &str| match q {
            "draft" => "Draft (smallest file)",
            "high" => "High (largest file)",
            _ => "Standard",
        };
        let chosen = egui::ComboBox::from_id_salt("timeline_quality").selected_text(named(&quality)).show_ui(ui, |ui| {
            for option in timeline::QUALITIES {
                if ui.selectable_label(quality == option, named(option)).clicked() && quality != option {
                    commands.push(param("quality", Value::Text(option.into())));
                }
            }
        });
        chosen.response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Render quality"));
        ui.end_row();
    });
    let colour = match node.params.get("background") {
        Some(Value::Text(text)) => text.clone(),
        _ => "#000000".into(),
    };
    if let Some(Some(v)) = colour_field(ui, &mut editor.draft, "frame.background", "Background", Some(&colour), t) {
        commands.push(param("background", Value::Text(v)));
    }
    ui.label(
        egui::RichText::new("Pixels, 16 to 4096. A clip is fitted inside the frame unless it is placed; the background fills the rest.")
            .color(t.text_muted)
            .small(),
    );
}

/// One frame of output at the card's frame rate.
fn fps(node: &nodecraft_graph::Node) -> u64 {
    let rate = match node.params.get("fps") {
        Some(Value::Text(text)) => text.parse::<u64>().unwrap_or(30),
        _ => 30,
    };
    rate.clamp(1, 1000)
}

/// The output frame of `size` pixels as shown fitted inside `area`.
fn fitted(area: Rect, size: [f32; 2]) -> Rect {
    let scale = (area.width() / size[0].max(1.0)).min(area.height() / size[1].max(1.0));
    Rect::from_center_size(area.center(), Vec2::new(size[0], size[1]) * scale.max(0.0))
}

/// The corners of a picture `aspect` wide to high, placed as `placement` says in the output
/// frame of `size` pixels shown fitted inside `area`: its cropped part fitted to the frame, sized,
/// turned clockwise about its centre and centred where it goes, as the render places it.
fn outline(area: Rect, size: [f32; 2], aspect: f32, placement: timeline::Placement) -> Option<[Pos2; 4]> {
    let frame = fitted(area, size);
    let [left, top, right, bottom] = placement.crop.map(f32::from);
    let unit = f32::from(timeline::UNIT);
    let kept = (aspect * (unit - left - right) / (unit - top - bottom).max(1.0)).max(0.001);
    let fit = if kept >= frame.width() / frame.height().max(0.001) {
        Vec2::new(frame.width(), frame.width() / kept)
    } else {
        Vec2::new(frame.height() * kept, frame.height())
    };
    let half = fit * f32::from(placement.scale) / unit / 2.0;
    let centre = frame.min + Vec2::new(frame.width() * placement.pos_x as f32, frame.height() * placement.pos_y as f32) / unit;
    let (sin, cos) = f32::from(placement.rotation).to_radians().sin_cos();
    let corner = |x: f32, y: f32| centre + Vec2::new(x * cos - y * sin, x * sin + y * cos);
    let corners = [corner(-half.x, -half.y), corner(half.x, -half.y), corner(half.x, half.y), corner(-half.x, half.y)];
    corners.iter().all(|c| c.x.is_finite() && c.y.is_finite()).then_some(corners)
}

/// Seconds between ruler ticks: the first step at least `min` points apart.
fn tick_us(scale: f32, min: f32) -> u64 {
    const STEPS: [u64; 10] = [100_000, 250_000, 500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000, 30_000_000, 60_000_000, 300_000_000];
    STEPS.into_iter().find(|step| seconds(*step) as f32 * scale >= min).unwrap_or(600_000_000)
}

/// The ruler with its playhead, and a lane per track with its playing clips laid along it: drag
/// a clip to reorder it on the main track, to move it in time elsewhere or onto another lane
/// that takes its media, its edges to trim it, and the ruler to move the playhead.
#[allow(clippy::too_many_arguments)]
fn track(
    ui: &mut egui::Ui,
    editor: &mut TimelineEditor,
    shown: &[Shown],
    placed: &[Placed],
    tracks: &[TrackKind],
    total: u64,
    posters: &HashMap<String, (egui::TextureId, [u32; 2])>,
    titles: &HashMap<String, String>,
    node: &nodecraft_graph::Node,
    t: &Tokens,
    commands: &mut Commands,
) {
    let fps = fps(node);
    let area = ui.available_rect_before_wrap();
    // Lanes share at most two thirds of the height, each no taller than the main track's.
    let count = tracks.len().max(1) as f32;
    let row = (t.timeline_track_height).min(((area.height() * 2.0 / 3.0 - t.timeline_ruler_height) / count).max(t.timeline_trim_grip * 4.0));
    let height = t.timeline_ruler_height + row * count;
    let preview = Rect::from_min_max(area.min, Pos2::new(area.max.x, (area.max.y - height - t.control_gap * 2.0).max(area.min.y)));
    let ruler = Rect::from_min_size(Pos2::new(area.min.x, preview.max.y + t.control_gap), Vec2::new(area.width(), t.timeline_ruler_height));
    let lane_of = |track: u8| Rect::from_min_size(Pos2::new(area.min.x, ruler.max.y + row * f32::from(track)), Vec2::new(area.width(), row));
    let lane = Rect::from_min_max(ruler.left_bottom(), Pos2::new(area.max.x, ruler.max.y + row * count));
    let painter = ui.painter_at(area);
    editor.preview = preview;

    // What the playhead is over on the main track: the clip's thumbnail, as a still.
    let head_us = editor.playhead_us.max(1);
    let under = placed.iter().rev().find(|p| p.track == 0 && inside(p, head_us)).or_else(|| placed.first()).map(|p| p.index);
    if let Some((texture, size)) = under.and_then(|i| shown.get(i)).and_then(|s| posters.get(&s.clip.id)) {
        let aspect = size[0] as f32 / size[1].max(1) as f32;
        let fit = Vec2::new(preview.height() * aspect, preview.height());
        let fit = if fit.x > preview.width() { Vec2::new(preview.width(), preview.width() / aspect.max(0.01)) } else { fit };
        painter.image(*texture, Rect::from_center_size(preview.center(), fit), Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    } else {
        let why = if placed.is_empty() { "Nothing plays yet: wire media in and enable a clip." } else { "Run the card to render the timeline." };
        painter.text(preview.center(), egui::Align2::CENTER_CENTER, why, egui::FontId::proportional(t.body_size), t.text_muted);
    }
    // Where the selected picture sits in the frame, as an outline over the preview.
    if let Some(s) = editor.selected.as_ref().and_then(|id| shown.iter().find(|s| &s.clip.id == id)).filter(|s| s.clip.input.visual()) {
        let size = frame_size(node);
        let aspect = posters.get(&s.clip.id).map_or(size[0] / size[1].max(1.0), |(_, px)| px[0] as f32 / px[1].max(1) as f32);
        if let Some(corners) = outline(preview, size, aspect, s.clip.placement()) {
            let frame = fitted(preview, size);
            painter.with_clip_rect(preview).add(egui::Shape::closed_line(corners.to_vec(), egui::Stroke::new(1.5, t.accent)));
            painter.rect_stroke(frame, 0.0, egui::Stroke::new(1.0, t.panel_stroke), egui::StrokeKind::Outside);
        }
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
    for track in (0..tracks.len()).filter_map(|k| u8::try_from(k).ok()) {
        let at = lane_of(track).shrink2(Vec2::new(0.0, 1.0));
        painter.rect_filled(at, f32::from(t.card_radius) / 2.0, t.card_fill);
        let name = track_name(tracks, track);
        painter.text(at.right_top() + Vec2::new(-t.control_gap, 2.0), egui::Align2::RIGHT_TOP, name, egui::FontId::proportional(t.label_size), t.text_muted);
    }

    // The clips, with grips at their edges. The dragged one draws where it is now.
    let grip = t.timeline_trim_grip;
    for p in placed {
        let Some(s) = shown.get(p.index) else { continue };
        let (label, start_us) = (name(titles, shown, s), p.start_us);
        let row = lane_of(p.track);
        let block = Rect::from_min_max(Pos2::new(x(p.start_us), row.min.y + 2.0), Pos2::new(x(p.start_us.saturating_add(p.length_us)), row.max.y - 2.0));
        let selected = editor.selected.as_ref() == Some(&s.clip.id);
        let id = ui.id().with(("timeline_clip", &s.clip.id));
        let r = ui.interact(block, id, Sense::click_and_drag());
        r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, selected, format!("{label} on the track")));
        let mut grips = vec![(Grip::Move, r)];
        let edges = [(Grip::Start, block.min.x, "Trim start of"), (Grip::End, block.max.x, "Trim end of")];
        for (kind, at, what) in edges {
            // An end only trims once it is known; a still's start is always its start.
            if (kind == Grip::End && s.end_us().is_none()) || (kind == Grip::Start && s.clip.input == Input::Stills) {
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
                editor.drag = Some(Drag {
                    clip: s.clip.id.clone(),
                    grip: *kind,
                    from_x: p.x,
                    from_us: start_us,
                    now: before.clone(),
                    before,
                    media_us: s.media_us,
                    to: None,
                });
                editor.selected = Some(s.clip.id.clone());
                editor.caption = None;
            }
        }
        if grips.first().is_some_and(|(_, r)| r.clicked()) {
            editor.selected = Some(s.clip.id.clone());
            editor.caption = None;
        }

        let colour = match s.clip.input {
            Input::Clips => t.wire_video,
            Input::Audio => t.wire_audio,
            Input::Stills => t.wire_image,
        };
        let stroke = if selected { egui::Stroke::new(2.0, t.card_stroke_selected) } else { egui::Stroke::new(1.0, colour) };
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
            // A move goes onto the lane under the pointer when that lane takes its media.
            let over = (0..tracks.len()).filter_map(|k| u8::try_from(k).ok()).find(|k| lane_of(*k).y_range().contains(p.y));
            let onto = over.filter(|k| drag.grip == Grip::Move && fits(tracks, *k, drag.before.input)).unwrap_or(drag.before.track);
            // Catch on what else is on the timeline, unless Alt is held.
            let mut snap = Snap { points: Vec::new(), reach_us: micros(f64::from(grip / editor.scale.max(0.01))), fps };
            if !ui.input(|i| i.modifiers.alt) {
                let main = drag.before.track == 0;
                // A main-track clip's followers move with its trim, so they aren't caught on.
                let others = placed
                    .iter()
                    .filter(|q| shown.get(q.index).is_some_and(|s| s.clip.id != drag.clip) && !(main && q.track == 0 && q.start_us >= drag.from_us));
                snap.points = others.flat_map(|q| [q.start_us, q.start_us.saturating_add(q.length_us)]).chain([0, editor.playhead_us]).collect();
            }
            drag.trim(p.x - drag.from_x, editor.scale, onto, &snap);
            drag.to = None;
            if drag.grip == Grip::Move && onto == 0 && drag.before.track == 0 {
                // Lands before the first other playing main-track clip whose middle is right of the pointer.
                let main = |q: &&Placed| q.track == 0 && shown.get(q.index).is_some_and(|s| s.clip.id != drag.clip);
                let before = placed.iter().filter(main).find(|q| x(q.start_us.saturating_add(q.length_us / 2)) > p.x).map(|q| q.index);
                let order: Vec<usize> = (0..shown.len()).filter(|i| shown.get(*i).is_some_and(|s| s.clip.track == 0 && s.clip.id != drag.clip)).collect();
                drag.to = Some(before.and_then(|b| order.iter().position(|i| *i == b)).unwrap_or(order.len()));
                let end = placed.iter().filter(main).map(|q| q.start_us.saturating_add(q.length_us).saturating_sub(q.overlap_us)).max().unwrap_or(0);
                let at = before.and_then(|b| placed.iter().find(|q| q.index == b)).map_or(x(end), |q| x(q.start_us));
                let main_lane = lane_of(0);
                painter.line_segment([Pos2::new(at, main_lane.min.y), Pos2::new(at, main_lane.max.y)], egui::Stroke::new(2.0, t.accent));
            }
        }
        if !ui.input(|i| i.pointer.any_down()) {
            let (id, before, now) = (drag.clip.clone(), drag.before.clone(), drag.now.clone());
            match drag.grip {
                Grip::Move if now.track != before.track => {
                    let mut set = serde_json::Map::new();
                    set.insert("track".into(), json!(now.track));
                    if now.track > 0 {
                        set.insert("at_us".into(), json!(now.at_us));
                    }
                    commands.push(update(editor, &id, Json::Object(set)));
                }
                Grip::Move if now.track > 0 => {
                    if now.at_us != before.at_us {
                        commands.push(update(editor, &id, json!({"at_us": now.at_us})));
                    }
                }
                Grip::Move => {
                    let from = shown.iter().filter(|s| s.clip.track == 0).position(|s| s.clip.id == id);
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
                    if now.at_us != before.at_us {
                        set.insert("at_us".into(), json!(now.at_us));
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
        Shown { clip, wired: true, media_us: video_us, video: None, source_hash: None, rendered_sources: Vec::new() }
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
        assert_eq!(cut_at(&clips, &placed, 1_000_000, None), Some((0, 1_500_000)));
        assert_eq!(cut_at(&clips, &placed, 0, None), None, "a cut is strictly inside a clip");
    }

    #[test]
    fn trims_stay_inside_the_video_and_keep_the_clip_long_enough() {
        let clip = Clip { fade_in_us: 400_000, fade_out_us: 400_000, ..Clip::whole("a".into(), "n".into(), 0) };
        let drag =
            |grip| Drag { clip: "a".into(), grip, from_x: 0.0, from_us: 0, before: clip.clone(), media_us: Some(1_000_000), now: clip.clone(), to: None };
        let mut start = drag(Grip::Start);
        start.trim(-50.0, 100.0, 0, &Snap::default());
        assert_eq!(start.now.in_us, 0, "not before the video starts");
        start.trim(30.0, 100.0, 0, &Snap::default());
        assert_eq!((start.now.in_us, start.now.fade_in_us, start.now.fade_out_us), (300_000, 350_000, 350_000), "fades shrink to fit");
        start.trim(500.0, 100.0, 0, &Snap::default());
        assert_eq!(start.now.in_us, 900_000, "at least MIN_US long");
        let mut end = drag(Grip::End);
        end.trim(-25.0, 100.0, 0, &Snap::default());
        assert_eq!(end.now.out_us, Some(750_000));
        end.trim(80.0, 100.0, 0, &Snap::default());
        assert_eq!(end.now.out_us, None, "back at the video's end plays to the end");
        end.trim(f32::NAN, 0.0, 0, &Snap::default());
        assert_eq!(end.now.out_us, None);
    }

    #[test]
    fn other_tracks_play_at_their_own_start_and_the_output_runs_to_the_last_end() {
        let mut sound = shown("s", 0, Some(1_000_000), Some(4_000_000), 0);
        (sound.clip.input, sound.clip.track, sound.clip.at_us) = (Input::Audio, 2, 3_000_000);
        let mut still = shown("p", 0, Some(500_000), None, 0);
        (still.clip.input, still.clip.track, still.clip.at_us) = (Input::Stills, 1, 200_000);
        let clips = vec![shown("a", 0, None, Some(2_000_000), 0), sound, still];
        let (placed, total) = place(&clips);
        let at: Vec<(u8, u64, u64)> = placed.iter().map(|p| (p.track, p.start_us, p.length_us)).collect();
        assert_eq!(at, [(0, 0, 2_000_000), (2, 3_000_000, 1_000_000), (1, 200_000, 500_000)]);
        assert_eq!(total, 4_000_000, "past the main track, to the sound's end");
        // The selected clip splits where it is under the playhead, else the main track's; stills don't.
        assert_eq!(cut_at(&clips, &placed, 300_000, Some("p")), Some((0, 300_000)));
        assert_eq!(cut_at(&clips, &placed, 3_500_000, Some("s")), Some((1, 500_000)));
        assert_eq!(cut_at(&clips, &placed, 3_500_000, None), None);
        let tracks = [TrackKind::Video, TrackKind::Video, TrackKind::Audio];
        assert!(fits(&tracks, 0, Input::Stills) && fits(&tracks, 2, Input::Audio) && !fits(&tracks, 1, Input::Audio) && !fits(&tracks, 3, Input::Clips));
        assert_eq!((track_name(&tracks, 0), track_name(&tracks, 2)), ("Main track".to_owned(), "Sound track 2".to_owned()));
    }

    #[test]
    fn moves_off_the_main_track_shift_the_start_and_start_trims_keep_the_end() {
        let clip = Clip { track: 1, at_us: 1_000_000, in_us: 500_000, ..Clip::whole("a".into(), "n".into(), 0) };
        let drag = |grip| Drag {
            clip: "a".into(),
            grip,
            from_x: 0.0,
            from_us: 1_000_000,
            before: clip.clone(),
            media_us: Some(3_000_000),
            now: clip.clone(),
            to: None,
        };
        let mut moving = drag(Grip::Move);
        moving.trim(50.0, 100.0, 1, &Snap::default());
        assert_eq!((moving.now.track, moving.now.at_us), (1, 1_500_000));
        moving.trim(-500.0, 100.0, 1, &Snap::default());
        assert_eq!(moving.now.at_us, 0, "not before the output starts");
        moving.trim(10.0, 100.0, 0, &Snap::default());
        assert_eq!((moving.now.track, moving.now.at_us), (0, 0), "the main track plays in order");
        let mut start = drag(Grip::Start);
        start.trim(20.0, 100.0, 1, &Snap::default());
        assert_eq!((start.now.in_us, start.now.at_us), (700_000, 1_200_000), "the end stays put");
        start.trim(-200.0, 100.0, 1, &Snap::default());
        assert_eq!((start.now.in_us, start.now.at_us), (0, 500_000), "back to the media's start");
        // From the main track, a clip moved onto another starts where it was, plus the travel.
        let main = Clip { transition_us: 300_000, ..Clip::whole("b".into(), "n".into(), 0) };
        let mut lifted =
            Drag { clip: "b".into(), grip: Grip::Move, from_x: 0.0, from_us: 2_000_000, before: main.clone(), media_us: None, now: main, to: None };
        lifted.trim(100.0, 100.0, 1, &Snap::default());
        assert_eq!((lifted.now.track, lifted.now.at_us, lifted.now.transition_us), (1, 3_000_000, 0));
    }

    #[test]
    fn a_faster_clip_is_shorter_on_the_track_and_splits_and_trims_through_its_media() {
        let mut fast = shown("a", 1_000_000, None, Some(5_000_000), 0);
        fast.clip.speed = 2000;
        let clips = vec![fast.clone(), shown("b", 0, None, Some(1_000_000), 0)];
        let (placed, total) = place(&clips);
        assert_eq!((placed.first().map(|p| p.length_us), total), (Some(2_000_000), 3_000_000), "4 s of media at double speed");
        assert_eq!(cut_at(&clips, &placed, 500_000, None), Some((0, 2_000_000)), "half a second in is a second of media");
        let drag = |grip| Drag {
            clip: "a".into(),
            grip,
            from_x: 0.0,
            from_us: 0,
            before: fast.clip.clone(),
            media_us: Some(5_000_000),
            now: fast.clip.clone(),
            to: None,
        };
        let mut end = drag(Grip::End);
        end.trim(-50.0, 100.0, 0, &Snap::default());
        assert_eq!(end.now.out_us, Some(4_000_000), "half a second of output is a second of media");
        let mut start = drag(Grip::Start);
        start.trim(25.0, 100.0, 0, &Snap::default());
        assert_eq!(start.now.in_us, 1_500_000);
    }

    #[test]
    fn dragged_edges_catch_on_nearby_edges_and_the_playhead_else_whole_frames() {
        let snap = Snap { points: vec![0, 2_000_000, 3_000_000], reach_us: 90_000, fps: 25 };
        // The nearer catch of either edge wins.
        assert_eq!(snap.travel(950_000, &[1_000_000, 1_900_000]), 1_000_000, "the start catches at 2 s");
        assert_eq!(snap.travel(1_060_000, &[1_000_000, 1_900_000]), 1_100_000, "the end is nearer 3 s");
        // Out of reach, the edge lands on the nearest whole frame of output.
        assert_eq!(snap.travel(410_000, &[1_000_000]), 400_000);
        assert_eq!(snap.travel(-1_030_000, &[1_000_000]), -1_000_000, "and catches on the output's start");
        assert_eq!(Snap::default().travel(123, &[]), 123);
        let clip = Clip { track: 1, at_us: 1_000_000, ..Clip::whole("a".into(), "n".into(), 0) };
        let mut moving =
            Drag { clip: "a".into(), grip: Grip::Move, from_x: 0.0, from_us: 1_000_000, before: clip.clone(), media_us: Some(900_000), now: clip, to: None };
        moving.trim(104.0, 100.0, 1, &snap);
        assert_eq!(moving.now.at_us, 2_000_000);
        moving.trim(f32::INFINITY, 100.0, 1, &snap);
        assert_eq!(moving.now.at_us, 1_000_000, "a hostile travel is no travel");
    }

    #[test]
    fn the_outline_follows_the_placement() {
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(200.0, 100.0));
        // A square picture fitted into a 2:1 frame shown 200 × 100: 100 × 100, centred.
        let identity = outline(area, [400.0, 200.0], 1.0, timeline::Placement::IDENTITY);
        assert_eq!(identity, Some([Pos2::new(50.0, 0.0), Pos2::new(150.0, 0.0), Pos2::new(150.0, 100.0), Pos2::new(50.0, 100.0)]));
        let moved = timeline::Placement { pos_x: 0, pos_y: 1000, scale: 500, crop: [0, 0, 500, 0], ..timeline::Placement::IDENTITY };
        // Half the width cut off leaves 1:2; fitted 50 × 100, then halved, centred on the bottom-left corner.
        let corners = outline(area, [400.0, 200.0], 1.0, moved);
        assert_eq!(corners.map(|c| (c[0], c[2])), Some((Pos2::new(-12.5, 75.0), Pos2::new(12.5, 125.0))));
        let turned = outline(area, [400.0, 200.0], 1.0, timeline::Placement { rotation: 90, ..timeline::Placement::IDENTITY });
        let first = turned.map(|c| c[0]).unwrap_or(Pos2::ZERO);
        assert!((first.x - 150.0).abs() < 0.01 && first.y.abs() < 0.01, "turned clockwise: the top-left corner goes top-right, {first:?}");
        let node = |fps: &str| {
            let mut n = nodecraft_graph::Node::new(NodeId::from_bytes([4; 16]), timeline::ID);
            n.params.insert("fps".into(), Value::Text(fps.into()));
            n
        };
        assert_eq!((fps(&node("25")), fps(&node("x"))), (25, 30));
    }

    #[test]
    fn times_read_and_show_in_seconds() {
        assert_eq!((micros(1.25), micros(-3.0), micros(f64::NAN), micros(1e12)), (1_250_000, 0, 0, timeline::MAX_US));
        assert_eq!((duration(2_250_000), duration(62_500_000)), ("2.2 s".to_owned(), "1:02.5".to_owned()));
        assert_eq!((tick_us(100.0, 60.0), tick_us(1.0, 60.0)), (1_000_000, 60_000_000));
    }

    #[test]
    fn source_trim_warnings_only_apply_to_playing_measured_time_media() {
        let mut clip = shown("a", 1_500_000, Some(2_000_000), Some(1_000_000), 0);
        assert!(clip.trim_warning().is_some(), "start and end may both exceed a regenerated source");
        clip.media_us = None;
        assert!(clip.trim_warning().is_none(), "unknown length is not proof of a bad trim");
        clip.media_us = Some(1_000_000);
        clip.clip.input = Input::Stills;
        assert!(clip.trim_warning().is_none(), "still display length is not source duration");
        clip.clip.input = Input::Audio;
        assert!(clip.trim_warning().is_some());
        clip.clip.enabled = false;
        assert!(clip.trim_warning().is_none());
        clip.clip.enabled = true;
        clip.wired = false;
        assert!(clip.trim_warning().is_none());
    }

    #[test]
    fn an_actual_shorter_probe_warns_even_when_metadata_claims_the_trim_fits() {
        let mut clip = shown("a", 0, Some(1_500_000), Some(2_000_000), 0);
        assert!(clip.trim_warning().is_none(), "advertised duration alone says the trim fits");
        clip.measured_length(Some(1_000_000));
        assert!(clip.trim_warning().is_some(), "a ready actual probe takes precedence before run");
        clip.measured_length(None);
        assert!(clip.trim_warning().is_some(), "a pending probe does not discard a known measurement");
        assert_eq!(clip.clip.out_us, Some(1_500_000), "measurement never mutates the stored trim");
    }
}
