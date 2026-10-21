//! Catalog-neutral production generation: image (`generation.image`), text (`generation.text`,
//! #84) and 3D (`generation.mesh`, #22) cards over the same contract. Engine compiles the
//! validated catalog into these immutable definitions; this L2 module never depends on catalog
//! or a provider.
use crate::{
    Category, GenerateInput, GenerateOutput, InputContract, InputValue, JobSpec, ModeContract, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx,
    PortCount, descriptor::invalid, mention,
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
/// A text generation card: an instruction in, a model's answer out (#84).
pub const TEXT_KIND: &str = "generation.text";
/// A 3D generation card: a prompt or images in, one 3D model out (#22).
pub const MESH_KIND: &str = "generation.mesh";
pub const MODEL: &str = "model";
pub const PROMPT: &str = "prompt";
const MAX_PROMPTS: usize = 1024;

/// Whether `kind` is one of the catalog-backed generation kinds.
pub fn is_generation(kind: &str) -> bool {
    kind == KIND || kind == TEXT_KIND || kind == MESH_KIND
}
/// The generation kind whose cards make `output`, if there is one.
pub fn kind_for(output: Modality) -> Option<&'static str> {
    match output {
        Modality::Image => Some(KIND),
        Modality::Text => Some(TEXT_KIND),
        Modality::Mesh => Some(MESH_KIND),
        _ => None,
    }
}

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
    output: Modality,
    id: &'static str,
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
fn base(id: &'static str, output: Modality) -> NodeDescriptor {
    NodeDescriptor {
        id: id.into(),
        label: match output {
            Modality::Text => "Generate text",
            Modality::Mesh => "Generate 3D",
            _ => "Generate image",
        }
        .into(),
        category: Category::Generation,
        inputs: vec![Port { name: PROMPT.into(), port_type: PortType { modality: Modality::Text, many: true } }],
        // One answer or model per run; an image run can make several.
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: output, many: output == Modality::Image } }],
        params: local_schema(None),
        quick_add: None,
    }
}
/// What a kind's messages call its output, so image wording stays exactly as it was.
fn noun(output: Modality) -> &'static str {
    match output {
        Modality::Text => "text",
        Modality::Mesh => "3D",
        _ => "image",
    }
}
fn said(kind: ErrorKind, output: Modality, message: &str) -> Error {
    Error::new(kind, message.replace("{noun}", noun(output)))
}
fn bad(output: Modality, message: &str) -> Error {
    said(ErrorKind::InvalidInput, output, message)
}
impl GenerationKind {
    /// Image generation. Takes ownership of already-admitted neutral definitions. The engine
    /// bounds aggregate retained definition bytes before constructing these owners.
    pub fn new(models: Vec<GenerationModel>, snapshot: [u8; 32]) -> Result<Self, Error> {
        Self::making(Modality::Image, models, snapshot)
    }
    /// Text generation over the same rules: each model's modes here all answer in text.
    pub fn text(models: Vec<GenerationModel>, snapshot: [u8; 32]) -> Result<Self, Error> {
        Self::making(Modality::Text, models, snapshot)
    }
    /// 3D generation: each model's modes here all make one 3D model.
    pub fn mesh(models: Vec<GenerationModel>, snapshot: [u8; 32]) -> Result<Self, Error> {
        Self::making(Modality::Mesh, models, snapshot)
    }
    /// What this kind's cards make.
    pub fn output(&self) -> Modality {
        self.output
    }
    fn unavailable(&self) -> Error {
        said(ErrorKind::Unsupported, self.output, "selected {noun} model is unavailable; choose a supported catalog model")
    }
    fn making(output: Modality, models: Vec<GenerationModel>, snapshot: [u8; 32]) -> Result<Self, Error> {
        let id = kind_for(output).ok_or_else(|| Error::new(ErrorKind::Unsupported, "generation of this modality is not supported"))?;
        if models.len() > 4096 {
            return Err(invalid("too many generation models"));
        }
        let mut definitions = BTreeMap::new();
        for model in models {
            model.parameters.validate()?;
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
                return Err(bad(output, "invalid {noun} model definition"));
            }
            if model.parameters.schema.params.iter().any(|p| p.name == MODEL || p.name == PROMPT || p.control == ParamControl::Model) {
                return Err(invalid("model settings collide with node-owned model or prompt controls"));
            }
            let local = local_schema(Some(&model.id));
            ParamSchema::validate_parts(&[&local, &model.parameters.schema])?;
            let mut descriptor = base(id, output);
            descriptor.params = local;
            descriptor.params.params.extend(model.parameters.schema.params.iter().cloned());
            let mut roles: BTreeMap<String, Modality> = [(PROMPT.into(), Modality::Text)].into();
            for mode in &model.modes {
                if mode.inputs.len() > 16 {
                    return Err(bad(output, "too many {noun} input roles"));
                }
                let mut seen = BTreeSet::new();
                for input in &mode.inputs {
                    if !crate::descriptor::identifier(&input.role) || input.max == 0 || input.min > input.max || !seen.insert(input.role.as_str()) {
                        return Err(bad(output, "unsupported or ambiguous {noun} input role"));
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
            contract.params = Contract::from_schema(id, ParamOwner::Node, local_schema(Some(&model.id)));
            contract.owned_groups.push(model.parameters.clone());
            contract.inputs = descriptor
                .inputs
                .iter()
                .map(|p| InputContract {
                    port: p.name.clone(),
                    role: p.name.clone(),
                    min: 0,
                    max: Some(if p.name == PROMPT { 1024 } else { 255 }),
                    // A batch of prompts makes one result per prompt (an answer, or a list of
                    // media that a list port downstream flattens, #519); media roles collect.
                    fans_out: p.name == PROMPT,
                })
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
            let model_id = model.id.clone();
            if definitions.insert(model_id, Definition { model, descriptor, contract }).is_some() {
                return Err(bad(output, "duplicate {noun} model"));
            }
        }
        Ok(Self { output, id, base: base(id, output), snapshot, definitions: Arc::new(definitions) })
    }
    /// The media wired into `mode`'s non-prompt roles, one input per asset: roles in the order
    /// the mode declares them, each role's assets in wire order. Every asset must carry its
    /// role's modality, and each role gets between its `min` and `max` assets.
    fn media(&self, mode: &GenerationMode, inputs: &[InputValue]) -> Result<Vec<GenerateInput>, Error> {
        let mut media = Vec::new();
        for role in mode.inputs.iter().filter(|role| role.role != PROMPT) {
            let mut count = 0usize;
            for input in inputs.iter().filter(|input| input.port == role.role) {
                if role.modality == Modality::Text {
                    return Err(said(ErrorKind::Unsupported, self.output, "connected text only feeds the {noun} prompt"));
                }
                let items = match &input.value {
                    Value::List(items) => items.as_slice(),
                    value => std::slice::from_ref(value),
                };
                for value in items {
                    let Value::Media(asset) = value else {
                        return Err(bad(self.output, "{noun} media inputs must be assets"));
                    };
                    if !role.modality.accepts_mime(asset.mime.essence()) {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            format!("input {} takes {:?} assets, not {}", role.role, role.modality, asset.mime.essence()),
                        ));
                    }
                    count = count.saturating_add(1);
                    if count > usize::from(role.max) {
                        return Err(Error::new(ErrorKind::InvalidInput, format!("input {} takes at most {} assets", role.role, role.max)));
                    }
                    media.push(GenerateInput { role: role.role.clone(), value: value.clone() });
                }
            }
            if count < usize::from(role.min) {
                return Err(Error::new(ErrorKind::InvalidInput, format!("input {} needs at least {} assets", role.role, role.min)));
            }
        }
        Ok(media)
    }
    pub fn model(&self, id: &str) -> Option<&GenerationModel> {
        self.definitions.get(id).map(|d| &d.model)
    }
    /// Every model this kind defines, by id.
    pub fn models(&self) -> impl Iterator<Item = &GenerationModel> {
        self.definitions.values().map(|d| &d.model)
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
            return Err(self.unavailable());
        };
        self.definitions.get(id).ok_or_else(|| self.unavailable())
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
        let definition = self.definitions.get(destination).ok_or_else(|| self.unavailable())?;
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
        if node.kind != self.id || node.inputs != d.descriptor.inputs || node.outputs != d.descriptor.outputs {
            return Err(invalid("generation ports do not match the selected model"));
        }
        // Generation submits through submission() in plan; visible is only its identity.
        d.contract.resolve(&node.params, ready)?.into_identity(ready)
    }
    fn instantiate_instance(&self, id: NodeId, mut config: BTreeMap<String, Value>, supplied: &ParamValues) -> Result<Node, Error> {
        let mut node = Node::new(id, self.id);
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
        let mode = match self.output {
            Modality::Text => "text-to-text",
            Modality::Mesh => "text-to-mesh",
            _ => "text-to-image",
        };
        inputs.iter().all(|p| p.modality == Modality::Text).then(|| ModeId::new(mode).ok()).flatten()
    }
    /// Wired media is accepted when one of the model's modes declares a role of every connected
    /// modality (#502); `plan` then checks roles, counts and asset types by port. The mode named
    /// here is advisory: `plan` resolves the mode again from asset counts and may pick another.
    fn instance_mode(&self, node: &Node, inputs: &[PortType]) -> Result<Option<ModeId>, Error> {
        self.resolve_instance(node)?;
        if inputs.iter().all(|p| p.modality == Modality::Text) {
            return Ok(self.mode(inputs));
        }
        Ok(self
            .definition(node)?
            .model
            .modes
            .iter()
            .find(|mode| {
                inputs.iter().all(|p| p.modality == Modality::Text || mode.inputs.iter().any(|role| role.role != PROMPT && role.modality == p.modality))
            })
            .map(|mode| mode.id.clone()))
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        self.validate_instance(ctx.node, true)?;
        let definition = self.definition(ctx.node)?;
        if ctx.inputs.len() > MAX_PROMPTS {
            return Err(bad(self.output, "too many {noun} prompt inputs"));
        }
        // Cards the prompt @mentions (#505) aren't wires: their text goes into the prompt.
        let (mentioned, wired): (Vec<&InputValue>, Vec<&InputValue>) = ctx.inputs.iter().partition(|input| input.port == mention::PORT);
        let inputs: Vec<InputValue> = wired.into_iter().cloned().collect();
        let mut wires = BTreeMap::new();
        for input in &inputs {
            let port = definition.descriptor.inputs.iter().find(|port| port.name == input.port);
            if !port.is_some_and(|port| port.port_type.modality == input.port_type.modality)
                || !definition.contract.inputs.iter().any(|port| port.port == input.port)
            {
                return Err(bad(self.output, "{noun} input names an unavailable port"));
            }
            // The planner folds a port's wires into one list, so a media role counts its assets.
            let n = match (&input.value, input.port == PROMPT) {
                (Value::List(items), false) => items.len(),
                _ => 1,
            };
            let count = wires.entry(input.port.as_str()).or_insert(0usize);
            *count = count.checked_add(n).ok_or_else(|| bad(self.output, "too many {noun} input wires"))?;
        }
        // Count actual connected inputs, not composed text fragments or the local prompt.
        // In particular local text is never a substitute for a required non-inline wire.
        let mode_id = match definition.contract.resolve_mode(&wires, true) {
            Ok(mode) => mode.ok_or_else(|| bad(self.output, "{noun} model has no declared input mode"))?,
            // With one mode, say which role the inputs break rather than that no mode fits.
            Err(error) => match definition.model.modes.as_slice() {
                [only] => return Err(self.media(only, &inputs).err().unwrap_or(error)),
                _ => return Err(error),
            },
        };
        let mode = definition.model.modes.iter().find(|mode| mode.id == *mode_id).ok_or_else(|| bad(self.output, "{noun} model mode is unavailable"))?;
        // Media roles are sent as assets (#502); a required text role other than the prompt has
        // no source yet.
        if !mode
            .inputs
            .iter()
            .all(|r| (r.role == PROMPT && r.modality == Modality::Text && r.min <= 1 && r.max >= 1) || r.modality != Modality::Text || r.min == 0)
        {
            return Err(said(ErrorKind::Unsupported, self.output, "this {noun} model requires unsupported input roles"));
        }
        let media = self.media(mode, &inputs)?;
        let prompt = match mode.inputs.iter().find(|r| r.role == PROMPT && r.modality == Modality::Text) {
            Some(role) => Some(self.prompt(role, ctx, &inputs, &mentioned)?),
            // A mode without a prompt (image to 3D) takes its media alone; its declared counts
            // already refuse wired prompts, and a mode with no inputs at all has nothing to send.
            None if media.is_empty() => return Err(Error::new(ErrorKind::Unsupported, "model has no supported prompt role")),
            None if !mentioned.is_empty() => return Err(bad(self.output, "this {noun} model takes no prompt, so it can't use @mentions")),
            None => None,
        };
        let target = ctx
            .targets
            .get(ctx.node.id, &definition.model.provider)
            .ok_or_else(|| said(ErrorKind::Unsupported, self.output, "{noun} provider route is not configured"))?;
        let params = definition.model.parameters.resolve_selected(&ctx.node.params, true)?.submission()?;
        Ok(vec![JobSpec::Generate {
            node: ctx.node.id,
            outputs: vec![GenerateOutput { port: "out".into(), modality: self.output }],
            provider: definition.model.provider.clone(),
            instance: target.instance(),
            model: definition.model.remote.clone(),
            params,
            inputs: prompt.map(|text| GenerateInput { role: PROMPT.into(), value: Value::Text(text) }).into_iter().chain(media).collect(),
        }])
    }
}
impl GenerationKind {
    /// The prompt a run sends: wired text replaces the card's own where the role is inline, and
    /// @mentioned cards' text replaces their tokens.
    fn prompt(&self, prompt_role: &GenerationRole, ctx: &PlanCtx<'_>, inputs: &[InputValue], mentioned: &[&InputValue]) -> Result<String, Error> {
        let mut texts = Vec::new();
        for input in inputs.iter().filter(|input| input.port == PROMPT) {
            if texts.len() >= MAX_PROMPTS {
                return Err(invalid("too many prompt values"));
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
                            return Err(bad(self.output, "{noun} prompts must contain text"));
                        };
                        texts.push(ConnectedText { order: u32::try_from(texts.len()).map_err(|_| invalid("too many prompt values"))?, tie: [0; 16], text });
                    }
                }
                _ => return Err(bad(self.output, "{noun} prompts must contain text")),
            }
        }
        let local = match ctx.node.params.get(PROMPT) {
            Some(Value::Text(text)) => text.as_str(),
            None => "",
            _ => return Err(bad(self.output, "{noun} prompt must be text")),
        };
        let local = if mentioned.is_empty() { Cow::Borrowed(local) } else { Cow::Owned(mention::substitute(local, &mentioned_texts(mentioned)?)?) };
        let local = local.as_ref();
        let prompt = compose_text(TextPrecedence::ConnectedReplaces, prompt_role.inline.then_some(local), &texts, "\n")?;
        if prompt.trim().is_empty() {
            return Err(bad(self.output, "{noun} generation needs a prompt"));
        }
        Ok(prompt)
    }
}

/// The text each @mentioned card brings, by card.
fn mentioned_texts<'v>(inputs: &[&'v InputValue]) -> Result<BTreeMap<NodeId, &'v str>, Error> {
    let mut texts = BTreeMap::new();
    for input in inputs {
        let values = match &input.value {
            Value::List(items) => items.as_slice(),
            value => std::slice::from_ref(value),
        };
        if values.len() != input.origins.len() {
            return Err(invalid("a mentioned card's text arrived without its card"));
        }
        for (value, origin) in values.iter().zip(&input.origins) {
            let Value::Text(text) = value else { return Err(invalid("a mentioned card gives no text")) };
            // The planner feeds one text per card; several would leave a token ambiguous.
            if texts.insert(*origin, text.as_str()).is_some() {
                return Err(invalid("a mentioned card gave several texts; mention a card with one text"));
            }
        }
    }
    Ok(texts)
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
