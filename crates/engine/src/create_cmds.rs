//! Creating nodes from a kind: what quick-add keys, the palette and agents use. The caller
//! supplies the new id so replay stays deterministic.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Position};
use nodecraft_types::{CanvasId, Error, NodeId, ParamValues};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateParams {
    pub canvas: CanvasId,
    pub id: NodeId,
    pub kind: String,
    pub pos: Position,
}

fn create(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: CreateParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let mut node = c.kinds.get(&p.kind)?.instantiate_instance(p.id, BTreeMap::new(), &ParamValues::new())?;
    node.pos = p.pos;
    let changes = Changeset::new(vec![Change::AddNode { canvas: p.canvas, node }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id}), gesture: None })
}

fn enabled_create(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    create(c, value).map(|_| ())
}

fn create_schema() -> Value {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "kind", "pos"],
        "properties": {"canvas": uuid, "id": uuid, "kind": {"type": "string", "maxLength": 128},
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.create",
        label: "Add node",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: create_schema,
        params_doc: "{canvas: CanvasId, id: NodeId, kind: string, pos: {x, y}}; a new node of `kind` with its default parameters. The caller supplies a new id. Quick-add keys are each kind's `quick_add` shortcut.",
        enabled: enabled_create,
        run: create,
    }]
}

#[cfg(test)]
mod tests {
    use crate::Session;
    use nodecraft_graph::{Canvas, Change, Project};
    use nodecraft_types::{CanvasId, ErrorKind, NodeId, ProjectId, Value};
    use serde_json::json;

    const C: CanvasId = CanvasId::from_bytes([1; 16]);

    fn session() -> Session {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Create").unwrap();
        Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        Session::new(project).unwrap()
    }

    #[test]
    fn creates_a_default_node_of_a_kind_as_one_undo_step() {
        let mut s = session();
        let before = s.document().clone();
        let id = NodeId::from_bytes([5; 16]);
        let params = json!({"canvas": C, "id": id, "kind": "source.text", "pos": {"x": 40.0, "y": -12.5}});
        s.enabled("node.create", &params).unwrap();
        assert_eq!(s.dispatch("node.create", &params).unwrap(), json!({"id": id}));
        let node = s.document().canvases().get(&C).unwrap().nodes().get(&id).unwrap().clone();
        assert_eq!((node.kind.as_str(), node.pos.x, node.pos.y), ("source.text", 40.0, -12.5));
        assert_eq!(node.params.get("text"), Some(&Value::Text(String::new())));
        assert_eq!(node.outputs.len(), 1);
        s.dispatch("edit.undo", &json!({})).unwrap();
        assert_eq!(s.document(), &before);
    }

    #[test]
    fn rejects_unknown_kinds_taken_ids_and_bad_positions() {
        let mut s = session();
        let id = NodeId::from_bytes([5; 16]);
        let ok = json!({"canvas": C, "id": id, "kind": "source.image", "pos": {"x": 0, "y": 0}});
        s.dispatch("node.create", &ok).unwrap();
        let before = s.document().clone();
        for (params, kind) in [
            (json!({"canvas": C, "id": NodeId::from_bytes([6; 16]), "kind": "no.such", "pos": {"x": 0, "y": 0}}), ErrorKind::NotFound),
            (json!({"canvas": C, "id": NodeId::from_bytes([6; 16]), "kind": "source.text", "pos": {"x": 1e30, "y": 0}}), ErrorKind::InvalidInput),
            (
                json!({"canvas": CanvasId::from_bytes([9; 16]), "id": NodeId::from_bytes([6; 16]), "kind": "source.text", "pos": {"x": 0, "y": 0}}),
                ErrorKind::NotFound,
            ),
        ] {
            assert!(s.enabled("node.create", &params).is_err(), "{params}");
            assert_eq!(s.dispatch("node.create", &params).unwrap_err().kind, kind, "{params}");
        }
        // The same id twice is rejected by the graph.
        assert!(s.dispatch("node.create", &ok).is_err());
        assert_eq!(s.document(), &before);
    }
}
