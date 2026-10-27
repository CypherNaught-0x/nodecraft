//! Read/edit the saved fields of an existing workflow card (#947).
use super::*;
use crate::commands::Context;
use nodecraft_comfy::{Exposed, ExposedKind};
use nodecraft_graph::Node;
use nodecraft_nodes::comfy_workflow::{self as card, Mapping, OutputName};
use nodecraft_types::{Modality, Value as Typed};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_CHOICES: usize = 512;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    canvas: CanvasId,
    id: NodeId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    node: String,
    input: String,
    name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    node: String,
    label: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Apply {
    canvas: CanvasId,
    id: NodeId,
    binding: String,
    inputs: Vec<Input>,
    outputs: Vec<Output>,
    #[serde(default)]
    output_names: Option<Vec<OutputName>>,
    #[serde(default)]
    disconnect: bool,
    #[serde(default)]
    clear_history: bool,
}
#[derive(Serialize)]
struct Choice {
    node: String,
    input: String,
    name: String,
    label: String,
    kind: &'static str,
    media: Option<Modality>,
    selected: bool,
}
fn source<'a>(c: &'a Context<'_>, canvas: CanvasId, id: NodeId) -> Result<&'a Node, Error> {
    let node = c
        .document
        .canvases()
        .get(&canvas)
        .and_then(|canvas| canvas.nodes().get(&id))
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "workflow card does not exist"))?;
    if node.kind != card::ID {
        return Err(invalid("select a ComfyUI workflow card"));
    }
    node.validate_payload()?;
    Ok(node)
}
fn catalog<'a>(c: &'a Context<'_>, node: &Node) -> Result<&'a NodeCatalog, Error> {
    match node.config.get(crate::provider_setup::COMFY_SERVER_CONFIG) {
        Some(Typed::Text(name)) => crate::comfy_server_cmds::catalog(c, name),
        _ => c.resources.get::<ComfyNodes>().map(|nodes| nodes.0.as_ref()),
    }
    .ok_or_else(|| Error::new(ErrorKind::Unsupported, "read this card's ComfyUI server nodes before editing its fields"))
}
fn binding(c: &Context<'_>, canvas: CanvasId, node: &Node) -> Result<String, Error> {
    let mut hash = blake3::Hasher::new_derive_key("NodeCraft ComfyUI field mapping v1");
    hash.update(c.document.id().to_string().as_bytes());
    hash.update(canvas.to_string().as_bytes());
    hash.update(node.id.to_string().as_bytes());
    // Parameter edits and unrelated document edits may continue while this modal is open.
    // Apply merges current values; only a changed workflow/mapping invalidates the selection.
    for key in [card::VERSION, card::WORKFLOW, card::MAPPING, crate::provider_setup::COMFY_SERVER_CONFIG] {
        let bytes = serde_json::to_vec(&node.config.get(key)).map_err(|_| invalid("mapping binding unavailable"))?;
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
    }
    Ok(hash.finalize().to_hex().to_string())
}
fn discover(c: &Context<'_>, node: &Node) -> Result<(ApiWorkflow, Exposed), Error> {
    let catalog = catalog(c, node)?;
    let mut workflow = card::bake_values(node, catalog)?;
    card::save_image_previews(&mut workflow, catalog)?;
    let exposed = nodecraft_comfy::discover(&workflow, catalog).map_err(|_| invalid("workflow fields exceed discovery limits"))?;
    if exposed.inputs.len() > MAX_CHOICES || exposed.outputs.len() > 64 {
        return Err(Error::new(ErrorKind::Overflow, "workflow has too many fields for this editor"));
    }
    Ok((workflow, exposed))
}
fn list(c: &Context<'_>, json: &Value) -> Result<Effect, Error> {
    let p: Target = params(json)?;
    let node = source(c, p.canvas, p.id)?;
    let old = card::mapping(node)?;
    let (_, exposed) = discover(c, node)?;
    let fields = exposed
        .inputs
        .into_iter()
        .map(|input| {
            let port = old.inputs.iter().find(|i| i.node == input.node && i.input == input.input);
            let parameter = old.params.iter().find(|i| i.node == input.node && i.input == input.input);
            let (kind, media) = match input.kind {
                ExposedKind::Media(media) => ("file", Some(media_modality(media))),
                ExposedKind::Text { .. } => ("text", None),
                ExposedKind::Int { .. } => ("integer", None),
                ExposedKind::Float { .. } => ("number", None),
                ExposedKind::Bool => ("boolean", None),
                ExposedKind::Choice { .. } => ("choice", None),
            };
            let name = port.map(|i| i.port.clone()).or_else(|| parameter.map(|i| i.spec.label.clone())).unwrap_or_else(|| {
                if media.is_some() { format!("input_{}_{}", input.node, input.input).to_ascii_lowercase() } else { bounded(&input.label, 256) }
            });
            Choice {
                node: input.node,
                input: input.input,
                name,
                label: bounded(&input.label, 256),
                kind,
                media,
                selected: port.is_some() || parameter.is_some(),
            }
        })
        .collect::<Vec<_>>();
    let outputs=exposed.outputs.into_iter().filter(|o|!o.preview && o.media.is_some()).map(|output| {
        let selected=old.outputs.iter().find(|o|o.node==output.node);
        json!({"node":output.node,"label":selected.map_or(output.label,|o|o.label.clone()),"media":output.media.map(media_modality),"selected":selected.is_some()})
    }).collect::<Vec<_>>();
    let result = json!({"binding":binding(c,p.canvas,node)?,"inputs":fields,"outputs":outputs,"input_order":old.inputs.iter().map(|i|json!([i.node,i.input])).chain(old.params.iter().map(|i|json!([i.node,i.input]))).collect::<Vec<_>>(),"output_order":old.outputs.iter().map(|o|&o.node).collect::<Vec<_>>(),"output_names":old.output_names,"ports":node.outputs,"history":node.history.len()});
    crate::commands::validate_json(&result)?;
    Ok(Effect::None { result })
}
fn media_modality(media: nodecraft_comfy::Media) -> Modality {
    match media {
        nodecraft_comfy::Media::Image => Modality::Image,
        nodecraft_comfy::Media::Video => Modality::Video,
        nodecraft_comfy::Media::Audio => Modality::Audio,
        nodecraft_comfy::Media::Mesh => Modality::Mesh,
    }
}
fn name(text: &str) -> Result<(), Error> {
    if text.trim().is_empty() || text.len() > 256 || text.chars().any(char::is_control) {
        return Err(invalid("field names must be nonempty, at most 256 bytes and contain no control characters"));
    }
    Ok(())
}
fn selected(mut exposed: Exposed, p: &Apply) -> Result<Exposed, Error> {
    if p.inputs.len() > 264 || p.outputs.is_empty() || p.outputs.len() > 64 {
        return Err(invalid("select bounded inputs and at least one result"));
    }
    let mut inputs = Vec::new();
    for wanted in &p.inputs {
        name(&wanted.name)?;
        let at = exposed
            .inputs
            .iter()
            .position(|i| i.node == wanted.node && i.input == wanted.input)
            .ok_or_else(|| invalid("an input is unavailable or selected twice"))?;
        let mut input = exposed.inputs.remove(at);
        // Media names are port identifiers; validate them via the generated descriptor later.
        if !matches!(input.kind, ExposedKind::Media(_)) {
            input.label = wanted.name.clone();
        }
        inputs.push(input);
    }
    let mut outputs = Vec::new();
    for wanted in &p.outputs {
        name(&wanted.label)?;
        let at = exposed
            .outputs
            .iter()
            .position(|o| o.node == wanted.node && !o.preview && o.media.is_some())
            .ok_or_else(|| invalid("a result is unavailable or selected twice"))?;
        let mut output = exposed.outputs.remove(at);
        output.label = wanted.label.clone();
        outputs.push(output);
    }
    Ok(Exposed { inputs, outputs, partners: exposed.partners })
}
fn preserve_parameter_names(mapping: &mut Mapping, old: &Mapping) -> Result<(), Error> {
    let mut names = old.params.iter().map(|p| p.spec.name.clone()).chain([card::MODEL.into()]).collect::<BTreeSet<_>>();
    for parameter in &mut mapping.params {
        if let Some(old) = old.params.iter().find(|p| p.node == parameter.node && p.input == parameter.input) {
            parameter.spec.name = old.spec.name.clone();
            continue;
        }
        if names.insert(parameter.spec.name.clone()) {
            continue;
        }
        let mut available = None;
        for index in 0..1000 {
            let candidate = format!("widget_{index}");
            if names.insert(candidate.clone()) {
                available = Some(candidate);
                break;
            }
        }
        parameter.spec.name = available.ok_or_else(|| invalid("cannot allocate a unique widget name"))?;
    }
    Ok(())
}
fn rename_edges(graph: &nodecraft_graph::Canvas, old: &Node, new: &Node, old_mapping: &Mapping, new_mapping: &Mapping) -> (Vec<Change>, usize) {
    let mut inputs = BTreeMap::new();
    for input in &old_mapping.inputs {
        inputs.insert(input.port.as_str(), new_mapping.inputs.iter().find(|i| i.node == input.node && i.input == input.input).map(|i| i.port.as_str()));
    }
    let mut outputs = BTreeMap::new();
    for port in &old.outputs {
        outputs.insert(port.name.as_str(), new.outputs.iter().find(|p| p.port_type.modality == port.port_type.modality).map(|p| p.name.as_str()));
    }
    let mut changes = Vec::new();
    let mut disconnected = 0usize;
    for edge in graph.edges().values() {
        if edge.to.node != old.id && edge.from.node != old.id {
            continue;
        }
        let mut edge = edge.clone();
        let mut remove = false;
        if edge.to.node == old.id {
            match inputs.get(edge.to.port.as_str()).copied().flatten() {
                Some(name) => edge.to.port = name.into(),
                None => remove = true,
            }
        }
        if edge.from.node == old.id {
            match outputs.get(edge.from.port.as_str()).copied().flatten() {
                Some(name) => edge.from.port = name.into(),
                None => remove = true,
            }
        }
        if remove {
            changes.push(Change::Disconnect { canvas: graph.id(), id: edge.id });
            disconnected = disconnected.saturating_add(1);
        } else if graph.edges().get(&edge.id) != Some(&edge) {
            changes.push(Change::SetEdge { canvas: graph.id(), edge });
        }
    }
    (changes, disconnected)
}
fn preserve_history(old: &Node, new: &mut Node, allow_clear: bool) -> Result<(), Error> {
    let order = new.outputs.iter().map(|port| old.outputs.iter().position(|p| p.port_type == port.port_type)).collect::<Option<Vec<_>>>();
    let incompatible = !old.history.is_empty()
        && (old.outputs.len() != new.outputs.len() || order.is_none() || old.history.iter().any(|g| g.outputs.len() != old.outputs.len()));
    if incompatible {
        if !allow_clear {
            return Err(Error::new(ErrorKind::Conflict, "changing result media types requires clear_history:true; Undo restores the old history"));
        }
        new.history.clear();
    } else if let Some(order) = order {
        for generation in &mut new.history {
            generation.outputs =
                order.iter().map(|i| generation.outputs.get(*i).cloned().ok_or_else(|| invalid("history output unavailable"))).collect::<Result<_, _>>()?;
        }
    }
    new.active = None;
    Ok(())
}
fn unchanged_selection(p: &Apply, old: &Mapping) -> bool {
    let media = p.inputs.iter().filter(|i| old.inputs.iter().any(|o| o.node == i.node && o.input == i.input)).collect::<Vec<_>>();
    let parameters = p.inputs.iter().filter(|i| old.params.iter().any(|o| o.node == i.node && o.input == i.input)).collect::<Vec<_>>();
    media.len() == old.inputs.len()
        && parameters.len() == old.params.len()
        && media.len().saturating_add(parameters.len()) == p.inputs.len()
        && media.iter().zip(&old.inputs).all(|(a, b)| a.node == b.node && a.input == b.input && a.name == b.port)
        && parameters.iter().zip(&old.params).all(|(a, b)| a.node == b.node && a.input == b.input && a.name == b.spec.label)
        && p.outputs.len() == old.outputs.len()
        && p.outputs.iter().zip(&old.outputs).all(|(a, b)| a.node == b.node && a.label == b.label)
        && p.output_names.as_ref().is_none_or(|names| names == &old.output_names)
}
fn idle(c: &Context<'_>, canvas: CanvasId, node: NodeId) -> Result<(), Error> {
    if c.resources.get::<crate::run_host::MappingEditGuard>().is_some_and(|guard| guard.busy(canvas, node)) {
        return Err(Error::new(ErrorKind::Busy, "finish or cancel this card's run before changing its fields"));
    }
    Ok(())
}
fn apply(c: &Context<'_>, json: &Value) -> Result<Effect, Error> {
    c.editable()?;
    let p: Apply = params(json)?;
    let old_node = source(c, p.canvas, p.id)?;
    idle(c, p.canvas, p.id)?;
    if p.binding.len() != 64 || p.binding != binding(c, p.canvas, old_node)? {
        return Err(Error::new(ErrorKind::Conflict, "workflow mapping changed; reload its fields before applying"));
    }
    let old = card::mapping(old_node)?;
    if unchanged_selection(&p, &old) {
        return Ok(Effect::None { result: json!({"changed":false,"disconnected":0}) });
    }
    let (workflow, exposed) = discover(c, old_node)?;
    let exposed = selected(exposed, &p)?;
    let proposal = card::propose(&workflow, &exposed)?;
    let mut config = old_node.config.clone();
    config.extend(proposal.config);
    let mut proposed = old_node.clone();
    proposed.config = config;
    let mut mapped = card::mapping(&proposed)?;
    for input in &mut mapped.inputs {
        input.port = p
            .inputs
            .iter()
            .find(|i| i.node == input.node && i.input == input.input)
            .map(|i| i.name.clone())
            .ok_or_else(|| invalid("input selection missing"))?;
    }
    preserve_parameter_names(&mut mapped, &old)?;
    // Hiding/relabeling fields must never erase previously recorded evidence of credit use.
    mapped.partners = old.partners.iter().chain(&mapped.partners).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    mapped.output_names = p.output_names.unwrap_or_else(|| old.output_names.clone());
    mapped.check()?;
    let encoded = card::Card { workflow: card::card(&proposed)?.workflow, mapping: mapped.clone() }.config()?;
    proposed.config.extend(encoded);
    let rebuilt = c.kinds.get(card::ID)?.instantiate_instance(old_node.id, proposed.config.clone(), &Default::default())?;
    proposed.inputs = rebuilt.inputs;
    proposed.outputs = rebuilt.outputs;
    proposed.params = rebuilt.params;
    preserve_history(old_node, &mut proposed, p.clear_history)?;
    let graph = c.document.canvases().get(&p.canvas).ok_or_else(|| invalid("canvas unavailable"))?;
    let (mut changes, disconnected) = rename_edges(graph, old_node, &proposed, &old, &mapped);
    if disconnected > 0 && !p.disconnect {
        return Err(Error::new(ErrorKind::Conflict, "hidden fields have connections; explicitly allow disconnecting them before applying"));
    }
    if proposed == *old_node && changes.is_empty() {
        return Ok(Effect::None { result: json!({"changed":false,"disconnected":0}) });
    }
    changes.insert(0, Change::SetNode { canvas: p.canvas, node: proposed });
    let changes = Changeset::new(changes);
    c.preview_changes(&changes)?;
    Ok(Effect::Edit { changes, result: json!({"changed":true,"disconnected":disconnected,"warnings":proposal.warnings}), gesture: None })
}
fn enabled_list(c: &Context<'_>, json: &Value) -> Result<(), Error> {
    let p: Target = params(json)?;
    let node = source(c, p.canvas, p.id)?;
    card::mapping(node)?;
    catalog(c, node).map(|_| ())
}
fn enabled_apply(c: &Context<'_>, json: &Value) -> Result<(), Error> {
    c.editable()?;
    let p: Apply = params(json)?;
    let node = source(c, p.canvas, p.id)?;
    idle(c, p.canvas, p.id)?;
    if p.binding != binding(c, p.canvas, node)? {
        return Err(Error::new(ErrorKind::Conflict, "workflow mapping changed; reload its fields"));
    }
    catalog(c, node).map(|_| ())
}
fn list_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"required":["canvas","id"],"properties":{"canvas":{"type":"string","format":"uuid"},"id":{"type":"string","format":"uuid"}}})
}
fn apply_schema() -> Value {
    let mut schema = list_schema();
    schema["required"] = json!(["canvas", "id", "binding", "inputs", "outputs"]);
    schema["properties"]["binding"] = json!({"type":"string","minLength":64,"maxLength":64});
    schema["properties"]["inputs"] = json!({"type":"array","maxItems":264,"items":{"type":"object","additionalProperties":false,"required":["node","input","name"],"properties":{"node":{"type":"string","maxLength":256},"input":{"type":"string","maxLength":256},"name":{"type":"string","minLength":1,"maxLength":256}}}});
    schema["properties"]["outputs"] = json!({"type":"array","minItems":1,"maxItems":64,"items":{"type":"object","additionalProperties":false,"required":["node","label"],"properties":{"node":{"type":"string","maxLength":256},"label":{"type":"string","minLength":1,"maxLength":256}}}});
    schema["properties"]["output_names"] = json!({"type":"array","maxItems":4,"items":{"type":"object","additionalProperties":false,"required":["modality","name"],"properties":{"modality":{"enum":["image","video","audio","mesh"]},"name":{"type":"string","minLength":1,"maxLength":128}}}});
    for flag in ["disconnect", "clear_history"] {
        schema["properties"][flag] = json!({"type":"boolean"});
    }
    schema
}
pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "comfy.workflow.mapping",
            label: "List workflow fields",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{canvas,id} -> {binding,inputs,outputs,input_order,output_order,output_names,ports,history}; available fields from this card's server, and current saved selection. Read-only; no network request.",
            enabled: enabled_list,
            run: list,
        },
        CommandSpec {
            id: "comfy.workflow.mapping_set",
            label: "Change workflow fields",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: apply_schema,
            params_doc: "{canvas,id,binding,inputs:[{node,input,name}],outputs:[{node,label}],output_names?:[{modality,name}],disconnect?:bool,clear_history?:bool}; select/name/order fields without reimport. Current values survive hiding; renamed ports retain connections. Hidden connected ports require explicit disconnect. Changing result media types with history requires explicit clear_history. One Undo step.",
            enabled: enabled_apply,
            run: apply,
        },
    ]
}
#[cfg(test)]
mod tests;
