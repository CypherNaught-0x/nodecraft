//! Port and mode declarations of the descriptor contract (#162): what each port is for, how
//! many wires it takes, and which input combinations select which mode. Parameters use the
//! L0 [`Contract`]. The specification is `docs/descriptor-contract.md`.

use crate::{ModeId, NodeDescriptor, descriptor::identifier, descriptor::invalid};
use nodecraft_types::{Contract, ContractVersion, Error, ErrorKind, ParamOwner};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const MAX_MODES: usize = 64;

/// An input port's role and how many wires it takes. Wires into one port are ordered by
/// their edge `order`, then edge id, and that order is the order a job receives them in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputContract {
    pub port: String,
    /// What the input is for (`prompt`, `start_frame`, `style_reference`); distinct roles may
    /// share a modality but never a meaning.
    pub role: String,
    /// At least this many wires before the node can run; above zero means required.
    #[serde(default)]
    pub min: u16,
    /// At most this many; absent means unbounded (list ports only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u16>,
    /// A list port that runs the node once per batch item (1.2, #515): a batch, or a result a
    /// batch fanned out, wired here takes its wire's place one item at a time instead of
    /// collecting every item into one run. Single ports always fan out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fans_out: bool,
}

impl InputContract {
    pub fn required(&self) -> bool {
        self.min > 0
    }
}

/// An output port's role and whether it holds one value or a collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputContract {
    pub port: String,
    pub role: String,
    pub collection: bool,
}

/// How many wires a mode needs on one port.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortCount {
    pub port: String,
    #[serde(default)]
    pub min: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<u16>,
}

/// A mode selected by the connected inputs, e.g. `first_last` when both frame ports are wired.
/// Ports a mode does not list may hold any count their own bounds allow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeContract {
    pub mode: ModeId,
    pub inputs: Vec<PortCount>,
    /// The settings this mode sends, when it sends only some (#559); `None` sends them all.
    /// Surfaces hide the others while this mode is the wired one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Vec<String>>,
}

impl ModeContract {
    /// Whether this mode sends the setting `name`.
    pub fn sends(&self, name: &str) -> bool {
        self.params.as_ref().is_none_or(|names| names.iter().any(|n| n == name))
    }
}

/// Everything a surface needs to describe, check and edit one node kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeContract {
    #[serde(default)]
    pub version: ContractVersion,
    pub kind: String,
    pub inputs: Vec<InputContract>,
    pub outputs: Vec<OutputContract>,
    /// Empty: the kind decides its mode itself (`NodeKind::mode`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modes: Vec<ModeContract>,
    pub params: Contract,
    /// Additional semantic owners, never flattened into the node's namespace. Their
    /// disjoint schemas follow params.schema in the instance descriptor's layout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owned_groups: Vec<Contract>,
}

impl NodeContract {
    /// The contract a descriptor implies: each port's role is its name, inputs are optional,
    /// single ports take at most one wire, and parameters gain only their implied rules.
    pub fn derive(descriptor: &NodeDescriptor) -> Self {
        let owner = if descriptor.category == crate::Category::Action { ParamOwner::Action } else { ParamOwner::Node };
        Self {
            version: ContractVersion::CURRENT,
            kind: descriptor.id.clone(),
            inputs: descriptor
                .inputs
                .iter()
                .map(|port| InputContract {
                    port: port.name.clone(),
                    role: port.name.clone(),
                    min: 0,
                    max: (!port.port_type.many).then_some(1),
                    fans_out: false,
                })
                .collect(),
            outputs: descriptor
                .outputs
                .iter()
                .map(|port| OutputContract { port: port.name.clone(), role: port.name.clone(), collection: port.port_type.many })
                .collect(),
            modes: Vec::new(),
            params: Contract::from_schema(descriptor.id.clone(), owner, descriptor.params.clone()),
            owned_groups: Vec::new(),
        }
    }

    /// The contract must describe exactly `descriptor`'s ports (same names, same order) and
    /// parameter schema, with possible bounds and modes.
    pub fn validate(&self, descriptor: &NodeDescriptor) -> Result<(), Error> {
        if self.version.major != ContractVersion::CURRENT.major {
            return Err(Error::new(ErrorKind::Unsupported, "this node contract needs a newer NodeCraft"));
        }
        if self.kind != descriptor.id || self.params.namespace != descriptor.id {
            return Err(invalid("a node contract must describe its own kind and parameters"));
        }
        self.validate_ownership(descriptor)?;
        let inputs: Vec<_> = self.inputs.iter().map(|input| input.port.as_str()).collect();
        let outputs: Vec<_> = self.outputs.iter().map(|output| output.port.as_str()).collect();
        if inputs != descriptor.inputs.iter().map(|port| port.name.as_str()).collect::<Vec<_>>()
            || outputs != descriptor.outputs.iter().map(|port| port.name.as_str()).collect::<Vec<_>>()
        {
            return Err(invalid("a node contract must list the descriptor's ports in order"));
        }
        let mut roles = BTreeSet::new();
        for (input, port) in self.inputs.iter().zip(&descriptor.inputs) {
            if !identifier(&input.role) {
                return Err(invalid("a port role is a dotted lowercase id"));
            }
            if !roles.insert(input.role.as_str()) {
                return Err(invalid("two inputs of a node contract share a role"));
            }
            if input.max.is_some_and(|max| max < input.min || max == 0) || (!port.port_type.many && input.max.is_none_or(|max| max > 1)) {
                return Err(invalid("an input's wire bounds are impossible for its port"));
            }
            if input.fans_out && !port.port_type.many {
                return Err(invalid("only a list input is marked as fanning out"));
            }
        }
        for (output, port) in self.outputs.iter().zip(&descriptor.outputs) {
            if !identifier(&output.role) || output.collection != port.port_type.many {
                return Err(invalid("an output's role or collection flag does not match its port"));
            }
        }
        if self.modes.len() > MAX_MODES {
            return Err(invalid("a node contract declares too many modes"));
        }
        let mut modes = BTreeSet::new();
        for mode in &self.modes {
            if !modes.insert(mode.mode.as_str()) {
                return Err(invalid("a node contract declares a mode twice"));
            }
            let mut ports = BTreeSet::new();
            for count in &mode.inputs {
                let input = self.inputs.iter().find(|input| input.port == count.port).ok_or_else(|| invalid("a mode names an input the node does not have"))?;
                let max = count.max.or(input.max);
                if !ports.insert(count.port.as_str())
                    || max.is_some_and(|max| max < count.min.max(input.min))
                    || input.max.is_some_and(|limit| count.min > limit)
                {
                    return Err(invalid("a mode needs a wire count its input cannot hold"));
                }
            }
            if let Some(names) = &mode.params {
                let mut seen = BTreeSet::new();
                for name in names {
                    if !seen.insert(name.as_str())
                        || !std::iter::once(&self.params).chain(&self.owned_groups).any(|group| group.schema.params.iter().any(|param| param.name == *name))
                    {
                        return Err(invalid("a mode lists a setting the node does not have, or lists one twice"));
                    }
                }
            }
        }
        for (index, first) in self.modes.iter().enumerate() {
            if self.modes.iter().skip(index.saturating_add(1)).any(|second| self.overlap(first, second)) {
                return Err(invalid("two modes of a node contract accept the same wiring"));
            }
        }
        Ok(())
    }

    /// The wire counts `mode` accepts on `input`, as an inclusive range (`None` is no limit).
    fn accepts(&self, mode: &ModeContract, input: &InputContract) -> (u16, Option<u16>) {
        match mode.inputs.iter().find(|count| count.port == input.port) {
            Some(count) => (count.min, count.max.or(input.max)),
            None => (0, input.max),
        }
    }

    /// Whether some wiring fits both modes, which would make the choice ambiguous.
    fn overlap(&self, first: &ModeContract, second: &ModeContract) -> bool {
        self.inputs.iter().all(|input| {
            let (first_min, first_max) = self.accepts(first, input);
            let (second_min, second_max) = self.accepts(second, input);
            let low = first_min.max(second_min);
            [first_max, second_max].into_iter().flatten().all(|high| low <= high)
        })
    }

    /// Check wire counts against the inputs' bounds, then pick the one declared mode they fit.
    /// `wires` maps input port names to how many wires reach them. With no declared modes, or
    /// with `ready` false and no mode fitting yet, this only checks bounds and returns `None`.
    pub fn resolve_mode(&self, wires: &BTreeMap<&str, usize>, ready: bool) -> Result<Option<&ModeId>, Error> {
        for port in wires.keys() {
            if !self.inputs.iter().any(|input| input.port == *port) {
                return Err(invalid("a wire reaches an input the node does not have"));
            }
        }
        let count = |port: &str| wires.get(port).copied().unwrap_or(0);
        for input in &self.inputs {
            let wired = count(&input.port);
            if input.max.is_some_and(|max| wired > usize::from(max)) {
                return Err(Error::new(ErrorKind::InvalidInput, format!("input {} takes at most {} wires", input.port, input.max.unwrap_or(0))));
            }
            if ready && wired < usize::from(input.min) {
                return Err(Error::new(ErrorKind::InvalidInput, format!("input {} needs at least {} wires", input.port, input.min)));
            }
        }
        if self.modes.is_empty() {
            return Ok(None);
        }
        let fits = |mode: &&ModeContract| {
            mode.inputs.iter().all(|need| {
                let wired = count(&need.port);
                wired >= usize::from(need.min) && need.max.is_none_or(|max| wired <= usize::from(max))
            })
        };
        let mut matching = self.modes.iter().filter(fits);
        match (matching.next(), matching.next()) {
            (Some(mode), None) => Ok(Some(&mode.mode)),
            // A node still being wired up has no mode yet; that is not an error until it runs.
            (None, _) if !ready => Ok(None),
            (None, _) => Err(Error::new(ErrorKind::Unsupported, "no mode of this node accepts these inputs")),
            (Some(first), Some(second)) => Err(Error::new(
                ErrorKind::InvalidInput,
                format!("these inputs fit more than one mode ({}, {}); connect or remove inputs to choose", first.mode.as_str(), second.mode.as_str()),
            )),
        }
    }
}

mod ownership;

#[cfg(test)]
mod tests;
