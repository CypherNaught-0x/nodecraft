//! Catalog-neutral production image generation. Engine compiles the validated catalog into
//! these immutable definitions; this L2 module never depends on catalog or a provider.
use crate::{
    Category, GenerateInput, GenerateOutput, InputContract, JobSpec, ModeContract, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx, PortCount,
    descriptor::invalid,
};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{
    ConnectedText, Contract, Error, ErrorKind, Modality, NodeId, ParamControl, ParamOwner, ParamSchema, ParamSpec, ParamValues, PortType, ResolvedParams,
    TextPrecedence, Value, compose_text,
};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub const KIND: &str = "generation.image";
pub const MODEL: &str = "model";
pub const PROMPT: &str = "prompt";
const MAX_PROMPTS: usize = 1024;

/// Original provider role cardinality; wire-to-prompt composition is a separate operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationRole {
    pub role: String,
    pub modality: Modality,
    pub min: u16,
    pub max: u16,
    pub inline: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationMode {
    pub id: ModeId,
    pub inputs: Vec<GenerationRole>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenerationModel {
    pub id: String,
    pub provider: String,
    pub remote: String,
    pub parameters: Contract,
    pub modes: Vec<GenerationMode>,
}
struct Definition {
    model: GenerationModel,
    descriptor: NodeDescriptor,
    contract: NodeContract,
}
#[derive(Clone)]
pub struct GenerationKind {
    base: NodeDescriptor,
    snapshot: [u8; 32],
    definitions: Arc<BTreeMap<String, Definition>>,
}
fn local_schema(model: Option<&str>) -> ParamSchema {
    ParamSchema {
        params: vec![
            ParamSpec {
                name: MODEL.into(),
                label: "Model".into(),
                description: None,
                control: ParamControl::Model,
                required: true,
                default: model.map(|s| Value::Text(s.into())),
                visible_if: None,
            },
            ParamSpec {
                name: PROMPT.into(),
                label: "Prompt".into(),
                description: None,
                control: ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 },
                required: false,
                default: Some(Value::Text(String::new())),
                visible_if: None,
            },
        ],
    }
}
fn base() -> NodeDescriptor {
    NodeDescriptor {
        id: KIND.into(),
        label: "Generate image".into(),
        category: Category::Generation,
        inputs: vec![Port { name: PROMPT.into(), port_type: PortType { modality: Modality::Text, many: true } }],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: true } }],
        params: local_schema(None),
        quick_add: None,
    }
}
fn unavailable() -> Error {
    Error::new(ErrorKind::Unsupported, "selected image model is unavailable; choose a supported catalog model")
}
impl GenerationKind {
    /// Takes ownership of already-admitted neutral definitions. The engine bounds aggregate
    /// retained definition bytes before constructing these owners.
    pub fn new(models: Vec<GenerationModel>, snapshot: [u8; 32]) -> Result<Self, Error> {
        if models.len() > 4096 {
            return Err(invalid("too many generation models"));
        }
        let mut definitions = BTreeMap::new();
        for model in models {
            model.parameters.validate()?;
            if model.parameters.rules.iter().any(|r| r.default == Some(nodecraft_types::DefaultPolicy::RandomSeed)) {
                return Err(Error::new(ErrorKind::Unsupported, "random-seed generation needs a host that records the realized seed"));
            }
            if !nodecraft_types::model_namespace(&model.id)
                || !nodecraft_types::model_namespace(&model.provider)
                || model.remote.is_empty()
                || model.remote.len() > 4096
                || model.remote.chars().any(char::is_control)
                || model.parameters.namespace != model.id
                || model.parameters.owner != ParamOwner::Model
                || model.modes.is_empty()
                || model.modes.len() > 32
            {
                return Err(invalid("invalid image model definition"));
            }
            if model.parameters.schema.params.iter().any(|p| p.name == MODEL || p.name == PROMPT || p.control == ParamControl::Model) {
                return Err(invalid("model settings collide with node-owned model or prompt controls"));
            }
            let local = local_schema(Some(&model.id));
            ParamSchema::validate_parts(&[&local, &model.parameters.schema])?;
            let mut descriptor = base();
            descriptor.params = local;
            descriptor.params.params.extend(model.parameters.schema.params.iter().cloned());
            let mut roles: BTreeMap<String, Modality> = [(PROMPT.into(), Modality::Text)].into();
            for mode in &model.modes {
                if mode.inputs.len() > 16 {
                    return Err(invalid("too many image input roles"));
                }
                let mut seen = BTreeSet::new();
                for input in &mode.inputs {
                    if !crate::descriptor::identifier(&input.role) || input.max == 0 || input.min > input.max || !seen.insert(input.role.as_str()) {
                        return Err(invalid("unsupported or ambiguous image input role"));
                    }
                    if let Some(prior) = roles.insert(input.role.clone(), input.modality)
                        && prior != input.modality
                    {
                        return Err(invalid("one input role has different modalities"));
                    }
                }
            }
            for (role, modality) in &roles {
                if role != PROMPT {
                    descriptor.inputs.push(Port { name: role.clone(), port_type: PortType { modality: *modality, many: true } });
                }
            }
            let mut contract = NodeContract::derive(&descriptor);
            contract.params = Contract::from_schema(KIND, ParamOwner::Node, local_schema(Some(&model.id)));
            contract.owned_groups.push(model.parameters.clone());
            contract.inputs = descriptor
                .inputs
                .iter()
                .map(|p| InputContract { port: p.name.clone(), role: p.name.clone(), min: 0, max: Some(if p.name == PROMPT { 1024 } else { 255 }) })
                .collect();
            contract.modes = model
                .modes
                .iter()
                .map(|mode| ModeContract {
                    mode: mode.id.clone(),
                    inputs: descriptor
                        .inputs
                        .iter()
                        .map(|port| {
                            let role = mode.inputs.iter().find(|role| role.role == port.name);
                            let (min, max) = role.map_or((0, 0), |r| if r.role == PROMPT { (if r.inline { 0 } else { r.min }, 1024) } else { (r.min, r.max) });
                            PortCount { port: port.name.clone(), min, max: Some(max) }
                        })
                        .collect(),
                })
                .collect();
            descriptor.validate()?;
            contract.validate(&descriptor)?;
            let id = model.id.clone();
            if definitions.insert(id, Definition { model, descriptor, contract }).is_some() {
                return Err(invalid("duplicate image model"));
            }
        }
        Ok(Self { base: base(), snapshot, definitions: Arc::new(definitions) })
    }
    pub fn model(&self, id: &str) -> Option<&GenerationModel> {
        self.definitions.get(id).map(|d| &d.model)
    }
    /// Borrow immutable destination layout/contracts without constructing a candidate node.
    pub fn model_descriptor(&self, id: &str) -> Option<&NodeDescriptor> {
        self.definitions.get(id).map(|d| &d.descriptor)
    }
    pub fn model_contract(&self, id: &str) -> Option<&NodeContract> {
        self.definitions.get(id).map(|d| &d.contract)
    }
    fn definition(&self, node: &Node) -> Result<&Definition, Error> {
        let Some(Value::Text(id)) = node.params.get(MODEL) else {
            return Err(unavailable());
        };
        self.definitions.get(id).ok_or_else(unavailable)
    }
    fn version(node: &Node) -> Result<(), Error> {
        if node.config.get("generation_contract_major").is_some_and(|v| v != &Value::Unsigned(nodecraft_types::Unsigned(1))) {
            return Err(Error::new(ErrorKind::Unsupported, "generation parameter version is unsupported"));
        }
        Ok(())
    }
    /// Detached construction for the shared model-swap preparation. No migration, no document
    /// mutation, no inference about incident edges; consumers validate actual wiring separately.
    pub fn prepare_instance(&self, existing: &Node, destination: &str, proposed: &ParamValues) -> Result<Node, Error> {
        existing.validate_payload()?;
        let definition = self.definitions.get(destination).ok_or_else(unavailable)?;
        definition.model.parameters.resolve(proposed, false)?.submission()?;
        let mut values = proposed.clone();
        values.insert(MODEL.into(), Value::Text(destination.into()));
        if let Some(prompt) = existing.params.get(PROMPT) {
            values.insert(PROMPT.into(), prompt.clone());
        }
        let detached = self.instantiate_instance(existing.id, existing.config.clone(), &values)?;
        let mut candidate = existing.clone();
        candidate.params = detached.params;
        candidate.config = detached.config;
        candidate.inputs = detached.inputs;
        candidate.outputs = detached.outputs;
        self.validate_instance(&candidate, false)?;
        Ok(candidate)
    }
}
impl NodeKind for GenerationKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.base
    }
    fn definition_snapshot(&self) -> Option<[u8; 32]> {
        Some(self.snapshot)
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        Self::version(node)?;
        Ok(Cow::Borrowed(self.definition(node).map_or(&self.base, |d| &d.descriptor)))
    }
    fn instance_contract(&self, node: &Node) -> Result<NodeContract, Error> {
        node.validate_payload()?;
        Self::version(node)?;
        match self.definition(node) {
            Ok(d) => Ok(d.contract.clone()),
            Err(_) => Ok(NodeContract::derive(&self.base)),
        }
    }
    fn validate_instance(&self, node: &Node, ready: bool) -> Result<ResolvedParams, Error> {
        node.validate_payload()?;
        Self::version(node)?;
        let d = self.definition(node)?;
        if node.kind != KIND || node.inputs != d.descriptor.inputs || node.outputs != d.descriptor.outputs {
            return Err(invalid("generation ports do not match the selected model"));
        }
        d.contract.resolve(&node.params, ready)?.into_resolved(ready)
    }
    fn instantiate_instance(&self, id: NodeId, mut config: BTreeMap<String, Value>, supplied: &ParamValues) -> Result<Node, Error> {
        let mut node = Node::new(id, KIND);
        // Resolve before copying supplied values; known controls reject lists recursively by type.
        let definition = match supplied.get(MODEL) {
            Some(Value::Text(id)) => self.definitions.get(id),
            _ => None,
        };
        let (descriptor, contract) = match definition {
            Some(d) => (&d.descriptor, d.contract.clone()),
            None => (&self.base, NodeContract::derive(&self.base)),
        };
        let r = contract.resolve(supplied, false)?;
        if let Some(issue) = r.issues.first() {
            return Err(Error::new(ErrorKind::InvalidInput, issue.message.clone()));
        }
        config.entry("generation_contract_major".into()).or_insert(Value::Unsigned(nodecraft_types::Unsigned(1)));
        node.config = config;
        Self::version(&node)?;
        node.params = r
            .params
            .into_iter()
            .filter_map(|p| match p.value {
                nodecraft_types::Effective::Explicit(value) if !p.reset => Some((p.name, value)),
                nodecraft_types::Effective::Default(value) if p.enabled => Some((p.name, value)),
                _ => None,
            })
            .collect();
        node.inputs = descriptor.inputs.clone();
        node.outputs = descriptor.outputs.clone();
        node.validate_payload()?;
        Ok(node)
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.iter().all(|p| p.modality == Modality::Text).then(|| ModeId::new("text-to-image").ok()).flatten()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.validate_instance(ctx.node, true)?;
        let definition = self.definition(ctx.node)?;
        if ctx.inputs.len() > MAX_PROMPTS {
            return Err(invalid("too many image prompt inputs"));
        }
        let mut wires = BTreeMap::new();
        for input in ctx.inputs {
            if input.port != PROMPT || input.port_type.modality != Modality::Text {
                return Err(Error::new(ErrorKind::Unsupported, "image generation media inputs require a host media resolver"));
            }
            if !definition.contract.inputs.iter().any(|port| port.port == input.port) {
                return Err(invalid("image input names an unavailable port"));
            }
            let count = wires.entry(input.port.as_str()).or_insert(0usize);
            *count = count.checked_add(1).ok_or_else(|| invalid("too many image input wires"))?;
        }
        // Count actual connected inputs, not composed text fragments or the local prompt.
        // In particular local text is never a substitute for a required non-inline wire.
        let mode_id = definition.contract.resolve_mode(&wires, true)?.ok_or_else(|| invalid("image model has no declared input mode"))?;
        let mode = definition.model.modes.iter().find(|mode| mode.id == *mode_id).ok_or_else(|| invalid("image model mode is unavailable"))?;
        if !mode.inputs.iter().all(|r| (r.role == PROMPT && r.modality == Modality::Text && r.min <= 1 && r.max >= 1) || r.min == 0) {
            return Err(Error::new(ErrorKind::Unsupported, "this image model requires unsupported input roles"));
        }
        let prompt_role = mode
            .inputs
            .iter()
            .find(|r| r.role == PROMPT && r.modality == Modality::Text)
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "model has no supported prompt role"))?;
        let mut texts = Vec::new();
        for input in ctx.inputs {
            if texts.len() >= MAX_PROMPTS {
                return Err(invalid("too many prompt values"));
            }
            if input.port != PROMPT || input.port_type.modality != Modality::Text {
                return Err(Error::new(ErrorKind::Unsupported, "image generation media inputs require a host media resolver"));
            }
            match &input.value {
                Value::Text(text) => {
                    texts.push(ConnectedText { order: u32::try_from(texts.len()).map_err(|_| invalid("too many prompt values"))?, tie: [0; 16], text })
                }
                Value::List(items) => {
                    for value in items {
                        if texts.len() >= MAX_PROMPTS {
                            return Err(invalid("too many prompt values"));
                        }
                        let Value::Text(text) = value else {
                            return Err(invalid("image prompts must contain text"));
                        };
                        texts.push(ConnectedText { order: u32::try_from(texts.len()).map_err(|_| invalid("too many prompt values"))?, tie: [0; 16], text });
                    }
                }
                _ => return Err(invalid("image prompts must contain text")),
            }
        }
        let local = match ctx.node.params.get(PROMPT) {
            Some(Value::Text(text)) => text.as_str(),
            None => "",
            _ => return Err(invalid("image prompt must be text")),
        };
        let prompt = compose_text(TextPrecedence::ConnectedReplaces, prompt_role.inline.then_some(local), &texts, "\n")?;
        if prompt.trim().is_empty() {
            return Err(invalid("image generation needs a prompt"));
        }
        let target = ctx
            .targets
            .get(ctx.node.id, &definition.model.provider)
            .ok_or_else(|| Error::new(ErrorKind::Unsupported, "image provider route is not configured"))?;
        let params = definition.model.parameters.resolve_selected(&ctx.node.params, true)?.submission()?;
        Ok(vec![JobSpec::Generate {
            node: ctx.node.id,
            outputs: vec![GenerateOutput { port: "out".into(), modality: Modality::Image }],
            provider: definition.model.provider.clone(),
            instance: target.instance(),
            model: definition.model.remote.clone(),
            params,
            inputs: vec![GenerateInput { role: PROMPT.into(), value: Value::Text(prompt) }],
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_mode_ids_are_lossless_and_bounded() {
        let mode = ModeId::new("text-to-image").unwrap();
        assert_eq!(mode.as_str(), "text-to-image");
        let encoded = serde_json::to_string(&mode).unwrap();
        assert_eq!(serde_json::from_str::<ModeId>(&encoded).unwrap(), mode);
        for bad in ["", ".", "..", "/mode", "mode/", "mode//next", "mode/../next", "Mode", "mode space"] {
            assert!(ModeId::new(bad).is_err());
        }
        assert!(ModeId::new("x".repeat(129)).is_err());
        assert!(ModeId::new("x".repeat(128)).is_ok());
    }
}
