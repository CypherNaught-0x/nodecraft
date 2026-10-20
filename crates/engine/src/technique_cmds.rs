//! Technique Builder (#93): check a workflow on a canvas and publish it as a Technique version
//! (`docs/decisions/2026-10-20-techniques.md`). Picking outputs traces everything they are made
//! from; picked inputs are the nodes a caller replaces. A published version is a frozen copy on a
//! canvas of kind technique, so later edits to the source canvas never change it; republishing
//! adds the next version beside it.
//!
//! `technique.check` reports why a selection can't be published yet without changing anything;
//! `technique.publish` refuses exactly when it reports a problem. Like the sheet commands, publish
//! derives every new id from one caller-supplied `seed`.
use crate::commands::{CommandSpec, Context, Effect, Phase, invalid, params};
use nodecraft_exec::planner::{Planned, RunMode, plan_with_targets};
use nodecraft_graph::{
    Canvas, CanvasKind, Change, Changeset, Edge, Endpoint, MAX_TECHNIQUE_NODES, MAX_TECHNIQUE_PORTS, Node, Technique, TechniqueParam, TechniquePort,
};
use nodecraft_types::{CanvasId, EdgeId, Error, ErrorKind, GenerationId, NodeId, TechniqueId};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Most jobs one run of a technique may make, counting each batch item: a published technique
/// can't fan out without bound.
pub const MAX_TECHNIQUE_JOBS: usize = 1000;
/// Most problems one check reports.
const MAX_PROBLEMS: usize = 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortParams {
    node: NodeId,
    /// The node's output to expose; may be left out when it has only one.
    #[serde(default)]
    port: Option<String>,
    /// Defaults to the node's title, or its port's name.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParamParams {
    node: NodeId,
    param: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckParams {
    canvas: CanvasId,
    #[serde(default)]
    inputs: Vec<PortParams>,
    outputs: Vec<PortParams>,
}

/// A selection, checked.
struct Checked {
    /// Everything the outputs are made from, the outputs and inputs included.
    nodes: BTreeSet<NodeId>,
    inputs: Vec<TechniquePort>,
    outputs: Vec<TechniquePort>,
    /// Jobs one run makes, when it could be planned.
    jobs: Option<usize>,
    problems: Vec<String>,
}

fn source<'a>(c: &'a Context<'_>, canvas: CanvasId) -> Result<&'a Canvas, Error> {
    let found = c.document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    if found.kind() != CanvasKind::Canvas {
        return Err(invalid("a technique is published from a plain canvas"));
    }
    Ok(found)
}

fn label(node: &Node) -> &str {
    if node.title.trim().is_empty() { &node.kind } else { &node.title }
}

fn port(canvas: &Canvas, p: &PortParams) -> Result<TechniquePort, Error> {
    let node = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("node {} is not on this canvas", p.node)))?;
    let output = match &p.port {
        Some(name) => node.outputs.iter().find(|output| &output.name == name),
        None if node.outputs.len() == 1 => node.outputs.first(),
        None => {
            let names: Vec<&str> = node.outputs.iter().map(|output| output.name.as_str()).collect();
            return Err(Error::new(ErrorKind::InvalidInput, format!("{} has {} outputs; pick one of: {}", label(node), names.len(), names.join(", "))));
        }
    }
    .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("{} has no output to expose", label(node))))?;
    let name = p.name.clone().unwrap_or_else(|| if node.title.trim().is_empty() { output.name.clone() } else { node.title.clone() });
    Ok(TechniquePort { name, description: p.description.clone(), node: node.id, port: output.name.clone(), port_type: output.port_type })
}

/// The outputs and everything upstream of them. The canvas is acyclic and the walk keeps a
/// seen-set, so it visits each node once.
fn trace(canvas: &Canvas, outputs: &[TechniquePort]) -> BTreeSet<NodeId> {
    let mut into: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for edge in canvas.edges().values() {
        into.entry(edge.to.node).or_default().push(edge.from.node);
    }
    let mut seen = BTreeSet::new();
    let mut queue: VecDeque<NodeId> = outputs.iter().map(|output| output.node).collect();
    while let Some(id) = queue.pop_front() {
        if seen.insert(id) {
            queue.extend(into.get(&id).into_iter().flatten().filter(|up| !seen.contains(up)));
        }
    }
    seen
}

fn check(c: &Context<'_>, canvas_id: CanvasId, inputs: &[PortParams], outputs: &[PortParams]) -> Result<Checked, Error> {
    let canvas = source(c, canvas_id)?;
    if inputs.len() > MAX_TECHNIQUE_PORTS || outputs.len() > MAX_TECHNIQUE_PORTS {
        return Err(Error::new(ErrorKind::Overflow, format!("a technique has at most {MAX_TECHNIQUE_PORTS} inputs and {MAX_TECHNIQUE_PORTS} outputs")));
    }
    let inputs = inputs.iter().map(|p| port(canvas, p)).collect::<Result<Vec<_>, _>>()?;
    let outputs = outputs.iter().map(|p| port(canvas, p)).collect::<Result<Vec<_>, _>>()?;
    let nodes = trace(canvas, &outputs);
    let mut problems = Vec::new();
    let name = |id: NodeId| canvas.nodes().get(&id).map_or("a missing node", label).to_owned();
    if outputs.is_empty() {
        problems.push("Pick at least one output.".to_owned());
    }
    let fed: BTreeSet<NodeId> = canvas.edges().values().map(|edge| edge.to.node).collect();
    let given: BTreeSet<NodeId> = outputs.iter().map(|output| output.node).collect();
    for input in &inputs {
        if fed.contains(&input.node) {
            problems.push(format!("{} has incoming wires, so it can't be an input.", name(input.node)));
        } else if given.contains(&input.node) {
            problems.push(format!("{} can't be both an input and an output.", name(input.node)));
        } else if !nodes.contains(&input.node) {
            problems.push(format!("{} doesn't feed any output.", name(input.node)));
        }
    }
    if nodes.len() > MAX_TECHNIQUE_NODES {
        problems.push(format!("The outputs are made from {} nodes; a technique holds at most {MAX_TECHNIQUE_NODES}.", nodes.len()));
    }
    // A run of the technique makes every result afresh, so plan as if each traced node were stale.
    let mut jobs = None;
    if problems.is_empty() {
        match plan_with_targets(canvas, c.kinds, &RunMode::WithUpstream, &given, &nodes, c.routing.routing.targets()) {
            Ok(Planned::Ready(plan)) => {
                let count = plan.steps.iter().try_fold(0usize, |n, step| n.checked_add(step.fanout.as_ref().map_or(1, |fanout| fanout.items.len().max(1))));
                match count {
                    Some(count) if count <= MAX_TECHNIQUE_JOBS => jobs = Some(count),
                    _ => problems.push(format!("One run would make more than {MAX_TECHNIQUE_JOBS} jobs; make its batches smaller.")),
                }
            }
            Ok(Planned::Blocked(blockers)) => problems.extend(blockers.iter().map(|blocker| blocker.message(canvas))),
            Err(error) => problems.push(error.message.clone()),
        }
    }
    problems.truncate(MAX_PROBLEMS);
    Ok(Checked { nodes, inputs, outputs, jobs, problems })
}

fn ports_json(ports: &[TechniquePort]) -> Json {
    ports.iter().map(|p| json!({"name": p.name, "node": p.node, "port": p.port, "type": p.port_type})).collect()
}

// ---- technique.check ----

fn run_check(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: CheckParams = params(value)?;
    if c.phase == Phase::Preview {
        source(c, p.canvas)?;
        return Ok(Effect::None { result: Json::Null });
    }
    let checked = check(c, p.canvas, &p.inputs, &p.outputs)?;
    Ok(Effect::None {
        result: json!({
            "ok": checked.problems.is_empty(),
            "problems": checked.problems,
            "nodes": checked.nodes,
            "inputs": ports_json(&checked.inputs),
            "outputs": ports_json(&checked.outputs),
            "jobs": checked.jobs,
        }),
    })
}

// ---- technique.publish ----

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishParams {
    canvas: CanvasId,
    /// The new canvas holding this version.
    published: CanvasId,
    seed: NodeId,
    technique: TechniqueId,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    inputs: Vec<PortParams>,
    outputs: Vec<PortParams>,
    #[serde(default)]
    params: Vec<ParamParams>,
}

/// A new id for the `n`th node, wire or result a publish makes from `seed`.
fn derive(seed: NodeId, what: &str, n: usize) -> [u8; 16] {
    let mut hash = blake3::Hasher::new_derive_key("nodecraft technique ids 2026-10-20");
    hash.update(seed.as_bytes());
    hash.update(what.as_bytes());
    hash.update(&(n as u64).to_le_bytes());
    let mut bytes = [0; 16];
    if let Some(head) = hash.finalize().as_bytes().first_chunk::<16>() {
        bytes = *head;
    }
    bytes
}

/// The next version number of `technique` in this project.
fn next_version(c: &Context<'_>, technique: TechniqueId) -> Result<u32, Error> {
    let latest =
        c.document.canvases().values().filter_map(Canvas::technique_info).filter(|info| info.id == technique).map(|info| info.version).max().unwrap_or(0);
    latest.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "this technique has no version numbers left"))
}

fn publish(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: PublishParams = params(value)?;
    if p.params.len() > MAX_TECHNIQUE_PORTS {
        return Err(Error::new(ErrorKind::Overflow, format!("a technique exposes at most {MAX_TECHNIQUE_PORTS} parameters")));
    }
    if c.document.canvases().contains_key(&p.published) {
        return Err(Error::new(ErrorKind::Conflict, "that canvas id is taken"));
    }
    let checked = check(c, p.canvas, &p.inputs, &p.outputs)?;
    if !checked.problems.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, format!("can't publish yet: {}", checked.problems.join(" "))));
    }
    let canvas = source(c, p.canvas)?;
    let version = next_version(c, p.technique)?;
    // Copies get new ids: ids are unique across a project's canvases.
    let ids: BTreeMap<NodeId, NodeId> = checked.nodes.iter().enumerate().map(|(n, id)| (*id, NodeId::from_bytes(derive(p.seed, "node", n)))).collect();
    let copy = |id: NodeId| ids.get(&id).copied().ok_or_else(|| Error::new(ErrorKind::Internal, "a traced node has no copy"));
    let mut nodes = Vec::with_capacity(ids.len());
    for (n, (id, new)) in ids.iter().enumerate() {
        let original = canvas.nodes().get(id).ok_or_else(|| Error::new(ErrorKind::Internal, "a traced node is missing"))?;
        let mut node = original.clone();
        node.id = *new;
        node.tag = None;
        node.field = None;
        // Only the result in use comes along, as the version's example.
        node.history = original.history.iter().filter(|generation| Some(generation.id) == original.active).cloned().collect();
        node.active = None;
        if let Some(generation) = node.history.first_mut() {
            generation.id = GenerationId::from_bytes(derive(p.seed, "generation", n));
            node.active = Some(generation.id);
        }
        nodes.push(node);
    }
    let mut edges = Vec::new();
    for edge in canvas.edges().values().filter(|edge| checked.nodes.contains(&edge.from.node) && checked.nodes.contains(&edge.to.node)) {
        edges.push(Edge {
            id: EdgeId::from_bytes(derive(p.seed, "edge", edges.len())),
            from: Endpoint { node: copy(edge.from.node)?, port: edge.from.port.clone() },
            to: Endpoint { node: copy(edge.to.node)?, port: edge.to.port.clone() },
            order: edge.order,
        });
    }
    let rebase =
        |ports: Vec<TechniquePort>| ports.into_iter().map(|port| Ok(TechniquePort { node: copy(port.node)?, ..port })).collect::<Result<Vec<_>, Error>>();
    let mut exposed = Vec::with_capacity(p.params.len());
    for param in &p.params {
        if !checked.nodes.contains(&param.node) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("node {} isn't part of this technique", param.node)));
        }
        let node = canvas.nodes().get(&param.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
        let instance = c.kinds.get(&node.kind)?.resolve_instance(node)?;
        let spec = instance
            .params
            .params
            .iter()
            .find(|spec| spec.name == param.param)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, format!("{} has no parameter {}", label(node), param.param)))?;
        exposed.push(TechniqueParam {
            name: param.name.clone().unwrap_or_else(|| spec.label.clone()),
            description: param.description.clone(),
            node: copy(param.node)?,
            param: param.param.clone(),
        });
    }
    let technique = Technique {
        id: p.technique,
        version,
        name: p.name.trim().to_owned(),
        description: p.description,
        category: p.category,
        tags: p.tags,
        inputs: rebase(checked.inputs)?,
        outputs: rebase(checked.outputs)?,
        params: exposed,
        source: Some(p.canvas),
    };
    let count = nodes.len();
    let title = format!("{} v{version}", technique.name);
    let published = Canvas::technique(p.published, title, technique, nodes, edges)?;
    let changes = Changeset::new(vec![Change::AddCanvas { canvas: published }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"canvas": p.published, "technique": p.technique, "version": version, "nodes": count}), gesture: None })
}

// ---- technique.list / technique.show / technique.remove ----

fn list(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    if !value.as_object().is_some_and(serde_json::Map::is_empty) && !value.is_null() {
        return Err(invalid("technique.list takes no parameters"));
    }
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Json::Null });
    }
    let mut versions: Vec<(&Canvas, &Technique)> =
        c.document.canvases().values().filter_map(|canvas| canvas.technique_info().map(|info| (canvas, info))).collect();
    versions.sort_by(|a, b| a.1.name.cmp(&b.1.name).then(a.1.id.cmp(&b.1.id)).then(a.1.version.cmp(&b.1.version)));
    let names = |ports: &[TechniquePort]| ports.iter().map(|port| port.name.clone()).collect::<Vec<_>>();
    let techniques: Vec<Json> = versions
        .into_iter()
        .map(|(canvas, info)| {
            json!({"canvas": canvas.id(), "technique": info.id, "version": info.version, "name": info.name, "category": info.category, "tags": info.tags,
                "inputs": names(&info.inputs), "outputs": names(&info.outputs), "nodes": canvas.nodes().len()})
        })
        .collect();
    Ok(Effect::None { result: json!({"techniques": techniques}) })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionParams {
    canvas: CanvasId,
}

fn version<'a>(c: &'a Context<'_>, canvas: CanvasId) -> Result<(&'a Canvas, &'a Technique), Error> {
    let found = c.document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    found.technique_info().map(|info| (found, info)).ok_or_else(|| invalid("that canvas isn't a published technique"))
}

fn show(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: VersionParams = params(value)?;
    let (canvas, info) = version(c, p.canvas)?;
    if c.phase == Phase::Preview {
        return Ok(Effect::None { result: Json::Null });
    }
    let nodes: Vec<Json> = canvas.nodes().values().map(|node| json!({"id": node.id, "kind": node.kind, "title": node.title})).collect();
    let mut technique = serde_json::to_value(info).map_err(|_| Error::new(ErrorKind::Internal, "technique could not be described"))?;
    if let Some(object) = technique.as_object_mut() {
        object.insert("canvas".into(), json!(canvas.id()));
        object.insert("nodes".into(), Json::Array(nodes));
        object.insert("edges".into(), json!(canvas.edges().len()));
    }
    Ok(Effect::None { result: technique })
}

fn remove(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: VersionParams = params(value)?;
    let (_, info) = version(c, p.canvas)?;
    let result = json!({"technique": info.id, "version": info.version});
    let changes = Changeset::new(vec![Change::RemoveCanvas { id: p.canvas }]);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result, gesture: None })
}

// ---- registration ----

/// Enabled when it would succeed; edits also need an editable document.
fn enabled(c: &Context<'_>, value: &Json, run: fn(&Context<'_>, &Json) -> Result<Effect, Error>) -> Result<(), Error> {
    if let Effect::Edit { .. } = run(c, value)? {
        c.editable()?;
    }
    Ok(())
}

fn uuid() -> Json {
    json!({"type": "string", "format": "uuid"})
}
fn schema(required: &[&str], properties: Json) -> Json {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": required, "properties": properties})
}
fn text(max: usize) -> Json {
    json!({"type": "string", "maxLength": max})
}
fn ports() -> Json {
    json!({"type": "array", "maxItems": MAX_TECHNIQUE_PORTS, "items": {"type": "object", "additionalProperties": false, "required": ["node"],
        "properties": {"node": uuid(), "port": text(128), "name": text(256), "description": text(4096)}}})
}

macro_rules! spec {
    ($id:literal, $label:literal, $schema:expr, $doc:literal, $run:ident) => {
        CommandSpec {
            id: $id,
            label: $label,
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: || $schema,
            params_doc: $doc,
            enabled: |c, v| enabled(c, v, $run),
            run: $run,
        }
    };
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "technique.check",
            "Check a technique",
            schema(&["canvas", "outputs"], json!({"canvas": uuid(), "inputs": ports(), "outputs": ports()})),
            "{canvas, inputs?: [{node, port?}], outputs: [{node, port?}]} -> {ok, problems: [text], nodes: [NodeId], inputs, outputs, jobs}; whether the outputs and everything they are made from can be published, with inputs as the nodes a caller replaces (no incoming wires). `port` may be left out for a node with one output. `jobs` counts one run's jobs, batch items included (at most 1000). Read-only.",
            run_check
        ),
        spec!(
            "technique.publish",
            "Publish a technique",
            schema(
                &["canvas", "published", "seed", "technique", "name", "outputs"],
                json!({"canvas": uuid(), "published": uuid(), "seed": uuid(), "technique": uuid(),
                    "name": {"type": "string", "minLength": 1, "maxLength": 256}, "description": text(4096), "category": text(256),
                    "tags": {"type": "array", "maxItems": 32, "items": {"type": "string", "minLength": 1, "maxLength": 64}},
                    "inputs": ports(), "outputs": ports(),
                    "params": {"type": "array", "maxItems": MAX_TECHNIQUE_PORTS, "items": {"type": "object", "additionalProperties": false, "required": ["node", "param"],
                        "properties": {"node": uuid(), "param": text(256), "name": text(256), "description": text(4096)}}}})
            ),
            "{canvas, published: new CanvasId, seed: new UUID, technique: TechniqueId, name, description?, category?, tags?, inputs?, outputs, params?: [{node, param, name?, description?}]} -> {canvas, technique, version, nodes}; publishes what technique.check accepts as the next version of `technique` (1 for a new id): a frozen copy on the new canvas, with each node's result in use as its example. `params` are parameters a caller may override. New ids derive from `seed`. Refused while technique.check reports a problem.",
            publish
        ),
        spec!(
            "technique.list",
            "List techniques",
            schema(&[], json!({})),
            "{} -> {techniques: [{canvas, technique, version, name, category, tags, inputs: [name], outputs: [name], nodes}]}; every published version, by name then version. Read-only.",
            list
        ),
        spec!(
            "technique.show",
            "Show a technique",
            schema(&["canvas"], json!({"canvas": uuid()})),
            "{canvas} -> {id, version, name, description, category, tags, inputs, outputs, params, source, canvas, nodes: [{id, kind, title}], edges}; one published version. Read-only.",
            show
        ),
        spec!(
            "technique.remove",
            "Remove a technique version",
            schema(&["canvas"], json!({"canvas": uuid()})),
            "{canvas} -> {technique, version}; removes one published version and its canvas, as one undo step.",
            remove
        ),
    ]
}

#[cfg(test)]
mod tests;
