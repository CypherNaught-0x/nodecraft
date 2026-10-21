//! The Timeline's commands (#109). A Timeline node keeps its clips in its config (see
//! `nodecraft-nodes::timeline`); every video wired into it has a clip, stored or automatic, and
//! a video can be played more than once by splitting its clip. `timeline.list` shows them all, in
//! playing order; every other command changes them as one undo step. A stored clip whose wire is
//! gone is still listed (and not played) until it is deleted or its video is wired in again.
//! Editing stores the automatic clips too, so each keeps its place from then on. Nothing here runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Node};
use nodecraft_nodes::timeline::{self, CONFIG, Clip, ID, INPUT, MAX_CLIPS, Timeline};
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

/// Most videos the editor lists from its wires; a run refuses more clips than `MAX_CLIPS` anyway.
const MAX_INPUTS: usize = 4 * MAX_CLIPS;

fn video(value: &Value) -> Option<&AssetRef> {
    match value {
        Value::Media(asset) if asset.mime.essence().starts_with("video/") => Some(asset),
        _ => None,
    }
}

/// The videos wired in now, in input order, each with the node it comes from: a source video's
/// asset, or each video of the upstream node's active result on the wired port, so a wire that
/// carries a list gives one entry per item, as a run does. A node with no result yet counts as
/// one video, None. For the editor and clip identity; a run resolves its inputs itself.
pub fn inputs(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Vec<(String, Option<AssetRef>)>, Error> {
    let canvas = c.document.canvases().get(&canvas).ok_or_else(|| not_found("canvas"))?;
    let incoming = nodecraft_exec::routing::incoming(canvas, c.kinds)?;
    let feeds = incoming.feeds.get(&id).into_iter().flatten().filter(|(port, ..)| *port == INPUT);
    let mut out = Vec::new();
    for (_, _, from, from_port) in feeds {
        let videos = canvas.nodes().get(from).map(|node| shown(node, from_port)).unwrap_or_else(|| vec![None]);
        out.extend(videos.into_iter().map(|v| (from.to_string(), v.cloned())));
        if out.len() > MAX_INPUTS {
            out.truncate(MAX_INPUTS);
            break;
        }
    }
    Ok(out)
}

/// The videos `node` sends on `port` now, one per value a run receives (a batch's per-item lists
/// flattened, as the planner collects them), and one None without a result.
fn shown<'a>(node: &'a Node, port: &str) -> Vec<Option<&'a AssetRef>> {
    if node.kind == "source.video" {
        return vec![node.params.get("asset").and_then(video)];
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
            .map(video)
            .collect(),
        (single, _) => vec![video(single)],
    }
}

/// The node each video wired in comes from, in input order.
pub fn sources(c: &Context<'_>, canvas: CanvasId, id: NodeId) -> Result<Vec<String>, Error> {
    Ok(inputs(c, canvas, id)?.into_iter().map(|(origin, _)| origin).collect())
}

/// Every clip the node shows, stored and automatic, in playing order.
pub fn listed(c: &Context<'_>, canvas: CanvasId, node: &Node) -> Result<Timeline, Error> {
    Ok(stored(node)?.with_sources(&sources(c, canvas, node.id)?))
}

/// The length of the video `clip` plays, in microseconds, when its asset says.
fn video_length(inputs: &[(String, Option<AssetRef>)], clip: &Clip) -> Option<u64> {
    let nth = usize::try_from(clip.nth).ok()?;
    let (_, asset) = inputs.iter().filter(|(origin, _)| *origin == clip.origin).nth(nth)?;
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
    let sources: Vec<String> = inputs.iter().map(|(origin, _)| origin.clone()).collect();
    let shown = stored(node)?.with_sources(&sources);
    let mut clips = Vec::with_capacity(shown.clips.len());
    for clip in &shown.clips {
        let mut entry = serde_json::to_value(clip).map_err(|_| invalid("the clips can't be listed"))?;
        if let Json::Object(fields) = &mut entry {
            fields.insert("wired".into(), Json::Bool(Timeline::is_wired(clip, &sources)));
            fields.insert("video_us".into(), video_length(&inputs, clip).map_or(Json::Null, Json::from));
        }
        clips.push(entry);
    }
    let inputs: Vec<Json> = inputs.into_iter().map(|(origin, video)| json!({"origin": origin, "video": video})).collect();
    Ok(Effect::None { result: json!({"clips": clips, "inputs": inputs}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    canvas: CanvasId,
    id: NodeId,
    clips: Vec<Clip>,
}

fn set(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: SetParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    store(p.canvas, node, Timeline { clips: p.clips })
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
    if ["id", "origin", "nth"].iter().any(|f| p.set.contains_key(*f)) {
        return Err(invalid("a clip's id and video don't change: add the video again instead"));
    }
    let current = timeline.clips.get(at).ok_or_else(|| not_found("clip"))?;
    let mut merged = serde_json::to_value(current).map_err(|_| invalid("the clip can't be read"))?;
    let Json::Object(fields) = &mut merged else { return Err(invalid("the clip can't be read")) };
    for (key, value) in p.set {
        // A null end plays to the video's end.
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
    let sources = sources(c, p.canvas, node.id)?;
    let mut timeline = stored(node)?.with_sources(&sources);
    let at = index_of(&timeline, &p.clip)?;
    let clip = timeline.clips.get(at).ok_or_else(|| not_found("clip"))?;
    // The last clip of a wired video would come straight back as an automatic one.
    let others = timeline.clips.iter().filter(|c| c.origin == clip.origin && c.nth == clip.nth).count();
    if Timeline::is_wired(clip, &sources) && others == 1 {
        return Err(invalid("a video's only clip stays while the video is wired in: disable it, or disconnect the video"));
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
    let to = usize::try_from(p.index).unwrap_or(usize::MAX).min(timeline.clips.len());
    timeline.clips.insert(to, clip);
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
    let sources: Vec<String> = inputs.iter().map(|(origin, _)| origin.clone()).collect();
    let mut timeline = stored(node)?.with_sources(&sources);
    let at = index_of(&timeline, &p.clip)?;
    let first = timeline.clips.get(at).ok_or_else(|| not_found("clip"))?.clone();
    let end =
        first.out_us.or_else(|| video_length(&inputs, &first)).ok_or_else(|| invalid("the clip's end isn't known yet: run its video first, or set its end"))?;
    if p.at_us <= first.in_us || p.at_us >= end {
        return Err(invalid("split a clip between its start and its end"));
    }
    // The cut is a plain cut: the first part keeps the fade in, the second the fade out and the
    // transition into what follows.
    let second = Clip { id: fresh_id(&timeline, &first.id), in_us: p.at_us, fade_in_us: 0, ..first.clone() };
    let first = Clip { out_us: Some(p.at_us), fade_out_us: 0, transition_us: 0, ..first };
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
    }))
}

fn clip_schema() -> Json {
    let mut properties = clip_fields();
    properties.insert("id".into(), clip_id());
    properties
        .insert("origin".into(), json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$", "description": "The id of the node the video comes from."}));
    properties.insert("nth".into(), json!({"type": "integer", "minimum": 0, "description": "Which of that node's videos, from 0."}));
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

fn set_schema() -> Json {
    object(&["clips"], props(json!({"clips": {"type": "array", "maxItems": MAX_CLIPS, "items": clip_schema()}})))
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
            params_doc: "{canvas, id: Timeline NodeId} -> {clips, inputs}; every clip in playing order: the stored ones and a whole-video clip for each wired video no stored clip names. A clip is {id, origin, nth, in_us, out_us?, fade_in_us, fade_out_us, volume, muted, transition_us, enabled, wired, video_us}: times in microseconds into its video, out_us absent to play to the end, volume in thousandths, transition_us the cross-fade into the next clip (0 cuts), wired whether its video is connected now and video_us that video's length when known. inputs lists each wired video in input order as {origin, video}: the node it comes from and the video it shows now (null until that node has a result).",
            enabled: always,
            run: list,
        },
        CommandSpec {
            id: "node.timeline_set",
            label: "Set clips",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{canvas, id, clips: [clip, …]} -> {changed}; replaces a Timeline node's clips, in playing order, as one undo step. A clip is {id, origin, nth?, in_us?, out_us?, fade_in_us?, fade_out_us?, volume?, muted?, transition_us?, enabled?} (see timeline.list). Wired videos no clip names play after them when the node runs.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "timeline.update",
            label: "Change clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{canvas, id, clip: clip id, set: {fields}} -> {changed}; changes some of a clip's fields (in_us, out_us (null plays to the end), fade_in_us, fade_out_us, volume, muted, transition_us, enabled), as one undo step. A clip's id and video don't change.",
            enabled: always,
            run: update,
        },
        CommandSpec {
            id: "timeline.move",
            label: "Move clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: move_schema,
            params_doc: "{canvas, id, clip: clip id, index} -> {changed}; moves a clip to `index` in playing order (0 is first; past the end is last), as one undo step.",
            enabled: always,
            run: move_clip,
        },
        CommandSpec {
            id: "timeline.split",
            label: "Split clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: split_schema,
            params_doc: "{canvas, id, clip: clip id, at_us} -> {changed, clip}; cuts a clip in two at `at_us` into its video, as one undo step: the first part keeps the fade in, the new second part (its id returned) the fade out and transition. The clip's end must be known: set, or its video run.",
            enabled: always,
            run: split,
        },
        CommandSpec {
            id: "timeline.remove",
            label: "Delete clip",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: clip_schema_only,
            params_doc: "{canvas, id, clip: clip id} -> {changed}; deletes a clip, as one undo step. A wired video's only clip stays (disable it instead); once its wire is gone, or another clip plays it, it can be deleted.",
            enabled: always,
            run: remove,
        },
    ]
}

#[cfg(test)]
mod tests;
