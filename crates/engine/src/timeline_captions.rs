//! The Timeline's caption commands (#574): text drawn over the picture from a start to an end
//! time, stored with the clips (see `nodecraft-nodes::timeline::caption`). Each command changes
//! them as one undo step; `timeline.list` lists them. Nothing here runs.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::timeline_cmds::{always, editor, not_found, object, props, store, stored, with_result};
use nodecraft_nodes::timeline::caption::{self, Caption, MAX_CAPTION_BYTES, MAX_CAPTIONS};
use nodecraft_types::{CanvasId, Error, NodeId};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

/// Where the new caption goes; every other parameter is one of its fields, and a field the
/// caption doesn't have is refused as it is read.
#[derive(Deserialize)]
struct AddParams {
    canvas: CanvasId,
    id: NodeId,
    /// The new caption's id; a fresh one when absent.
    #[serde(default)]
    caption: Option<String>,
}

/// `fields` read as a caption with id `id`: the ones left out take their defaults.
fn read(id: String, mut fields: Map<String, Json>) -> Result<Caption, Error> {
    fields.insert("id".into(), Json::String(id));
    serde_json::from_value(Json::Object(fields)).map_err(|_| invalid("the caption isn't valid: a field is missing or unknown, or has the wrong type or value"))
}

fn add(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: AddParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = stored(node)?;
    if timeline.captions.len() >= MAX_CAPTIONS {
        return Err(invalid("a timeline has at most 64 captions"));
    }
    let mut fields = value.as_object().cloned().unwrap_or_default();
    for key in ["canvas", "id", "caption"] {
        fields.remove(key);
    }
    let id = match p.caption {
        Some(id) if timeline.captions.iter().any(|c| c.id == id) => {
            return Err(Error::new(nodecraft_types::ErrorKind::Conflict, "a caption with that id exists"));
        }
        Some(id) => id,
        None => Caption::fresh_id(&timeline.captions),
    };
    let added = read(id.clone(), fields)?;
    added.check()?;
    timeline.captions.push(added);
    let effect = store(p.canvas, node, timeline)?;
    Ok(with_result(effect, json!({"changed": true, "caption": id})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    canvas: CanvasId,
    id: NodeId,
    caption: String,
    set: Map<String, Json>,
}

fn update(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: UpdateParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = stored(node)?;
    if p.set.contains_key("id") {
        return Err(invalid("a caption's id doesn't change: add another instead"));
    }
    let slot = timeline.captions.iter_mut().find(|c| c.id == p.caption).ok_or_else(|| not_found("caption"))?;
    let Json::Object(mut fields) = serde_json::to_value(&*slot).map_err(|_| invalid("the caption can't be read"))? else {
        return Err(invalid("the caption can't be read"));
    };
    fields.extend(p.set);
    fields.remove("id");
    *slot = read(p.caption, fields)?;
    store(p.canvas, node, timeline)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveParams {
    canvas: CanvasId,
    id: NodeId,
    caption: String,
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: RemoveParams = params(value)?;
    let node = editor(c, p.canvas, p.id)?;
    let mut timeline = stored(node)?;
    let at = timeline.captions.iter().position(|c| c.id == p.caption).ok_or_else(|| not_found("caption"))?;
    timeline.captions.remove(at);
    store(p.canvas, node, timeline)
}

fn caption_id() -> Json {
    json!({"type": "string", "pattern": "^[A-Za-z0-9_-]{1,64}$"})
}

/// A caption's fields besides its id, for schemas.
fn fields() -> Map<String, Json> {
    let time = json!({"type": "integer", "minimum": 0, "maximum": nodecraft_nodes::timeline::MAX_US});
    let color = json!({"type": "string", "pattern": "^#([0-9A-Fa-f]{6}|[0-9A-Fa-f]{8})$"});
    props(json!({
        "text": {"type": "string", "minLength": 1, "maxLength": MAX_CAPTION_BYTES, "description": "At most 400 bytes and 8 lines; lines break only at newlines."},
        "start_us": time, "end_us": time,
        "font": {"type": "string", "minLength": 1, "maxLength": caption::MAX_FONT_BYTES, "description": "A font name; one that isn't available draws in the bundled Lato."},
        "size": {"type": "integer", "minimum": caption::MIN_CAPTION_SIZE, "maximum": caption::MAX_CAPTION_SIZE, "description": "In output pixels."},
        "align": {"enum": ["left", "center", "right"]},
        "position": {"enum": ["top", "middle", "bottom"]},
        "color": color, "stroke": color,
        "stroke_width": {"type": "integer", "minimum": 0, "maximum": caption::MAX_CAPTION_STROKE, "description": "How far the outline reaches past each letter, in output pixels; 0 draws none."},
        "enabled": {"type": "boolean"},
    }))
}

/// One stored caption, for `node.timeline_set`.
pub(crate) fn caption_schema() -> Json {
    let mut properties = fields();
    properties.insert("id".into(), caption_id());
    json!({"type": "object", "additionalProperties": false, "required": ["id", "text", "start_us", "end_us"], "properties": properties})
}

fn add_schema() -> Json {
    let mut properties = fields();
    properties.insert("caption".into(), caption_id());
    object(&["text", "start_us", "end_us"], properties)
}

fn update_schema() -> Json {
    object(&["caption", "set"], props(json!({"caption": caption_id(), "set": {"type": "object", "additionalProperties": false, "properties": fields()}})))
}

fn remove_schema() -> Json {
    object(&["caption"], props(json!({"caption": caption_id()})))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "timeline.caption_add",
            label: "Add caption",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: add_schema,
            params_doc: "{canvas, id: Timeline NodeId, text, start_us, end_us, caption?: id, font?, size?, align?, position?, color?, stroke?, stroke_width?, enabled?} -> {changed, caption}; adds a caption drawn over the picture from start_us until end_us (microseconds from the start of the output), as one undo step, and returns its id (caption-1, caption-2, … unless given). font defaults to lato (a font that isn't available draws in Lato), size to 48 output pixels, align to center (left|center|right), position to bottom (top|middle|bottom; a twentieth of the frame from its edge), color to #ffffff and stroke to #000000 (#rrggbb or #rrggbbaa); stroke_width (0 to 32 pixels, default 0) draws an outline that far past each letter. text is at most 400 bytes and 8 lines. A caption doesn't lengthen the output: past its end it is cut. At most 64.",
            enabled: always,
            run: add,
        },
        CommandSpec {
            id: "timeline.caption_update",
            label: "Change caption",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{canvas, id, caption: caption id, set: {fields}} -> {changed}; changes some of a caption's fields (text, start_us, end_us, font, size, align, position, color, stroke, stroke_width, enabled; see timeline.caption_add), as one undo step. Its id doesn't change.",
            enabled: always,
            run: update,
        },
        CommandSpec {
            id: "timeline.caption_remove",
            label: "Delete caption",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: remove_schema,
            params_doc: "{canvas, id, caption: caption id} -> {changed}; deletes a caption, as one undo step.",
            enabled: always,
            run: remove,
        },
    ]
}

#[cfg(test)]
mod tests;
