//! The ComfyUI workflow card (#9): an imported ComfyUI workflow that runs as one node. The
//! workflow (API format) and its mapping (which of its inputs are the card's ports and
//! parameters, and which of its output nodes give results) are stored on the node's config, so
//! its ports never change unless the workflow is imported again.
//!
//! A run sends the stored workflow to the `comfyui` provider with the card's parameter values
//! and the media wired into its ports; the provider checks it against the server, uploads the
//! media and queues it. It is free (it runs on your own server), so its catalog model
//! [`MODEL_ID`] takes no parameters and is never offered on other cards.
use crate::technique::slug;
use crate::{Category, GenerateInput, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_comfy::{ApiWorkflow, Exposed, ExposedInput, ExposedKind, InputRole, Media};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Choice, Error, ErrorKind, Integer, Modality, Number, ParamControl, ParamSchema, ParamSpec, ParamValues, PortType, Unsigned, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// The workflow card's kind id.
pub const ID: &str = "generation.comfy_workflow";
/// The provider adapter that runs it.
pub const PROVIDER: &str = "comfyui";
/// Its catalog model: free, no parameters, offered on no other card.
pub const MODEL_ID: &str = "comfyui.workflow";
/// That model's `provider_model`, which the adapter receives.
pub const REMOTE_MODEL: &str = "workflow";
/// The card's model parameter; the run picker and admission read it like a generation card's.
pub const MODEL: &str = "model";
/// Config: the version of the entries below (`1`).
pub const VERSION: &str = "comfy.workflow.version";
/// Config: the [`Mapping`], as JSON text.
pub const MAPPING: &str = "comfy.workflow.v1.mapping";
/// Config: the workflow in ComfyUI's API format, as JSON text.
pub const WORKFLOW: &str = "comfy.workflow.v1.api";
/// The job input carrying the workflow's JSON text.
pub const WORKFLOW_ROLE: &str = "comfy.workflow";
/// The job input carrying the card's values, as JSON text of `{"<node>.<input>": value}`.
pub const VALUES_ROLE: &str = "comfy.values";
/// The job input naming the workflow's output nodes the card keeps, as JSON text of `["<node>", …]`.
pub const OUTPUTS_ROLE: &str = "comfy.outputs";
/// Most bytes of a stored workflow. It travels as one text input (at most 1 MiB).
pub const MAX_WORKFLOW_BYTES: usize = 960 * 1024;
/// Most bytes of a stored mapping.
const MAX_MAPPING_BYTES: usize = 512 * 1024;
/// Most bytes of the values a run sends.
const MAX_VALUES_BYTES: usize = 1024 * 1024;
/// Most media inputs, parameters and output nodes a card maps.
const MAX_INPUTS: usize = 64;
const MAX_PARAMS: usize = 200;
const MAX_OUTPUTS: usize = 64;
/// Longest workflow node id or input name a mapping names.
const MAX_TARGET_BYTES: usize = 256;
/// Longest text a text parameter holds.
const MAX_PARAM_TEXT: usize = 256 * 1024;
const VERSION_1: u64 = 1;

/// Which of a workflow's inputs and outputs the card exposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mapping {
    /// File inputs (a `LoadImage`-style loader), each an input port.
    pub inputs: Vec<MappedInput>,
    /// Literal inputs, each a card parameter.
    pub params: Vec<MappedParam>,
    /// Output nodes whose saved files are the card's results.
    pub outputs: Vec<MappedOutput>,
}

/// One input port: what is wired into it is uploaded and set on `node`'s `input`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedInput {
    pub node: String,
    pub input: String,
    pub port: String,
    pub modality: Modality,
}

/// One parameter: its value is set on `node`'s `input`. Without a value the workflow keeps its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedParam {
    pub node: String,
    pub input: String,
    pub spec: ParamSpec,
}

/// One output node and the media it saves; results land on the port for that media.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappedOutput {
    pub node: String,
    pub label: String,
    pub modality: Modality,
}

/// What a workflow card stores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    /// The workflow's API JSON text.
    pub workflow: String,
    pub mapping: Mapping,
}

fn port_modality(media: Media) -> Modality {
    match media {
        Media::Image => Modality::Image,
        Media::Video => Modality::Video,
        Media::Audio => Modality::Audio,
        Media::Mesh => Modality::Mesh,
    }
}

fn is_media(modality: Modality) -> bool {
    matches!(modality, Modality::Image | Modality::Video | Modality::Audio | Modality::Mesh)
}

/// The output port results of `modality` land on.
fn output_port(modality: Modality) -> &'static str {
    match modality {
        Modality::Video => "video",
        Modality::Audio => "audio",
        Modality::Mesh => "mesh",
        _ => "image",
    }
}

fn target(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_TARGET_BYTES && !text.chars().any(char::is_control) && !text.contains('.')
}

/// `text` cut to at most `max` bytes, at a character boundary.
fn bounded(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default().to_owned()
}

impl Mapping {
    /// Checks the mapping's own shape: bounded counts and names, unique ports, parameters and
    /// targets, and media modalities. Workflow node ids are checked against the workflow by
    /// [`Mapping::check_targets`].
    pub fn check(&self) -> Result<(), Error> {
        if self.inputs.len() > MAX_INPUTS || self.params.len() > MAX_PARAMS || self.outputs.len() > MAX_OUTPUTS {
            return Err(invalid("a ComfyUI workflow card maps too many inputs, parameters or outputs"));
        }
        if self.outputs.is_empty() {
            return Err(invalid("a ComfyUI workflow card needs an output node"));
        }
        let mut targets = BTreeSet::new();
        let mut ports = BTreeSet::new();
        for input in &self.inputs {
            if !target(&input.node) || !target(&input.input) || !is_media(input.modality) {
                return Err(invalid("a ComfyUI workflow card's file input is malformed"));
            }
            if !targets.insert((input.node.as_str(), input.input.as_str())) || !ports.insert(input.port.as_str()) {
                return Err(invalid("a ComfyUI workflow card maps an input twice"));
            }
        }
        let mut names = BTreeSet::from([MODEL]);
        for param in &self.params {
            if !target(&param.node) || !target(&param.input) {
                return Err(invalid("a ComfyUI workflow card's parameter is malformed"));
            }
            if !targets.insert((param.node.as_str(), param.input.as_str())) || !names.insert(param.spec.name.as_str()) {
                return Err(invalid("a ComfyUI workflow card maps an input twice"));
            }
        }
        let mut outputs = BTreeSet::new();
        for output in &self.outputs {
            if !target(&output.node) || output.label.trim().is_empty() || output.label.len() > 256 || !is_media(output.modality) {
                return Err(invalid("a ComfyUI workflow card's output is malformed"));
            }
            if !outputs.insert(output.node.as_str()) {
                return Err(invalid("a ComfyUI workflow card maps an output twice"));
            }
        }
        Ok(())
    }

    /// Checks every node the mapping names is in `workflow`.
    pub fn check_targets(&self, workflow: &ApiWorkflow) -> Result<(), Error> {
        let nodes = self.inputs.iter().map(|i| &i.node).chain(self.params.iter().map(|p| &p.node)).chain(self.outputs.iter().map(|o| &o.node));
        for node in nodes {
            if !workflow.nodes.contains_key(node) {
                return Err(invalid("a ComfyUI workflow card maps a node its workflow doesn't have"));
            }
        }
        Ok(())
    }

    /// The card's output ports: one list port per kind of media its output nodes save, in the
    /// order they first appear. Results are routed by media kind, so one port per kind.
    fn output_ports(&self) -> Vec<Port> {
        let mut seen = Vec::new();
        for output in &self.outputs {
            if !seen.contains(&output.modality) {
                seen.push(output.modality);
            }
        }
        seen.into_iter().map(|modality| Port { name: output_port(modality).into(), port_type: PortType { modality, many: true } }).collect()
    }
}

impl Card {
    /// The config that makes a workflow card run this workflow.
    pub fn config(&self) -> Result<BTreeMap<String, Value>, Error> {
        self.mapping.check()?;
        if self.workflow.len() > MAX_WORKFLOW_BYTES {
            return Err(Error::new(ErrorKind::Overflow, "this ComfyUI workflow is too large to place as a card"));
        }
        let mapping = serde_json::to_string(&self.mapping).map_err(|_| Error::new(ErrorKind::Internal, "a ComfyUI workflow mapping could not be stored"))?;
        if mapping.len() > MAX_MAPPING_BYTES {
            return Err(Error::new(ErrorKind::Overflow, "this ComfyUI workflow exposes too much to place as a card"));
        }
        Ok([
            (VERSION.to_owned(), Value::Unsigned(Unsigned(VERSION_1))),
            (MAPPING.to_owned(), Value::Text(mapping)),
            (WORKFLOW.to_owned(), Value::Text(self.workflow.clone())),
        ]
        .into_iter()
        .collect())
    }
}

/// The mapping a workflow card stores. The workflow text is not parsed here.
pub fn mapping(node: &Node) -> Result<Mapping, Error> {
    match node.config.get(VERSION) {
        Some(Value::Unsigned(Unsigned(VERSION_1))) => {}
        Some(_) => return Err(Error::new(ErrorKind::Unsupported, "this ComfyUI workflow card was made by a newer NodeCraft")),
        None => return Err(invalid("a ComfyUI workflow card needs an imported workflow")),
    }
    let mapping: Mapping = match node.config.get(MAPPING) {
        Some(Value::Text(text)) if text.len() <= MAX_MAPPING_BYTES => {
            serde_json::from_str(text).map_err(|_| invalid("a ComfyUI workflow card's mapping is malformed"))?
        }
        Some(Value::Text(_)) => return Err(invalid("a ComfyUI workflow card's mapping is too large")),
        _ => return Err(invalid("a ComfyUI workflow card needs an imported workflow")),
    };
    mapping.check()?;
    Ok(mapping)
}

/// What a workflow card stores: its mapping and its workflow's text.
pub fn card(node: &Node) -> Result<Card, Error> {
    let mapping = mapping(node)?;
    let workflow = match node.config.get(WORKFLOW) {
        Some(Value::Text(text)) if text.len() <= MAX_WORKFLOW_BYTES => text.clone(),
        Some(Value::Text(_)) => return Err(invalid("a ComfyUI workflow card's workflow is too large")),
        _ => return Err(invalid("a ComfyUI workflow card needs an imported workflow")),
    };
    Ok(Card { workflow, mapping })
}

/// The control a literal input gets as a card parameter.
fn control(input: &ExposedInput) -> Option<ParamControl> {
    let one = Unsigned(1);
    Some(match &input.kind {
        ExposedKind::Media(_) => return None,
        ExposedKind::Text { multiline } => ParamControl::Text { multiline: *multiline, max_bytes: MAX_PARAM_TEXT },
        ExposedKind::Bool => ParamControl::Toggle,
        ExposedKind::Int { max, .. } if input.role == InputRole::Seed => ParamControl::Seed { max: max.and_then(|m| u64::try_from(m).ok()).map(Unsigned) },
        ExposedKind::Int { min: Some(min), max, .. } if *min >= 0 => {
            ParamControl::Unsigned { min: u64::try_from(*min).ok().map(Unsigned), max: max.and_then(|m| u64::try_from(m).ok()).map(Unsigned), step: one }
        }
        ExposedKind::Int { min, max, .. } => ParamControl::Integer {
            min: min.and_then(|m| i64::try_from(m).ok()).map(Integer),
            max: max.and_then(|m| i64::try_from(m).ok()).map(Integer),
            step: one,
        },
        ExposedKind::Float { min, max, step } => ParamControl::Slider {
            min: min.and_then(|m| Number::new(m).ok()),
            max: max.and_then(|m| Number::new(m).ok()),
            step: step.filter(|s| s.is_finite() && *s > 0.0).and_then(|s| Number::new(s).ok()).or_else(|| Number::new(0.01).ok())?,
        },
        ExposedKind::Choice { options } => {
            let mut seen = BTreeSet::new();
            let options: Vec<Choice> = options
                .iter()
                .filter(|o| !o.trim().is_empty() && o.len() <= 4096 && seen.insert(o.as_str()))
                .take(16_384)
                .map(|o| Choice { value: o.clone(), label: o.clone() })
                .collect();
            if options.is_empty() { ParamControl::Text { multiline: false, max_bytes: 4096 } } else { ParamControl::Choice { options } }
        }
    })
}

/// `value` (a workflow's JSON) as a value of `control`, if it is one.
fn from_json(value: &serde_json::Value, control: &ParamControl) -> Option<Value> {
    let value = match control {
        ParamControl::Text { .. } | ParamControl::Choice { .. } => Value::Text(value.as_str()?.to_owned()),
        ParamControl::Toggle => Value::Bool(value.as_bool()?),
        ParamControl::Seed { .. } | ParamControl::Unsigned { .. } => Value::Unsigned(Unsigned(value.as_u64()?)),
        ParamControl::Integer { .. } => Value::Integer(Integer(value.as_i64()?)),
        ParamControl::Slider { .. } => Value::Number(Number::new(value.as_f64()?).ok()?),
        _ => return None,
    };
    control.validate_value(&value).ok().map(|()| value)
}

/// A card value as the JSON the workflow takes.
fn to_json(value: &Value) -> Result<serde_json::Value, Error> {
    Ok(match value {
        Value::Text(text) => serde_json::Value::String(text.clone()),
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(Integer(n)) => serde_json::Value::from(*n),
        Value::Unsigned(Unsigned(n)) => serde_json::Value::from(*n),
        Value::Number(n) => {
            serde_json::Number::from_f64(n.get()).map(serde_json::Value::Number).ok_or_else(|| invalid("a ComfyUI value isn't a finite number"))?
        }
        _ => return Err(invalid("a ComfyUI workflow card's parameter isn't a plain value")),
    })
}

/// The card config for `workflow` exposing `exposed`: its file inputs as ports, its other
/// inputs as parameters (defaulting to the workflow's values), and its output nodes that save
/// files as results. `exposed` is the selection to make (start from
/// [`nodecraft_comfy::discover`], keeping inputs whose role is
/// [`InputRole::exposed_by_default`]); preview outputs and outputs of unknown media are left out.
pub fn propose(workflow: &ApiWorkflow, exposed: &Exposed) -> Result<BTreeMap<String, Value>, Error> {
    if exposed.inputs.len() > MAX_INPUTS.saturating_add(MAX_PARAMS) || exposed.outputs.len() > MAX_OUTPUTS {
        return Err(Error::new(ErrorKind::Overflow, "this ComfyUI workflow exposes too much to place as a card"));
    }
    let mut ports = BTreeSet::new();
    let mut names = BTreeSet::from([MODEL.to_owned()]);
    let mut mapping = Mapping { inputs: Vec::new(), params: Vec::new(), outputs: Vec::new() };
    for input in &exposed.inputs {
        if let ExposedKind::Media(media) = input.kind {
            mapping.inputs.push(MappedInput {
                node: input.node.clone(),
                input: input.input.clone(),
                port: slug(&input.label, &mut ports),
                modality: port_modality(media),
            });
            continue;
        }
        let control = control(input).ok_or_else(|| invalid("a ComfyUI input can't be a card parameter"))?;
        let default = input.current.as_ref().and_then(|v| from_json(v, &control)).or_else(|| input.default.as_ref().and_then(|v| from_json(v, &control)));
        let label = bounded(&input.label, 256);
        let spec = ParamSpec {
            name: slug(&input.label, &mut names),
            label: if label.trim().is_empty() { input.input.clone() } else { label },
            description: input.tooltip.as_deref().map(|t| bounded(t, 4096)).filter(|t| !t.trim().is_empty()),
            control,
            required: false,
            default,
            visible_if: None,
        };
        ParamSchema { params: vec![spec.clone()] }.validate().map_err(|_| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("the ComfyUI input {}.{} can't be a card parameter", bounded(&input.node, 64), bounded(&input.input, 64)),
            )
        })?;
        mapping.params.push(MappedParam { node: input.node.clone(), input: input.input.clone(), spec });
    }
    for output in exposed.outputs.iter().filter(|o| !o.preview) {
        let Some(media) = output.media else { continue };
        let label = bounded(&output.label, 256);
        mapping.outputs.push(MappedOutput {
            node: output.node.clone(),
            label: if label.trim().is_empty() { output.node.clone() } else { label },
            modality: port_modality(media),
        });
    }
    if mapping.outputs.is_empty() {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "this ComfyUI workflow saves nothing NodeCraft can collect; add a Save Image (or video, audio, 3D) node",
        ));
    }
    mapping.check()?;
    mapping.check_targets(workflow)?;
    let json = workflow.to_json().map_err(|_| invalid("this ComfyUI workflow is too large to store"))?;
    let workflow = serde_json::to_string(&json).map_err(|_| Error::new(ErrorKind::Internal, "a ComfyUI workflow could not be stored"))?;
    let card = Card { workflow, mapping };
    descriptor(Some(&card.mapping)).validate()?;
    card.config()
}

fn model_spec() -> ParamSpec {
    ParamSpec {
        name: MODEL.into(),
        label: "Model".into(),
        description: Some("Runs on your ComfyUI server.".into()),
        control: ParamControl::Model,
        required: true,
        default: Some(Value::Text(MODEL_ID.into())),
        visible_if: None,
    }
}

fn descriptor(mapping: Option<&Mapping>) -> NodeDescriptor {
    let inputs = mapping
        .map(|m| m.inputs.iter().map(|i| Port { name: i.port.clone(), port_type: PortType { modality: i.modality, many: false } }).collect())
        .unwrap_or_default();
    let params = std::iter::once(model_spec()).chain(mapping.into_iter().flat_map(|m| m.params.iter().map(|p| p.spec.clone()))).collect();
    NodeDescriptor {
        id: ID.into(),
        label: "ComfyUI workflow".into(),
        category: Category::Generation,
        inputs,
        outputs: mapping.map(Mapping::output_ports).unwrap_or_default(),
        params: ParamSchema { params },
        quick_add: None,
    }
}

pub struct ComfyWorkflowKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> ComfyWorkflowKind {
    ComfyWorkflowKind { descriptor: descriptor(None) }
}

impl NodeKind for ComfyWorkflowKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        Ok(Cow::Owned(descriptor(Some(&mapping(node)?))))
    }
    /// Never offered by Turn into: a workflow card is placed by importing a workflow.
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn instance_mode(&self, node: &Node, _: &[PortType]) -> Result<Option<ModeId>, Error> {
        self.resolve_instance(node)?;
        Ok(ModeId::new(REMOTE_MODEL).ok())
    }
    /// One job for the `comfyui` provider: the workflow, the card's values and its output nodes as
    /// text inputs, each wired file under its `<node>.<input>` role, no model parameters. Every
    /// input port must be wired.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let resolved = self.validate_instance(ctx.node, true)?;
        if !matches!(resolved.values.get(MODEL), Some(Value::Text(model)) if model == MODEL_ID) {
            return Err(Error::new(ErrorKind::Unsupported, "a ComfyUI workflow card runs only on its own ComfyUI workflow model"));
        }
        let card = card(ctx.node)?;
        let workflow = ApiWorkflow::parse(card.workflow.as_bytes(), None).map_err(|_| invalid("a ComfyUI workflow card's workflow is malformed"))?;
        card.mapping.check_targets(&workflow)?;
        drop(workflow);
        let mut values = serde_json::Map::new();
        for param in &card.mapping.params {
            if let Some(value) = resolved.values.get(&param.spec.name) {
                values.insert(format!("{}.{}", param.node, param.input), to_json(value)?);
            }
        }
        let values = serde_json::to_string(&values).map_err(|_| Error::new(ErrorKind::Internal, "a ComfyUI workflow card's values could not be sent"))?;
        if values.len() > MAX_VALUES_BYTES {
            return Err(Error::new(ErrorKind::Overflow, "a ComfyUI workflow card's values are too large to send"));
        }
        let mut inputs = vec![
            GenerateInput { role: WORKFLOW_ROLE.into(), value: Value::Text(card.workflow) },
            GenerateInput { role: VALUES_ROLE.into(), value: Value::Text(values) },
        ];
        let mut filled = BTreeSet::new();
        for input in ctx.inputs {
            let mapped =
                card.mapping.inputs.iter().find(|m| m.port == input.port).ok_or_else(|| invalid("a ComfyUI workflow card input names an unavailable port"))?;
            let items = match &input.value {
                Value::List(items) => items.as_slice(),
                value => std::slice::from_ref(value),
            };
            for value in items {
                let Value::Media(asset) = value else {
                    return Err(invalid("a ComfyUI workflow card's inputs must be files"));
                };
                if !mapped.modality.accepts_mime(asset.mime.essence()) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        format!("input {} takes {:?} files, not {}", mapped.port, mapped.modality, bounded(asset.mime.essence(), 128)),
                    ));
                }
                if !filled.insert(mapped.port.as_str()) {
                    return Err(Error::new(ErrorKind::InvalidInput, format!("input {} takes one file", mapped.port)));
                }
                inputs.push(GenerateInput { role: format!("{}.{}", mapped.node, mapped.input), value: value.clone() });
            }
        }
        // An unwired input would run on whatever file the workflow's author last loaded.
        if let Some(unwired) = card.mapping.inputs.iter().find(|m| !filled.contains(m.port.as_str())) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("connect a file to {} before running this workflow", unwired.port)));
        }
        let kept: Vec<&str> = card.mapping.outputs.iter().map(|o| o.node.as_str()).collect();
        let kept = serde_json::to_string(&kept).map_err(|_| Error::new(ErrorKind::Internal, "a ComfyUI workflow card's outputs could not be sent"))?;
        inputs.push(GenerateInput { role: OUTPUTS_ROLE.into(), value: Value::Text(kept) });
        let target =
            ctx.targets.get(ctx.node.id, PROVIDER).ok_or_else(|| Error::new(ErrorKind::Unsupported, "the ComfyUI provider route is not configured"))?;
        let outputs = card
            .mapping
            .output_ports()
            .into_iter()
            .map(|port| GenerateOutput { port: port.name, modality: port.port_type.modality, style_family: None })
            .collect();
        Ok(vec![JobSpec::Generate {
            node: ctx.node.id,
            outputs,
            provider: PROVIDER.into(),
            instance: target.instance(),
            model: REMOTE_MODEL.into(),
            params: ParamValues::new(),
            inputs,
        }])
    }
}

#[cfg(test)]
mod tests;
