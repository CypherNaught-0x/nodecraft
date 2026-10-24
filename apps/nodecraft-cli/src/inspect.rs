//! `commands` and `inspect`: what the engine can do, and what a project holds. Read-only.
use std::collections::BTreeSet;

use nodecraft_engine::Session;
use nodecraft_graph::{Canvas, Node};
use nodecraft_types::{Error, NodeId};
use serde_json::{Value, json};

/// Every engine command: id, label and shortcut; with `full`, its parameters too.
pub fn commands(session: &Session, full: bool) -> Value {
    let specs = session.registry().specs().map(|spec| {
        let mut entry = json!({"id": spec.id, "label": spec.label, "shortcut": spec.shortcut});
        if full && let Some(map) = entry.as_object_mut() {
            map.insert("params".into(), json!(spec.params_doc));
            map.insert("input_schema".into(), (spec.input_schema)());
        }
        entry
    });
    Value::Array(specs.collect())
}

pub fn commands_text(listed: &Value, full: bool) -> String {
    let mut out = String::new();
    for entry in listed.as_array().into_iter().flatten() {
        let id = entry["id"].as_str().unwrap_or_default();
        let label = entry["label"].as_str().unwrap_or_default();
        match entry["shortcut"].as_str() {
            Some(shortcut) => out.push_str(&format!("{id:<32} {label} ({shortcut})\n")),
            None => out.push_str(&format!("{id:<32} {label}\n")),
        }
        if full && let Some(params) = entry["params"].as_str().filter(|p| !p.is_empty()) {
            out.push_str(&format!("    {params}\n"));
        }
    }
    out
}

/// The model a node's `model` parameter names, when it names one.
pub fn model_of(node: &Node) -> Option<&str> {
    match node.params.get(nodecraft_nodes::generation::MODEL) {
        Some(nodecraft_types::Value::Text(model)) if !model.is_empty() => Some(model),
        _ => None,
    }
}

/// Whether a node has an active result to reuse.
pub fn has_result(node: &Node) -> bool {
    node.active.is_some_and(|active| node.history.iter().any(|generation| generation.id == active))
}

fn canvas_json(canvas: &Canvas, stale: &BTreeSet<NodeId>) -> Value {
    let nodes: Vec<Value> = canvas
        .nodes()
        .values()
        .map(|node| {
            json!({"id": node.id, "kind": node.kind, "title": node.title, "model": model_of(node),
                "result": has_result(node), "stale": stale.contains(&node.id)})
        })
        .collect();
    json!({"id": canvas.id(), "title": canvas.title(), "kind": canvas.kind(), "nodes": nodes, "edges": canvas.edges().len()})
}

/// The project's canvases and their nodes: kind, title, model, whether each has a result and is
/// stale.
pub fn inspect(session: &Session) -> Result<Value, Error> {
    let project = session.document();
    let mut canvases = Vec::new();
    for canvas in project.canvases().values() {
        let stale = nodecraft_exec::staleness::stale(canvas, session.kinds())?;
        canvases.push(canvas_json(canvas, &stale));
    }
    Ok(json!({"project": {"id": project.id(), "title": project.title()}, "canvases": canvases}))
}

pub fn inspect_text(report: &Value) -> String {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let mut out = format!("{}\n", text(&report["project"]["title"]));
    for canvas in report["canvases"].as_array().into_iter().flatten() {
        let nodes = canvas["nodes"].as_array().map_or(&[][..], Vec::as_slice);
        out.push_str(&format!(
            "\n{} “{}” {}: {} nodes, {} edges\n",
            text(&canvas["kind"]),
            text(&canvas["title"]),
            text(&canvas["id"]),
            nodes.len(),
            canvas["edges"].as_u64().unwrap_or(0)
        ));
        for node in nodes {
            let mut line = format!("  {}  {:<12} {}", text(&node["id"]), text(&node["kind"]), text(&node["title"]));
            if let Some(model) = node["model"].as_str() {
                line.push_str(&format!("  [{model}]"));
            }
            if node["stale"] == true {
                line.push_str("  stale");
            } else if node["result"] == true {
                line.push_str("  has a result");
            }
            out.push_str(line.trim_end());
            out.push('\n');
        }
    }
    out
}
