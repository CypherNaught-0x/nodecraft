//! Contextual representation gate frozen against reviewed v1 commit08586e3319f5577f1ac6f618103bfa4702e39043.
//! Unknown extension fields retain old serde behavior; only actual graph/value positions acquire
//! new meaning in v2 and are inspected here. Missing default fields never bypass this gate.
use nodecraft_types::Error;
use serde_json::Value;
pub(crate) fn validate(bytes: &[u8]) -> Result<(), Error> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| crate::invalid("invalid legacy JSON"))?;
    reserved::validate_value(&value)?;
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
        return Err(unsupported());
    }
    Ok(())
}
fn node(item: &Value) -> Result<(), Error> {
    if item.get("config").is_some() {
        return Err(unsupported());
    }
    if item.get("kind").and_then(Value::as_str) == Some("structure.note") {
        return Err(unsupported());
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
    if item.get("input_hash").is_some() {
        return Err(unsupported());
    }
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
        Some("bool" | "number" | "integer" | "unsigned") => return Err(unsupported()),
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
                return Err(unsupported());
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
            return Err(unsupported());
        }
        _ => {}
    }
    Ok(())
}

fn unsupported() -> Error {
    Error::new(nodecraft_types::ErrorKind::Unsupported, "project requires newer format support; v1 cannot store this representation")
}

// Same-project contextual ownership logic from reviewed Comments-v2 commit
// 4d2b9df618f8ef724c29e8ce837f5ee8a36fd5f2, kept frozen for future migrations.
mod reserved {
    use nodecraft_types::Error;
    use serde_json::Value;

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
            return Err(super::unsupported());
        }
        Ok(())
    }
    fn field(value: &Value) -> Result<(), Error> {
        if ["rect", "color", "collapsed"].iter().any(|key| value.get(key).is_some()) {
            return Err(super::unsupported());
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
            _ => return Err(super::unsupported()),
        }
        Ok(())
    }
}

/// Borrowed counterpart for graph-validated current models. Every current typed role
/// may now contain v2-only discussion members. Current serialization always emits those
/// defaulted fields, so even a legacy-shaped typed discussion requires the v2 writer.
/// Keep parity tests with the frozen raw v1 guard.
pub(crate) fn project(project: &nodecraft_graph::Project) -> Result<(), Error> {
    fn scalar(value: &nodecraft_types::Value) -> Result<(), Error> {
        use nodecraft_types::Value;
        match value {
            Value::Bool(_) | Value::Number(_) | Value::Integer(_) | Value::Unsigned(_) => Err(unsupported()),
            Value::List(values) => {
                for value in values {
                    scalar(value)?;
                }
                Ok(())
            }
            Value::Text(_) | Value::Media(_) => Ok(()),
        }
    }
    for canvas in project.canvases().values() {
        if !canvas.comments().is_empty() {
            return Err(unsupported());
        }
        for node in canvas.nodes().values() {
            if !node.config.is_empty() || node.kind == "structure.note" {
                return Err(unsupported());
            }
            if node.history.iter().any(|generation| generation.input_hash.is_some()) {
                return Err(unsupported());
            }
            for value in node.params.values().chain(node.history.iter().flat_map(|generation| &generation.outputs)) {
                scalar(value)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Generation, Node, Project};
    use nodecraft_types::{CanvasId, GenerationId, NodeId, ProjectId, Value as Wire};
    #[test]
    fn borrowed_current_role_guard_matches_frozen_raw_guard() {
        let c = CanvasId::from_bytes([1; 16]);
        let mut nodes = vec![Node::new(NodeId::from_bytes([2; 16]), "source.text")];
        let mut configured = nodes[0].clone();
        configured.config.insert("x".into(), Wire::Text("x".into()));
        nodes.push(configured);
        let mut note = nodes[0].clone();
        note.kind = "structure.note".into();
        nodes.push(note);
        let mut hashed = nodes[0].clone();
        hashed.history.push(Generation {
            id: GenerationId::from_bytes([5; 16]),
            outputs: vec![],
            input_hash: Some(nodecraft_types::Blake3::from_bytes([6; 32])),
        });
        nodes.push(hashed);
        for value in [
            serde_json::json!({"type":"bool","value":true}),
            serde_json::json!({"type":"number","value":1.5}),
            serde_json::json!({"type":"integer","value":"-1"}),
            serde_json::json!({"type":"unsigned","value":"1"}),
            serde_json::json!({"type":"text","value":"legacy"}),
        ] {
            let value: Wire = serde_json::from_value(value).unwrap();
            let value = Wire::List(vec![Wire::List(vec![value])]);
            let mut params = nodes[0].clone();
            params.params.insert("nested".into(), value.clone());
            nodes.push(params);
            let mut output = nodes[0].clone();
            output.history.push(Generation { id: GenerationId::from_bytes([3; 16]), outputs: vec![value], input_hash: None });
            nodes.push(output);
        }
        for node in nodes {
            let mut p = Project::new(ProjectId::from_bytes([4; 16]), "Parity").unwrap();
            Change::AddCanvas { canvas: Canvas::new(c, "C") }.apply(&mut p).unwrap();
            Change::AddNode { canvas: c, node }.apply(&mut p).unwrap();
            p.validate().unwrap();
            let json = crate::archive::bounded_json(&serde_json::json!({"version":1,"project":p}), crate::MAX_MANIFEST_BYTES).unwrap();
            assert_eq!(project(&p).is_ok(), validate(&json).is_ok());
        }
        let fixture = crate::decode(include_bytes!("../tests/fixtures/v1/comments-v1.ncraft")).unwrap();
        assert!(project(&fixture.project).is_err());
        let mut reader = zip::ZipArchive::new(std::io::Cursor::new(include_bytes!("../tests/fixtures/v1/comments-v1.ncraft"))).unwrap();
        let mut raw = String::new();
        std::io::Read::read_to_string(&mut reader.by_name("project.json").unwrap(), &mut raw).unwrap();
        assert!(validate(raw.as_bytes()).is_ok(), "genuine old bytes remain valid even though current typed serialization emits v2 defaults");
        let recovered = crate::journal::recover(include_bytes!("../tests/fixtures/v1/comments-v1.ncrj")).unwrap();
        assert!(project(&recovered.snapshot.project).is_err());
    }
}

#[cfg(test)]
mod measurements {
    #[test]
    #[ignore = "manual compatibility preflight cost sample, no durability claims"]
    fn compatibility_preflight_cost() {
        use nodecraft_graph::{Change, Changeset};
        use nodecraft_types::{CanvasId, NodeId, Value};
        let changes = Changeset::new(vec![Change::SetParam {
            canvas: CanvasId::from_bytes([1; 16]),
            id: NodeId::from_bytes([2; 16]),
            name: "sample".into(),
            value: Some(Value::Text("x".repeat(256 * 1024))),
        }]);
        let record = serde_json::json!({"checkpoint":nodecraft_types::Blake3::from_bytes([0;32]),"sequence":u64::MAX,"changes":changes});
        let bytes = crate::archive::bounded_json(&record, 16 * 1024 * 1024).unwrap();
        let start = std::time::Instant::now();
        for _ in 0..100 {
            crate::journal::validate_replay(&changes, &crate::AssetStore::default()).unwrap();
        }
        eprintln!(
            "compatibility sample: bounded record buffer={} bytes; 100 full replay preflights={:?}; includes existing canonical-budget traversal plus extra serialized buffer and raw JSON tree; not fsync/durability",
            bytes.len(),
            start.elapsed()
        );
    }
}
