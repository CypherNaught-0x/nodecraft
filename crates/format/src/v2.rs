//! Comments-v2 ownership boundary. Tags/Fields expand only in coordinated format v3.
//! Inspect actual roles, retaining v1's harmless unknown extension-field behavior elsewhere.
use nodecraft_types::Error;
use serde_json::Value;
pub(crate) fn validate(bytes: &[u8]) -> Result<(), Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| crate::invalid("invalid v2 JSON"))?;
    validate_value(&value)
}
pub(crate) fn validate_value(value: &Value) -> Result<(), Error> {
    if let Some(canvases) = value.get("project").and_then(|p| p.get("canvases")).and_then(Value::as_object) {
        for canvas in canvases.values() {
            canvas_data(canvas)?;
        }
    }
    if let Some(changes) = value.get("changes").and_then(|v| v.get("changes")).and_then(Value::as_array) {
        for change in changes {
            operation(change)?;
        }
    }
    Ok(())
}
fn tag(value: &Value) -> Result<(), Error> {
    if value.get("color").is_some() {
        return Err(crate::invalid("tag colors require format v3"));
    }
    Ok(())
}
fn field(value: &Value) -> Result<(), Error> {
    if ["rect", "color", "collapsed"].iter().any(|key| value.get(key).is_some()) {
        return Err(crate::invalid("field geometry requires format v3"));
    }
    Ok(())
}
fn canvas_data(value: &Value) -> Result<(), Error> {
    if let Some(tags) = value.get("tags").and_then(Value::as_object) {
        for value in tags.values() {
            tag(value)?;
        }
    }
    if let Some(fields) = value.get("fields").and_then(Value::as_object) {
        for value in fields.values() {
            field(value)?;
        }
    }
    Ok(())
}
fn operation(value: &Value) -> Result<(), Error> {
    match value.get("type").and_then(Value::as_str) {
        Some("add_canvas") => {
            if let Some(canvas) = value.get("canvas") {
                canvas_data(canvas)?;
            }
        }
        Some("put_tag") => {
            if let Some(value) = value.get("tag") {
                tag(value)?;
            }
        }
        Some("put_field") => {
            if let Some(value) = value.get("field") {
                field(value)?;
            }
        }
        Some(
            "set_project_title"
            | "remove_canvas"
            | "set_canvas_title"
            | "add_node"
            | "restore_node"
            | "remove_node"
            | "set_node"
            | "move_node"
            | "set_node_title"
            | "set_param"
            | "add_generation"
            | "remove_generation"
            | "set_active_generation"
            | "connect"
            | "disconnect"
            | "set_edge"
            | "remove_field"
            | "remove_tag"
            | "put_comment"
            | "remove_comment"
            | "set_comment_text"
            | "set_comment_pin"
            | "set_comment_resolved"
            | "insert_comment_reply"
            | "remove_comment_reply"
            | "set_comment_reply_text",
        ) => {}
        _ => return Err(crate::invalid("operation requires a newer project format")),
    }
    Ok(())
}
