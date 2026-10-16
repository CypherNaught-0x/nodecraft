use nodecraft_graph::{Node, Port};
use nodecraft_types::{Error, ErrorKind, NodeId, ParamSchema, ParamValues, PortType, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
        node.title = self.label.clone();
        node.inputs = self.inputs.clone();
        node.outputs = self.outputs.clone();
        node.params = values;
        Ok(node)
    }

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

pub struct InputValue {
    pub port: String,
    pub port_type: PortType,
    pub value: Value,
}

pub struct PlanCtx<'a> {
    pub node: &'a Node,
    pub inputs: &'a [InputValue],
}

/// Pure job descriptions consumed by the run planner. Source jobs emit existing values/assets;
/// they perform no decoding, filesystem access or paid provider calls.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobSpec {
    Source { node: NodeId, output: String, value: Value },
}

pub trait NodeKind: Send + Sync {
    fn descriptor(&self) -> &NodeDescriptor;
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId>;
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error>;
}
