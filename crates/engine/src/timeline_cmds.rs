//! The Timeline's commands (#109, #576). A Timeline node keeps its clips and tracks in its config
//! (see `nodecraft-nodes::timeline`); every video, sound and picture wired into it has a clip,
//! stored or automatic, and media can be played more than once by splitting its clip.
//! `timeline.list` shows them all; every other command changes them as one undo step. A stored
//! clip whose wire is gone is still listed (and not played) until it is deleted or wired again.
//! Editing stores the automatic clips too, so each keeps its place from then on. Nothing here runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Node};
use nodecraft_nodes::timeline::{self, CONFIG, Clip, ID, Input, MAX_CLIPS, MAX_TRACKS, Timeline, Track, TrackKind, Wire};
use nodecraft_types::{AssetRef, CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

fn not_found(what: &str) -> Error {
    Error::new(ErrorKind::NotFound, format!("{what} does not exist"))
}

fn editor<'a>(c: &'a Context<'_>, canvas: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    let node = c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))?.nodes().get(&id).ok_or_else(|| not_found("document entity"))?;
    if node.kind == ID { Ok(node) } else { Err(invalid("only a Timeline node has clips")) }
}

/// The clips a Timeline node stores, checked.
pub fn stored(node: &Node) -> Result<Timeline, Error> {
    Timeline::parse(timeline::stored(node)?).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the node's clips can't be read: {}", e.message)))
}

/// Most inputs the editor lists from its wires; a run refuses more clips than `MAX_CLIPS` anyway.
const MAX_INPUTS: usize = 4 * MAX_CLIPS;

/// The media type and source kind of what `input` takes.
fn takes(input: Input) -> (&'static str, &'static str) {
    match input {
        Input::Clips => ("video/", "source.video"),
        Input::Audio => ("audio/", "source.audio"),
        Input::Stills => ("image/", "source.image"),
    }
}

fn media(value: &Value, input: Input) -> Option<&AssetRef> {
    match value {
        Value::Media(asset) if asset.mime.essence().starts_with(takes(input).0) => Some(asset),
        _ => None,
    }
}

/// The media wired in now, in input order (port by port, as a run plans them), each with its
/// wire: a source node's asset, or each item of the upstream node's active result on the wired
/// port, so a wire that carries a list gives one entry per item, as a run does. A node with no
/// result yet counts as one entry, None. For the editor and clip identity; a run resolves its
/// inputs itself.
pub fn inputs(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Vec<(Wire, Option<AssetRef>)>, Error> {
    let canvas = c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))?;
    let incoming = nodecraft_exec::routing::incoming(canvas, c.kinds)?;
    let mut out = Vec::new();
    for input in Input::ALL {
        let feeds = incoming.feeds.get(&id).into_iter().flatten().filter(|(port, ..)| *port == input.port());
        for (_, _, from, from_port) in feeds {
            let items = canvas.nodes().get(from).map(|node| shown(node, from_port, input)).unwrap_or_else(|| vec![None]);
            out.extend(items.into_iter().map(|v| (Wire::new(input, from.to_string()), v.cloned())));
            if out.len() > MAX_INPUTS {
                out.truncate(MAX_INPUTS);
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// The media `node` sends on `port` now that `input` takes, one per value a run receives (a
/// batch's per-item lists flattened, as the planner collects them), and one None without a result.
fn shown<'a>(node: &'a Node, port: &str, input: Input) -> Vec<Option<&'a AssetRef>> {
    if node.kind == takes(input).1 {
        return vec![node.params.get("asset").and_then(|v| media(v, input))];
    }
    let result = node.outputs.iter().position(|p| p.name == port).and_then(|index| Some((node.history.iter().find(|g| Some(g.id) == node.active)?, index)));
    let Some(value) = result.and_then(|(result, index)| result.outputs.get(index).map(|v| (v, !result.items.is_empty()))) else {
        return vec![None];
    };
    match value {
        (Value::List(items), fanned) => items
            .iter()
            .flat_map(|item| match item {
                Value::List(inner) if fanned => inner.iter().collect::<Vec<_>>(),
                single => vec![single],
            })
            .take(MAX_INPUTS.saturating_add(1))
            .map(|v| media(v, input))
            .collect(),
        (single, _) => vec![media(single, input)],
    }
}

/// Each wire into the node, in input order.
pub fn wires(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Vec<Wire>, Error> {
    Ok(inputs(c, canvas, id)?.into_iter().map(|(wire, _)| wire).collect())
}

/// Every clip the node shows, stored and automatic.
pub fn listed(c: &Context<'_>, canvas: CanvasId, node: &Node) -> Result<Timeline, Error> {
    Ok(stored(node)?.with_sources(&wires(c, canvas, node.id)?))
}

/// The length of the video or sound `clip` plays, in microseconds, when its asset says. A
/// still has no length of its own.
fn media_length(inputs: &[(Wire, Option<AssetRef>)], clip: &Clip) -> Option<u64> {
    let nth = usize::try_from(clip.nth).ok()?;
    let (_, asset) = inputs.iter().filter(|(wire, _)| wire.input == clip.input && wire.origin == clip.origin).nth(nth)?;
    if clip.input == Input::Stills {
        return None;
    }
    asset.as_ref()?.meta.duration_ms?.checked_mul(1000)
}

fn store(canvas: CanvasId, node: &Node, timeline: Timeline) -> Result<Effect, Error> {
    let json = timeline.encode()?;
    let mut changed = node.clone();
    changed.config.insert(CONFIG.into(), Value::Text(json));
    if changed == *node {
        return Ok(Effect::None { result: json!({"changed": false}) });
    }
    Ok(Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas, node: changed }]), result: json!({"changed": true}), gesture: None })
}

fn with_result(effect: Effect, result: Json) -> Effect {
    match effect {
        Effect::Edit { changes, gesture, .. } => Effect::Edit { changes, result, gesture },
        other => other,
    }
}

fn index_of(timeline: &Timeline, id: &str) -> Result<usize, Error> {
    timeline.clips.iter().position(|c| c.id == id).ok_or_else(|| not_found("clip"))
}

fn always(_: &Context<'_>, _: &Json) -> Result<(), Error> {
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeParams {
    canvas: CanvasId,
    id: NodeId,
}

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: NodeParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let inputs = inputs(c, p.canvas, p.id)?;
    let wires: Vec<Wire> = inputs.iter().map(|(wire, _)| wire.clone()).collect();
    let shown = stored(node)?.with_sources(&wires);
    let mut clips = Vec::with_capacity(shown.clips.len());
    for clip in &shown.clips {
        let mut entry = serde_json::to_value(clip).map_err(|_| invalid("the clips can't be listed"))?;
        if let Json::Object(fields) = &mut entry {
            // Every field, defaults too, so a reader needn't know them.
            fields.insert("input".into(), json!(clip.input));
            fields.insert("track".into(), json!(clip.track));
            fields.insert("at_us".into(), json!(clip.at_us));
            fields.insert("wired".into(), Json::Bool(Timeline::is_wired(clip, &wires)));
            fields.insert("media_us".into(), media_length(&inputs, clip).map_or(Json::Null, Json::from));
        }
        clips.push(entry);
    }
    let tracks: Vec<Json> = std::iter::once(TrackKind::Video).chain(shown.tracks.iter().map(|t| t.kind)).map(|kind| json!({"kind": kind})).collect();
    let inputs: Vec<Json> = inputs.into_iter().map(|(wire, media)| json!({"origin": wire.origin, "input": wire.input, "media": media})).collect();
    Ok(Effect::None { result: json!({"clips": clips, "tracks": tracks, "inputs": inputs}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    canvas: CanvasId,
    id: NodeId,
    clips: Vec<Clip>,
    #[serde(default)]
    tracks: Vec<Track>,
}

fn set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SetParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    store(p.canvas, node, Timeline { clips: p.clips, tracks: p.tracks })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    canvas: CanvasId,
    id: NodeId,
    clip: String,
    set: Map<String, Json>,
}

fn update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpdateParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = listed(c, p.canvas, node)?;
    let at = index_of(&timeline, &p.clip)?;
    if ["id", "origin", "nth", "input"].iter().any(|f| p.set.contains_key(*f)) {
        return Err(invalid("a clip's id and media don't change: add the media again instead"));
    }
    let current = timeline.clips.get(at).ok_or_else(|| not_found("clip"))?;
    let mut merged = serde_json::to_value(current).map_err(|_| invalid("the clip can't be read"))?;
    let Json::Object(fields) = &mut merged else { return Err(invalid("the clip can't be read")) };
    // A clip moved to the main track joins its order; one moved off it crosses into nothing.
    let to_track = p.set.get("track").and_then(Json::as_u64);
    if to_track == Some(0) && !p.set.contains_key("at_us") {
        fields.insert("at_us".into(), json!(0));
    }
    if to_track.is_some_and(|t| t != 0) && !p.set.contains_key("transition_us") {
        fields.insert("transition_us".into(), json!(0));
    }
    for (key, value) in p.set {
        // A null end plays to the media's end.
        if value.is_null() && key == "out_us" {
            fields.remove(&key);
        } else {
            fields.insert(key, value);
        }
    }
    let changed: Clip = serde_json::from_value(merged).map_err(|_| invalid("the change isn't valid: a field is unknown or has the wrong type or value"))?;
    if let Some(slot) = timeline.clips.get_mut(at) {
        *slot = changed;
    }
    store(p.canvas, node, timeline)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipParams {
    canvas: CanvasId,
    id: NodeId,
    clip: String,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ClipParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let wires = wires(c, p.canvas, node.id)?;
    let mut timeline = stored(node)?.with_sources(&wires);
    let at = index_of(&timeline, &p.clip)?;
    let clip = timeline.clips.get(at).ok_or_else(|| not_found("clip"))?;
    // The last clip of wired media would come straight back as an automatic one.
    let others = timeline.clips.iter().filter(|c| c.input == clip.input && c.origin == clip.origin && c.nth == clip.nth).count();
    if Timeline::is_wired(clip, &wires) && others == 1 {
        return Err(invalid("wired media's only clip stays while it is wired in: disable it, or disconnect it"));
    }
    timeline.clips.remove(at);
    store(p.canvas, node, timeline)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MoveParams {
    canvas: CanvasId,
    id: NodeId,
    clip: String,
    /// The new place in playing order, 0 first; past the end means last.
    index: u32,
}

fn move_clip(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: MoveParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = listed(c, p.canvas, node)?;
    let at = index_of(&timeline, &p.clip)?;
    let clip = timeline.clips.remove(at);
    // `index` counts the clips on the same track: where the clip goes among them.
    let track = clip.track;
    let mut on_track = timeline.clips.iter().enumerate().filter(|(_, c)| c.track == track).map(|(i, _)| i);
    let to = usize::try_from(p.index).ok().and_then(|n| on_track.nth(n)).unwrap_or(timeline.clips.len());
    timeline.clips.insert(to, clip);
    store(p.canvas, node, timeline)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackAddParams {
    canvas: CanvasId,
    id: NodeId,
    kind: TrackKind,
}

fn track_add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: TrackAddParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = listed(c, p.canvas, node)?;
    if timeline.tracks.len() >= MAX_TRACKS {
        return Err(invalid("a timeline has at most 8 tracks besides the main one"));
    }
    timeline.tracks.push(Track { kind: p.kind });
    let track = timeline.tracks.len();
    let effect = store(p.canvas, node, timeline)?;
    Ok(with_result(effect, json!({"changed": true, "track": track})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrackParams {
    canvas: CanvasId,
    id: NodeId,
    track: u8,
}

fn track_remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: TrackParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = listed(c, p.canvas, node)?;
    if p.track == 0 {
        return Err(invalid("the main track stays"));
    }
    if timeline.kind_of(p.track).is_none() {
        return Err(not_found("track"));
    }
    if timeline.clips.iter().any(|c| c.track == p.track) {
        return Err(invalid("only an empty track can be deleted: move or delete its clips first"));
    }
    timeline.tracks.remove(usize::from(p.track).saturating_sub(1));
    for clip in timeline.clips.iter_mut().filter(|c| c.track > p.track) {
        clip.track = clip.track.saturating_sub(1);
    }
    store(p.canvas, node, timeline)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SplitParams {
    canvas: CanvasId,
    id: NodeId,
    clip: String,
    /// Where to cut, in microseconds into the clip's video.
    at_us: u64,
}

/// A clip id not yet in `timeline`: `base-2`, `base-3`, … (shortened to fit).
fn fresh_id(timeline: &Timeline, base: &str) -> String {
    let stem = base.get(..base.len().min(timeline::MAX_ID_BYTES.saturating_sub(12))).unwrap_or("clip");
    let mut n = 2u32;
    loop {
        let id = format!("{stem}-{n}");
        if timeline.get(&id).is_none() {
            return id;
        }
        n = n.saturating_add(1);
    }
}

fn split(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SplitParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let inputs = inputs(c, p.canvas, p.id)?;
    let wires: Vec<Wire> = inputs.iter().map(|(wire, _)| wire.clone()).collect();
    let mut timeline = stored(node)?.with_sources(&wires);
    let at = index_of(&timeline, &p.clip)?;
    let first = timeline.clips.get(at).ok_or_else(|| not_found("clip"))?.clone();
    if first.input == Input::Stills {
        return Err(invalid("a still has no frames to cut: set its length, or add it again"));
    }
    // Without a known end the second part plays to the media's end; a cut past it is reported
    // by the run, which probes it.
    let end = first.out_us.or_else(|| media_length(&inputs, &first)).unwrap_or(u64::MAX);
    if p.at_us <= first.in_us || p.at_us >= end {
        return Err(invalid("split a clip between its start and its end"));
    }
    // The cut is a plain cut: the first part keeps the fade in, the second the fade out and the
    // transition into what follows, each no longer than its part.
    let (before, after) = (p.at_us.saturating_sub(first.in_us), end.saturating_sub(p.at_us));
    let second = Clip {
        id: fresh_id(&timeline, &first.id),
        // On a track beyond the main one the second part starts where the first now ends.
        at_us: if first.track == 0 { 0 } else { first.at_us.saturating_add(before) },
        in_us: p.at_us,
        fade_in_us: 0,
        fade_out_us: first.fade_out_us.min(after),
        transition_us: first.transition_us.min(after),
        ..first.clone()
    };
    let first = Clip { out_us: Some(p.at_us), fade_in_us: first.fade_in_us.min(before), fade_out_us: 0, transition_us: 0, ..first };
    let second_id = second.id.clone();
    if let Some(slot) = timeline.clips.get_mut(at) {
        *slot = first;
    }
    timeline.clips.insert(at.saturating_add(1), second);
    let effect = store(p.canvas, node, timeline)?;
    Ok(with_result(effect, json!({"changed": true, "clip": second_id})))
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}

fn clip_id() -> Json {
    json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$"})
}

fn clip_fields() -> Map<String, Json> {
    let time = json!({"type": "integer", "minimum": 0, "maximum": timeline::MAX_US});
    props(json!({
        "in_us": time, "fade_in_us": time, "fade_out_us": time, "transition_us": time,
        "out_us": {"type": ["integer", "null"], "minimum": 1, "maximum": timeline::MAX_US, "description": "Where the clip ends in its video; absent or null plays to the video's end."},
        "volume": {"type": "integer", "minimum": 0, "maximum": timeline::MAX_VOLUME, "description": "Thousandths: 1000 plays the sound as recorded."},
        "muted": {"type": "boolean"}, "enabled": {"type": "boolean"},
        "track": {"type": "integer", "minimum": 0, "maximum": MAX_TRACKS, "description": "0 is the main track; 1 the first of the timeline's other tracks."},
        "at_us": {"type": "integer", "minimum": 0, "maximum": timeline::MAX_US, "description": "Where the clip starts on a track beyond the main one, from the start of the output."},
    }))
}

fn clip_schema() -> Json {
    let mut properties = clip_fields();
    properties.insert("id".into(), clip_id());
    properties
        .insert("origin".into(), json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$", "description": "The id of the node the media comes from."}));
    properties.insert("nth".into(), json!({"type": "integer", "minimum": 0, "description": "Which of that node's media on this input, from 0."}));
    properties.insert("input".into(), json!({"enum": ["clips", "audio", "stills"], "description": "The port the media is wired into; clips when absent."}));
    json!({"type": "object", "additionalProperties": false, "required": ["id", "origin"], "properties": properties})
}

fn object(required: &[&str], mut properties: Map<String, Json>) -> Json {
    properties.insert("canvas".into(), uuid());
    properties.insert("id".into(), uuid());
    let mut required: Vec<&str> = required.to_vec();
    required.splice(0..0, ["canvas", "id"]);
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}

fn props(value: Json) -> Map<String, Json> {
    match value {
        Json::Object(map) => map,
        _ => Map::new(),
    }
}

fn list_schema() -> Json {
    object(&[], Map::new())
}

fn track_kind() -> Json {
    json!({"enum": ["video", "audio"]})
}

fn set_schema() -> Json {
    let track = json!({"type": "object", "additionalProperties": false, "required": ["kind"], "properties": {"kind": track_kind()}});
    object(
        &["clips"],
        props(
            json!({"clips": {"type": "array", "maxItems": MAX_CLIPS, "items": clip_schema()}, "tracks": {"type": "array", "maxItems": MAX_TRACKS, "items": track}}),
        ),
    )
}

fn track_add_schema() -> Json {
    object(&["kind"], props(json!({"kind": track_kind()})))
}

fn track_schema() -> Json {
    object(&["track"], props(json!({"track": {"type": "integer", "minimum": 1, "maximum": MAX_TRACKS}})))
}

fn update_schema() -> Json {
    object(&["clip", "set"], props(json!({"clip": clip_id(), "set": {"type": "object", "additionalProperties": false, "properties": clip_fields()}})))
}

fn clip_schema_only() -> Json {
    object(&["clip"], props(json!({"clip": clip_id()})))
}

fn move_schema() -> Json {
    object(&["clip", "index"], props(json!({"clip": clip_id(), "index": {"type": "integer", "minimum": 0, "maximum": MAX_CLIPS}})))
}

fn split_schema() -> Json {
    object(&["clip", "at_us"], props(json!({"clip": clip_id(), "at_us": {"type": "integer", "minimum": 1, "maximum": timeline::MAX_US}})))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "timeline.list",
            label: "List clips",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas, id: Timeline NodeId} -> {clips, tracks, inputs}; every clip: the stored ones and a whole clip for each wired video, sound or picture no stored clip names. A clip is {id, origin, input, nth, track, at_us, in_us, out_us?, fade_in_us, fade_out_us, volume, muted, transition_us, enabled, wired, media_us}: input the port its media is wired into (clips, audio or stills), track 0 the main track (its clips play in list order) or another track (its clips start at at_us), times in microseconds into its media, out_us absent to play to the end (a still's length), volume in thousandths, transition_us the cross-fade into the next main-track clip (0 cuts), wired whether its media is connected now and media_us the video's or sound's length when known. tracks lists every track as {kind: video|audio}, the main track first: video tracks draw over the ones before them, audio tracks mix in. inputs lists each wired item in input order as {origin, input, media}: the node it comes from, its port and the asset it shows now (null until that node has a result).",
            enabled: always,
            run: list,
        },
        CommandSpec {
            id: "node.timeline_set",
            label: "Set clips",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{canvas, id, clips: [clip, …], tracks?: [{kind}, …]} -> {changed}; replaces a Timeline node's clips (main-track ones in playing order) and its tracks beyond the main one, as one undo step. A clip is {id, origin, input?, nth?, track?, at_us?, in_us?, out_us?, fade_in_us?, fade_out_us?, volume?, muted?, transition_us?, enabled?} (see timeline.list). Wired media no clip names is added when the node runs: videos and pictures after the main track's clips, sounds on the first audio track.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "timeline.update",
            label: "Change clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{canvas, id, clip: clip id, set: {fields}} -> {changed}; changes some of a clip's fields (in_us, out_us (null plays to the end), fade_in_us, fade_out_us, volume, muted, transition_us, enabled, track, at_us), as one undo step. A clip moved onto the main track starts in its order (at_us 0), one moved off it loses its transition, unless set says otherwise. A clip's id and media don't change.",
            enabled: always,
            run: update,
        },
        CommandSpec {
            id: "timeline.move",
            label: "Move clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: move_schema,
            params_doc: "{canvas, id, clip: clip id, index} -> {changed}; moves a clip to `index` among the clips on its track (0 is first; past the end is last), as one undo step. On the main track that is playing order.",
            enabled: always,
            run: move_clip,
        },
        CommandSpec {
            id: "timeline.split",
            label: "Split clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: split_schema,
            params_doc: "{canvas, id, clip: clip id, at_us} -> {changed, clip}; cuts a video or sound clip in two at `at_us` into its media, as one undo step: the first part keeps the fade in, the new second part (its id returned) the fade out and transition, each shortened to fit its part; off the main track the second part starts where the first ends. A clip with no set end whose media hasn't run splits too; its second part plays to the end. A still doesn't split.",
            enabled: always,
            run: split,
        },
        CommandSpec {
            id: "timeline.remove",
            label: "Delete clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: clip_schema_only,
            params_doc: "{canvas, id, clip: clip id} -> {changed}; deletes a clip, as one undo step. Wired media's only clip stays (disable it instead); once its wire is gone, or another clip plays it, it can be deleted.",
            enabled: always,
            run: remove,
        },
        CommandSpec {
            id: "timeline.track_add",
            label: "Add track",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: track_add_schema,
            params_doc: "{canvas, id, kind: video|audio} -> {changed, track}; adds an empty track above the others, as one undo step, and returns its number. A video track draws its clips over the tracks before it; an audio track mixes its sounds in. At most 8 besides the main track.",
            enabled: always,
            run: track_add,
        },
        CommandSpec {
            id: "timeline.track_remove",
            label: "Delete track",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: track_schema,
            params_doc: "{canvas, id, track} -> {changed}; deletes an empty track (not the main one, 0), as one undo step; the tracks above it move down one.",
            enabled: always,
            run: track_remove,
        },
    ]
}

#[cfg(test)]
mod tests;
