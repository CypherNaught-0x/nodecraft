//! The technique node (#94): one published Technique version, collapsed into a node. Its
//! interface (which version it pins, its typed inputs and outputs, and the parameters a caller may
//! override) is stored on the node in `config["technique"]`, so its ports and parameters never
//! change unless it is upgraded, whatever happens to other versions.
//!
//! A technique node is inlined before planning (`nodecraft_exec::technique`): the planner runs the
//! version's nodes with the instance's inputs and parameters, and the instance's own step gives
//! their results back as its outputs. Its plan is that last step: one source job per output value.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, Registry, descriptor::invalid};
use nodecraft_graph::{Canvas, Node, Port};
use nodecraft_types::{CanvasId, Error, ErrorKind, ParamSchema, ParamSpec, PortType, TechniqueId, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// The technique node's kind id.
pub const ID: &str = "structure.technique";
/// The config key holding the node's [`Interface`], as JSON text.
pub const CONFIG: &str = "technique";
/// Inlining names the inputs that carry an output's value `output:<port>`, which no real port can
/// be called.
pub const OUTPUT_PREFIX: &str = "output:";
/// Most bytes of an interface's JSON.
const MAX_INTERFACE_BYTES: usize = 256 * 1024;

/// What a technique node pins: one version, and the ports and parameters it exposes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    pub technique: TechniqueId,
    pub version: u32,
    /// The canvas holding that version.
    pub canvas: CanvasId,
    pub name: String,
    pub inputs: Vec<Exposed>,
    pub outputs: Vec<Exposed>,
    /// One per exposed parameter, in the version's order: the inner parameter's schema under the
    /// node's own parameter name, labelled with the technique's name for it, defaulting to the
    /// value the version was published with.
    pub params: Vec<ParamSpec>,
}

/// One input or output: the node's port for it, and the technique's name for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exposed {
    pub port: String,
    pub name: String,
    pub port_type: PortType,
}

impl Interface {
    /// Whether this interface still describes `other`, the one its version gives now: the same
    /// version, ports and parameter names. Parameter schemas may differ (a catalog update can
    /// change an inner model's controls) without the instance falling out of step.
    pub fn matches(&self, other: &Self) -> bool {
        let names = |params: &[ParamSpec]| params.iter().map(|spec| (spec.name.clone(), spec.label.clone())).collect::<Vec<_>>();
        self.technique == other.technique
            && self.version == other.version
            && self.canvas == other.canvas
            && self.inputs == other.inputs
            && self.outputs == other.outputs
            && names(&self.params) == names(&other.params)
    }

    /// The config that makes a technique node pin this interface.
    pub fn config(&self) -> Result<BTreeMap<String, Value>, Error> {
        let text = serde_json::to_string(self).map_err(|_| Error::new(ErrorKind::Internal, "a technique interface could not be stored"))?;
        if text.len() > MAX_INTERFACE_BYTES {
            return Err(Error::new(ErrorKind::Overflow, "this technique exposes too much to place as a node"));
        }
        Ok([(CONFIG.to_owned(), Value::Text(text))].into_iter().collect())
    }
}

/// The interface a technique node pins.
pub fn interface(node: &Node) -> Result<Interface, Error> {
    match node.config.get(CONFIG) {
        Some(Value::Text(text)) if text.len() <= MAX_INTERFACE_BYTES => {
            serde_json::from_str(text).map_err(|_| invalid("a technique node's interface is malformed"))
        }
        Some(Value::Text(_)) => Err(invalid("a technique node's interface is too large")),
        _ => Err(invalid("a technique node needs the version it runs")),
    }
}

/// A port or parameter name for `label`: lowercase letters, digits and `_`, unique among `taken`.
pub(crate) fn slug(label: &str, taken: &mut BTreeSet<String>) -> String {
    let mut base = String::new();
    for c in label.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            base.push(c);
        } else if !base.ends_with('_') && !base.is_empty() {
            base.push('_');
        }
        if base.len() >= 64 {
            break;
        }
    }
    let base = base.trim_end_matches('_');
    let base = if base.is_empty() { "port" } else { base }.to_owned();
    let mut name = base.clone();
    let mut n = 1u32;
    while taken.contains(&name) {
        n = n.saturating_add(1);
        name = format!("{base}_{n}");
    }
    taken.insert(name.clone());
    name
}

/// The interface the published version on `canvas` gives a node placed now. Its parameters are
/// the inner nodes' own, as `kinds` describe them, defaulting to the values the version holds.
pub fn interface_of(canvas: &Canvas, kinds: &Registry) -> Result<Interface, Error> {
    let info = canvas.technique_info().ok_or_else(|| invalid("that canvas isn't a published technique"))?;
    if info.retired {
        return Err(Error::new(ErrorKind::NotFound, "that technique version was removed"));
    }
    let ports = |ports: &[nodecraft_graph::TechniquePort]| {
        let mut taken = BTreeSet::new();
        ports.iter().map(|port| Exposed { port: slug(&port.name, &mut taken), name: port.name.clone(), port_type: port.port_type }).collect()
    };
    let mut taken = BTreeSet::new();
    let mut params = Vec::with_capacity(info.params.len());
    for exposed in &info.params {
        let node = canvas.nodes().get(&exposed.node).ok_or_else(|| invalid("a technique parameter's node is missing"))?;
        let schema = kinds.get(&node.kind)?.resolve_instance(node)?;
        let spec = schema.params.params.iter().find(|spec| spec.name == exposed.param).ok_or_else(|| invalid("a technique parameter is gone"))?;
        params.push(ParamSpec {
            name: slug(&exposed.name, &mut taken),
            label: exposed.name.clone(),
            description: if exposed.description.is_empty() { spec.description.clone() } else { Some(exposed.description.clone()) },
            control: spec.control.clone(),
            required: false,
            default: node.params.get(&exposed.param).cloned().or_else(|| spec.default.clone()),
            // It names the inner node's other parameters, which the technique doesn't expose.
            visible_if: None,
        });
    }
    Ok(Interface {
        technique: info.id,
        version: info.version,
        canvas: canvas.id(),
        name: info.name.clone(),
        inputs: ports(&info.inputs),
        outputs: ports(&info.outputs),
        params,
    })
}

fn descriptor(interface: Option<&Interface>) -> NodeDescriptor {
    let ports = |exposed: &[Exposed]| exposed.iter().map(|e| Port { name: e.port.clone(), port_type: e.port_type }).collect();
    NodeDescriptor {
        id: ID.into(),
        label: "Technique".into(),
        category: Category::Structure,
        inputs: interface.map(|i| ports(&i.inputs)).unwrap_or_default(),
        outputs: interface.map(|i| ports(&i.outputs)).unwrap_or_default(),
        params: ParamSchema { params: interface.map(|i| i.params.clone()).unwrap_or_default() },
        quick_add: None,
    }
}

pub struct TechniqueKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> TechniqueKind {
    TechniqueKind { descriptor: descriptor(None) }
}

impl NodeKind for TechniqueKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        Ok(Cow::Owned(descriptor(Some(&interface(node)?))))
    }
    /// Never offered by Turn into: a technique node is placed from a published version.
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        None
    }
    fn instance_mode(&self, node: &Node, _: &[PortType]) -> Result<Option<ModeId>, Error> {
        self.resolve_instance(node)?;
        Ok(ModeId::new("technique").ok())
    }
    /// One source job per output value, from the inputs inlining gives the step. A list output
    /// gets one job per item, so its result is the list itself.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let interface = interface(ctx.node)?;
        let mut jobs = Vec::new();
        for output in &interface.outputs {
            let port = format!("{OUTPUT_PREFIX}{}", output.port);
            let input = ctx
                .inputs
                .iter()
                .find(|input| input.port == port)
                .ok_or_else(|| Error::new(ErrorKind::Unsupported, "a technique node runs only once its technique is inlined"))?;
            match (&input.value, output.port_type.many) {
                (Value::List(values), true) => {
                    jobs.extend(values.iter().map(|value| JobSpec::Source { node: ctx.node.id, output: output.port.clone(), value: value.clone() }))
                }
                (_, true) => return Err(invalid("a technique's list output didn't get a list")),
                (value, false) => jobs.push(JobSpec::Source { node: ctx.node.id, output: output.port.clone(), value: value.clone() }),
            }
        }
        Ok(jobs)
    }
}

#[cfg(test)]
mod tests;
