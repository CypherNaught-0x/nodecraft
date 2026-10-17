use nodecraft_graph::{Node, Port};
use nodecraft_types::{Error, ErrorKind, Modality, NodeId, ParamSchema, ParamValues, PortType, Value};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Source,
    Generation,
    Action,
    Structure,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDescriptor {
    pub id: String,
    pub label: String,
    pub category: Category,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub params: ParamSchema,
    pub quick_add: Option<String>,
}

pub(crate) fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.split('.').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

impl NodeDescriptor {
    pub fn validate(&self) -> Result<(), Error> {
        if !identifier(&self.id) || self.label.trim().is_empty() || self.label.len() > 256 {
            return Err(invalid("node descriptor needs a stable bounded id and label"));
        }
        if self.quick_add.as_ref().is_some_and(|key| key.is_empty() || key.len() > 32 || key.chars().any(char::is_control)) {
            return Err(invalid("invalid quick-add shortcut"));
        }
        for ports in [&self.inputs, &self.outputs] {
            if ports.len() > 256 {
                return Err(invalid("descriptor exceeds port limit"));
            }
            let mut names = BTreeSet::new();
            for port in ports {
                if !identifier(&port.name) || !names.insert(&port.name) {
                    return Err(invalid("port names must be bounded and unique per direction"));
                }
            }
        }
        self.params.validate()
    }

    /// Creates detached graph data. The caller inserts it through a command/changeset.
    /// Missing required values are allowed until planning, so media can be selected later.
    pub fn instantiate(&self, id: NodeId, supplied: &ParamValues) -> Result<Node, Error> {
        self.validate()?;
        let values = self.params.resolve(supplied, false)?.values;
        let mut node = Node::new(id, &self.id);
        // Empty means automatic title: the shell derives it from the prompt or this label.
        node.inputs = self.inputs.clone();
        node.outputs = self.outputs.clone();
        node.params = values;
        Ok(node)
    }

    /// Static-descriptor helper. Stored/dynamic node consumers use NodeKind::validate_instance.
    pub fn validate_node(&self, node: &Node, ready: bool) -> Result<ParamValues, Error> {
        self.validate()?;
        if node.kind != self.id || node.inputs != self.inputs || node.outputs != self.outputs {
            return Err(invalid("node kind or ports do not match its descriptor"));
        }
        Ok(self.params.resolve(&node.params, ready)?.values)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModeId(String);

impl ModeId {
    pub fn new(value: impl Into<String>) -> Result<Self, Error> {
        Self::try_from(value.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn source() -> Self {
        Self("source".into())
    }
}
impl TryFrom<String> for ModeId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        if identifier(&value) { Ok(Self(value)) } else { Err(invalid("invalid mode id")) }
    }
}
impl From<ModeId> for String {
    fn from(value: ModeId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug)]
pub struct InputValue {
    pub port: String,
    pub port_type: PortType,
    pub value: Value,
}

#[derive(Clone, Debug)]
pub struct PlanCtx<'a> {
    pub node: &'a Node,
    pub inputs: &'a [InputValue],
}

/// Pure job descriptions consumed by the run planner. Source jobs emit existing values/assets;
/// they perform no decoding, filesystem access or paid provider calls. Generate jobs call a model
/// through a provider adapter (#79): the host looks the provider up by id, sends it the model,
/// parameters and role-tagged inputs, and maps what it returns onto `output`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobSpec {
    Source {
        node: NodeId,
        output: String,
        value: Value,
    },
    Generate {
        node: NodeId,
        /// Where results go: each result goes to the first port whose modality it has (several
        /// results on one port make a list output's items). A result no port takes fails the job.
        outputs: Vec<GenerateOutput>,
        /// The provider adapter's id (`fal`, `comfyui`, `mock`); also the scheduler lane.
        provider: String,
        /// The provider's model id (from the catalog).
        model: String,
        /// The model's parameters, as the node resolved them.
        params: ParamValues,
        inputs: Vec<GenerateInput>,
    },
}

/// An output port a model call fills, and the modality of results it takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateOutput {
    pub port: String,
    pub modality: Modality,
}

/// One input to a model call, in a role such as `prompt`, `system` or `image`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerateInput {
    pub role: String,
    pub value: Value,
}

/// A proposed explicit schema migration. Apply `node` with graph's SetNode only after showing
/// the warnings; migrations never silently discard stale parameters in the live document.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeMigration {
    pub node: Node,
    pub warnings: Vec<String>,
}

pub trait NodeKind: Send + Sync {
    fn descriptor(&self) -> &NodeDescriptor;
    /// Static palette metadata only. Instance consumers call resolve_instance().
    fn instance<'a>(&'a self, _node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        Ok(Cow::Borrowed(self.descriptor()))
    }
    /// Validates detached input before invoking a dynamic kind, then validates its descriptor
    /// before callers clone schema/ports. UI, MCP and planning use this checked entry point.
    fn resolve_instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        node.validate_payload()?;
        let descriptor = self.instance(node)?;
        descriptor.validate()?;
        if node.kind != descriptor.id || descriptor.id != self.descriptor().id {
            return Err(invalid("instance descriptor changed its node kind"));
        }
        Ok(descriptor)
    }
    fn validate_instance(&self, node: &Node, ready: bool) -> Result<crate::ResolvedParams, Error> {
        let descriptor = self.resolve_instance(node)?;
        if node.inputs != descriptor.inputs || node.outputs != descriptor.outputs {
            return Err(invalid("node ports do not match its instance descriptor"));
        }
        descriptor.params.resolve(&node.params, ready)
    }
    fn instantiate_instance(&self, id: NodeId, config: BTreeMap<String, Value>, supplied: &ParamValues) -> Result<Node, Error> {
        self.descriptor().validate()?;
        let mut node = Node::new(id, &self.descriptor().id);
        node.config = config;
        let descriptor = self.resolve_instance(&node)?;
        node.params = descriptor.params.resolve(supplied, false)?.values;
        node.inputs = descriptor.inputs.clone();
        node.outputs = descriptor.outputs.clone();
        node.validate_payload()?;
        Ok(node)
    }
    /// Kind-owned version migration. Unknown config keys must be retained. Callers use
    /// prepare_migration(), show warnings, and apply the returned node through SetNode.
    fn migrate(&self, _node: &Node) -> Result<Option<NodeMigration>, Error> {
        Ok(None)
    }
    fn prepare_migration(&self, node: &Node) -> Result<Option<NodeMigration>, Error> {
        node.validate_payload()?;
        let migration = self.migrate(node)?;
        if let Some(proposed) = &migration {
            if proposed.node.id != node.id || proposed.node.kind != node.kind {
                return Err(invalid("migration changed node identity"));
            }
            if proposed.warnings.len() > 256 || proposed.warnings.iter().any(|warning| warning.len() > 4096) {
                return Err(invalid("migration warnings exceed limits"));
            }
            self.validate_instance(&proposed.node, false)?;
        }
        Ok(migration)
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId>;
    /// Dynamic kinds can override input compatibility using their per-instance config.
    fn instance_mode(&self, node: &Node, inputs: &[PortType]) -> Result<Option<ModeId>, Error> {
        self.resolve_instance(node)?;
        Ok(self.mode(inputs))
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error>;
}
