//! 3D captures (#112). `mesh.capture` adds a 3D photo or 3D view sheet node wired to a model,
//! its camera and style taken from the viewer's current view, as one undo step. The viewer's
//! saved orbit is separate from the new capture's parameters, so the capture repeats exactly
//! after the viewer moves on. Nothing here runs: the new node runs like any action.
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use nodecraft_graph::{Change, Changeset, Edge, Endpoint, Position};
use nodecraft_nodes::mesh::{PHOTO, TURNTABLE, VIEWS};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, Modality, NodeId, Number, ParamValues, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// The canvas extent positions may use (as `node.move`).
const MAX_COORDINATE: f32 = 10_000_000.0;
/// The steepest camera a capture takes, in degrees (its Elevation setting's range).
const MAX_PITCH: f64 = 88.0;

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Shot {
    Photo,
    Views,
    Turntable,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Style {
    Shaded,
    Unlit,
    Wireframe,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureParams {
    canvas: CanvasId,
    /// The model output to capture.
    from: Endpoint,
    shot: Shot,
    /// The new node's and connection's ids; the caller supplies them so replay stays
    /// deterministic.
    id: NodeId,
    edge: EdgeId,
    pos: Position,
    /// The viewer's camera: degrees round and above the model, and zoom (1 fits the model).
    #[serde(default)]
    yaw: Option<f64>,
    #[serde(default)]
    pitch: Option<f64>,
    #[serde(default)]
    zoom: Option<f64>,
    #[serde(default)]
    style: Option<Style>,
}

fn number(value: f64) -> Result<Value, Error> {
    Number::new(value).map(Value::Number)
}

/// `degrees` as an angle from -180 to 180, to the nearest degree.
fn angle(degrees: f64) -> f64 {
    let turned = (degrees.round() + 180.0).rem_euclid(360.0) - 180.0;
    if turned.is_finite() { turned } else { 0.0 }
}

fn capture(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CaptureParams = params(value)?;
    if !(p.pos.x.is_finite() && p.pos.y.is_finite() && p.pos.x.abs() <= MAX_COORDINATE && p.pos.y.abs() <= MAX_COORDINATE) {
        return Err(invalid("position is outside the canvas"));
    }
    let finite = |v: Option<f64>, name: &str| match v {
        Some(v) if !v.is_finite() => Err(Error::new(ErrorKind::InvalidInput, format!("{name} must be a finite number"))),
        other => Ok(other),
    };
    let (yaw, pitch, zoom) = (finite(p.yaw, "yaw")?, finite(p.pitch, "pitch")?, finite(p.zoom, "zoom")?);
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let source = canvas.nodes().get(&p.from.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    let output = source.outputs.iter().find(|port| port.name == p.from.port).ok_or_else(|| Error::new(ErrorKind::NotFound, "that output does not exist"))?;
    if output.port_type.modality != Modality::Mesh || output.port_type.many {
        return Err(invalid("a capture takes one 3D model"));
    }
    let mut supplied = ParamValues::new();
    if let Some(yaw) = yaw {
        supplied.insert(if p.shot == Shot::Photo { "yaw" } else { "start" }.into(), number(angle(yaw))?);
    }
    if let Some(pitch) = pitch {
        supplied.insert("pitch".into(), number(pitch.round().clamp(-MAX_PITCH, MAX_PITCH))?);
    }
    if let Some(zoom) = zoom {
        supplied.insert("zoom".into(), number(((zoom * 20.0).round() / 20.0).clamp(0.25, 16.0))?);
    }
    if let Some(style) = p.style {
        let style = match style {
            Style::Shaded => "shaded",
            Style::Unlit => "unlit",
            Style::Wireframe => "wireframe",
        };
        supplied.insert("style".into(), Value::Text(style.into()));
    }
    let kind = match p.shot {
        Shot::Photo => PHOTO,
        Shot::Views => VIEWS,
        Shot::Turntable => TURNTABLE,
    };
    let mut node = c.kinds.get(kind)?.instantiate_instance(p.id, BTreeMap::new(), &supplied)?;
    node.pos = p.pos;
    let changes = Changeset::new(vec![
        Change::AddNode { canvas: p.canvas, node },
        Change::Connect { canvas: p.canvas, edge: Edge { id: p.edge, from: p.from, to: Endpoint { node: p.id, port: "model".into() }, order: 0 } },
    ]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"id": p.id}), gesture: None })
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    capture(c, value).map(|_| ())
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    let coordinate = json!({"type": "number", "minimum": -MAX_COORDINATE, "maximum": MAX_COORDINATE});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "from", "shot", "id", "edge", "pos"],
        "properties": {"canvas": uuid, "id": uuid, "edge": uuid,
            "pos": {"type": "object", "required": ["x", "y"], "additionalProperties": false, "properties": {"x": coordinate, "y": coordinate}},
            "from": {"type": "object", "required": ["node", "port"], "additionalProperties": false, "properties": {"node": uuid, "port": {"type": "string", "maxLength": 256}}},
            "shot": {"enum": ["photo", "views", "turntable"]},
            "yaw": {"type": "number", "description": "Degrees round the model; 0 looks at its front, along -z."},
            "pitch": {"type": "number", "description": "Degrees above the model, -88 to 88."},
            "zoom": {"type": "number", "description": "1 fits the model; larger is closer (0.25 to 16)."},
            "style": {"enum": ["shaded", "unlit", "wireframe"]}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "mesh.capture",
        label: "Capture 3D view",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, from: {node, port}, shot: \"photo\"|\"views\"|\"turntable\", id: new NodeId, edge: new EdgeId, pos: {x, y}, yaw?, pitch?, zoom?, style?} -> {id}; adds a 3D photo (shot photo) or 3D view sheet (shot views: 4, 8 or 16 views round the model, the first at yaw) or a 3D turntable video (shot turntable) wired to the model output `from`, as one undo step. yaw and pitch are degrees, zoom 1 fits the model; omitted ones keep the node's defaults. The camera is saved on the new node, so re-running it draws the same capture sequence. It does not run: run the new node as usual.",
        enabled,
        run: capture,
    }]
}

#[cfg(test)]
mod tests;
