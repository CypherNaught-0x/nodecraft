//! Contextual representation gate frozen against reviewed v1 commit08586e3319f5577f1ac6f618103bfa4702e39043.
//! Unknown extension fields retain old serde behavior; only actual graph/value positions acquire
//! new meaning in v2 and are inspected here. Missing default fields never bypass this gate.
use nodecraft_types::Error;
use serde_json::Value;
pub(crate) fn validate(bytes: &[u8]) -> Result<(), Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| crate::invalid("invalid legacy JSON"))?;
    crate::v2::validate_value(&value)?;
    if let Some(project) = value.get("project") {
        project_data(project)?;
    }
    if let Some(changes) = value.get("changes").and_then(|v| v.get("changes")).and_then(Value::as_array) {
        for change in changes {
            operation(change)?;
        }
    }
    Ok(())
}
fn project_data(project: &Value) -> Result<(), Error> {
    if let Some(canvases) = project.get("canvases").and_then(Value::as_object) {
        for canvas in canvases.values() {
            canvas_data(canvas)?;
        }
    }
    Ok(())
}
fn canvas_data(canvas: &Value) -> Result<(), Error> {
    if let Some(nodes) = canvas.get("nodes").and_then(Value::as_object) {
        for item in nodes.values() {
            node(item)?;
        }
    }
    if let Some(comments) = canvas.get("comments").and_then(Value::as_object) {
        for item in comments.values() {
            comment(item)?;
        }
    }
    Ok(())
}
fn comment(item: &Value) -> Result<(), Error> {
    if ["node", "replies", "resolved"].iter().any(|key| item.get(key).is_some()) {
        return Err(crate::invalid("discussion fields require format v2"));
    }
    Ok(())
}
fn node(item: &Value) -> Result<(), Error> {
    if item.get("config").is_some() {
        return Err(crate::invalid("node configuration requires format v2"));
    }
    if item.get("kind").and_then(Value::as_str) == Some("structure.note") {
        return Err(crate::invalid("Note nodes require format v2"));
    }
    if let Some(params) = item.get("params").and_then(Value::as_object) {
        for param in params.values() {
            value(param, 0)?;
        }
    }
    if let Some(history) = item.get("history").and_then(Value::as_array) {
        for entry in history {
            generation(entry)?;
        }
    }
    Ok(())
}
fn generation(item: &Value) -> Result<(), Error> {
    if let Some(outputs) = item.get("outputs").and_then(Value::as_array) {
        for output in outputs {
            value(output, 0)?;
        }
    }
    Ok(())
}
fn value(item: &Value, depth: usize) -> Result<(), Error> {
    if depth > 32 {
        return Err(crate::invalid("legacy value exceeds depth limit"));
    }
    match item.get("type").and_then(Value::as_str) {
        Some("bool" | "number" | "integer" | "unsigned") => return Err(crate::invalid("scalar values require format v2")),
        Some("list") => {
            if let Some(items) = item.get("value").and_then(Value::as_array) {
                for item in items {
                    value(item, depth.saturating_add(1))?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}
fn operation(item: &Value) -> Result<(), Error> {
    match item.get("type").and_then(Value::as_str) {
        Some("add_canvas") => {
            if let Some(canvas) = item.get("canvas") {
                canvas_data(canvas)?;
            }
        }
        Some("add_node" | "set_node" | "restore_node") => {
            if let Some(node_data) = item.get("node") {
                node(node_data)?;
            }
            if item.get("type").and_then(Value::as_str) == Some("restore_node") && item.get("pins").is_some() {
                return Err(crate::invalid("restored pins require format v2"));
            }
        }
        Some("set_param") => {
            if let Some(param) = item.get("value") {
                value(param, 0)?;
            }
        }
        Some("add_generation") => {
            if let Some(entry) = item.get("generation") {
                generation(entry)?;
            }
        }
        Some("put_comment") => {
            if let Some(root) = item.get("comment") {
                comment(root)?;
            }
        }
        Some("set_comment_text" | "set_comment_pin" | "set_comment_resolved" | "insert_comment_reply" | "remove_comment_reply" | "set_comment_reply_text") => {
            return Err(crate::invalid("discussion operations require format v2"));
        }
        _ => {}
    }
    Ok(())
}
