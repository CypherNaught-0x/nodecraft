//! Published Techniques (#93): a version of a workflow, frozen as a canvas of kind technique
//! (`docs/decisions/2026-10-20-techniques.md`). The canvas holds a copy of the traced subgraph;
//! its [`Technique`] names the nodes that take the technique's inputs and give its outputs. A
//! published version is never edited: every change to its canvas is refused, so a new version is
//! a new canvas, and removing or re-adding a whole version stays undoable.
use std::collections::BTreeSet;

use nodecraft_types::{CanvasId, Error, NodeId, PortType, TechniqueId};
use serde::{Deserialize, Serialize};

use crate::validate::{invalid, text};
use crate::{Canvas, CanvasKind, Edge, Node};

/// Most bytes in a technique's name or a port's or parameter's name.
pub const MAX_TECHNIQUE_NAME_BYTES: usize = 256;
/// Most bytes in a technique's, a port's or a parameter's description.
pub const MAX_TECHNIQUE_DESCRIPTION_BYTES: usize = 4096;
/// Most tags one technique carries.
pub const MAX_TECHNIQUE_TAGS: usize = 32;
/// Most inputs, outputs or user-editable parameters one technique exposes.
pub const MAX_TECHNIQUE_PORTS: usize = 64;
/// Most nodes one published version holds.
pub const MAX_TECHNIQUE_NODES: usize = 500;
const MAX_TAG_BYTES: usize = 64;

/// What a published version is: which technique, which version of it, what it is called and
/// which of its nodes take its inputs, give its outputs and expose parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Technique {
    /// The same for every version of one technique.
    pub id: TechniqueId,
    /// 1 for the first version, one more for each republish.
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Nodes with no incoming wires whose content a caller replaces.
    #[serde(default)]
    pub inputs: Vec<TechniquePort>,
    /// Nodes whose results the technique gives back.
    pub outputs: Vec<TechniquePort>,
    /// Parameters a caller may override.
    #[serde(default)]
    pub params: Vec<TechniqueParam>,
    /// The canvas it was published from, if it still exists; informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CanvasId>,
}

/// One exposed input or output: a node of the published canvas and one of its output ports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechniquePort {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub node: NodeId,
    pub port: String,
    pub port_type: PortType,
}

/// One parameter of a node of the published canvas that a caller may override.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechniqueParam {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub node: NodeId,
    pub param: String,
}

impl Canvas {
    /// A published technique version holding `nodes` and `edges`, checked as a whole.
    pub fn technique(id: CanvasId, title: impl Into<String>, technique: Technique, nodes: Vec<Node>, edges: Vec<Edge>) -> Result<Self, Error> {
        let mut canvas = Self { kind: CanvasKind::Technique, technique: Some(technique), ..Self::new(id, title) };
        for node in nodes {
            if canvas.nodes.insert(node.id, node).is_some() {
                return Err(invalid("duplicate node id in technique"));
            }
        }
        for edge in edges {
            if canvas.edges.insert(edge.id, edge).is_some() {
                return Err(invalid("duplicate edge id in technique"));
            }
        }
        canvas.validate()?;
        Ok(canvas)
    }

    /// What this canvas publishes, if it is a technique version.
    pub fn technique_info(&self) -> Option<&Technique> {
        self.technique.as_ref()
    }
}

fn name(value: &str, what: &str) -> Result<(), Error> {
    if value.trim().is_empty() || value.len() > MAX_TECHNIQUE_NAME_BYTES {
        return Err(invalid(&format!("a technique's {what} name must have 1 to 256 bytes")));
    }
    Ok(())
}

fn port(canvas: &Canvas, port: &TechniquePort) -> Result<(), Error> {
    name(&port.name, "port")?;
    text(&port.description, MAX_TECHNIQUE_DESCRIPTION_BYTES)?;
    let node = canvas.nodes.get(&port.node).ok_or_else(|| invalid("a technique port's node is missing"))?;
    match node.outputs.iter().find(|output| output.name == port.port) {
        Some(output) if output.port_type == port.port_type => Ok(()),
        Some(_) => Err(invalid("a technique port's type does not match its node")),
        None => Err(invalid("a technique port's node has no such output")),
    }
}

fn ports(canvas: &Canvas, ports: &[TechniquePort]) -> Result<(), Error> {
    if ports.len() > MAX_TECHNIQUE_PORTS {
        return Err(invalid("technique exposes too many ports"));
    }
    let mut names = BTreeSet::new();
    let mut ends = BTreeSet::new();
    for item in ports {
        port(canvas, item)?;
        if !names.insert(item.name.as_str()) || !ends.insert((item.node, item.port.as_str())) {
            return Err(invalid("a technique exposes a port twice"));
        }
    }
    Ok(())
}

impl Technique {
    /// Whether this describes `canvas`: limits, and every port and parameter on a node it holds.
    pub(crate) fn validate(&self, canvas: &Canvas) -> Result<(), Error> {
        if self.version == 0 {
            return Err(invalid("a technique's versions start at 1"));
        }
        name(&self.name, "")?;
        text(&self.description, MAX_TECHNIQUE_DESCRIPTION_BYTES)?;
        text(&self.category, MAX_TECHNIQUE_NAME_BYTES)?;
        if self.tags.len() > MAX_TECHNIQUE_TAGS || self.tags.iter().any(|tag| tag.trim().is_empty() || tag.len() > MAX_TAG_BYTES) {
            return Err(invalid("a technique has at most 32 tags of 1 to 64 bytes"));
        }
        if canvas.nodes.len() > MAX_TECHNIQUE_NODES {
            return Err(invalid("technique exceeds its node limit"));
        }
        if self.outputs.is_empty() {
            return Err(invalid("a technique gives at least one output"));
        }
        ports(canvas, &self.inputs)?;
        ports(canvas, &self.outputs)?;
        let fed: BTreeSet<NodeId> = canvas.edges.values().map(|edge| edge.to.node).collect();
        if self.inputs.iter().any(|input| fed.contains(&input.node)) {
            return Err(invalid("a technique input has incoming wires"));
        }
        if self.params.len() > MAX_TECHNIQUE_PORTS {
            return Err(invalid("technique exposes too many parameters"));
        }
        let mut names = BTreeSet::new();
        let mut ends = BTreeSet::new();
        for param in &self.params {
            name(&param.name, "parameter")?;
            text(&param.description, MAX_TECHNIQUE_DESCRIPTION_BYTES)?;
            text(&param.param, MAX_TECHNIQUE_NAME_BYTES)?;
            if !canvas.nodes.contains_key(&param.node) {
                return Err(invalid("a technique parameter's node is missing"));
            }
            if !names.insert(param.name.as_str()) || !ends.insert((param.node, param.param.as_str())) {
                return Err(invalid("a technique exposes a parameter twice"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Change, Endpoint, Port, Project};
    use nodecraft_types::{EdgeId, Modality, ProjectId};

    const TEXT: PortType = PortType { modality: Modality::Text, many: false };

    fn node(n: u8, inputs: bool) -> Node {
        let mut node = Node::new(NodeId::from_bytes([n; 16]), "source.text");
        node.outputs.push(Port { name: "out".into(), port_type: TEXT });
        if inputs {
            node.inputs.push(Port { name: "in".into(), port_type: TEXT });
        }
        node
    }

    fn port(n: u8, name: &str) -> TechniquePort {
        TechniquePort { name: name.into(), description: String::new(), node: NodeId::from_bytes([n; 16]), port: "out".into(), port_type: TEXT }
    }

    fn info(version: u32) -> Technique {
        Technique {
            id: TechniqueId::from_bytes([9; 16]),
            version,
            name: "Caption".into(),
            description: String::new(),
            category: String::new(),
            tags: vec![],
            inputs: vec![port(1, "subject")],
            outputs: vec![port(2, "caption")],
            params: vec![TechniqueParam { name: "Style".into(), description: String::new(), node: NodeId::from_bytes([2; 16]), param: "text".into() }],
            source: None,
        }
    }

    fn wire() -> Edge {
        Edge {
            id: EdgeId::from_bytes([3; 16]),
            from: Endpoint { node: NodeId::from_bytes([1; 16]), port: "out".into() },
            to: Endpoint { node: NodeId::from_bytes([2; 16]), port: "in".into() },
            order: 0,
        }
    }

    fn version(canvas: u8, nodes: u8, number: u32) -> Canvas {
        let mut info = info(number);
        for port in info.inputs.iter_mut().chain(&mut info.outputs) {
            port.node = NodeId::from_bytes([port.node.as_bytes()[0] + nodes; 16]);
        }
        for param in &mut info.params {
            param.node = NodeId::from_bytes([param.node.as_bytes()[0] + nodes; 16]);
        }
        let mut edge = wire();
        edge.id = EdgeId::from_bytes([3 + nodes; 16]);
        edge.from.node = NodeId::from_bytes([1 + nodes; 16]);
        edge.to.node = NodeId::from_bytes([2 + nodes; 16]);
        Canvas::technique(CanvasId::from_bytes([canvas; 16]), "Caption v1", info, vec![node(1 + nodes, false), node(2 + nodes, true)], vec![edge]).unwrap()
    }

    #[test]
    fn a_published_version_is_frozen_but_can_be_removed_and_restored() {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "P").unwrap();
        let canvas = version(7, 0, 1);
        let id = canvas.id();
        let undo = Change::AddCanvas { canvas }.apply(&mut project).unwrap();
        let before = project.clone();
        for change in [
            Change::SetCanvasTitle { canvas: id, title: "Renamed".into() },
            Change::RemoveNode { canvas: id, id: NodeId::from_bytes([2; 16]) },
            Change::Disconnect { canvas: id, id: EdgeId::from_bytes([3; 16]) },
        ] {
            let error = change.apply(&mut project).unwrap_err();
            assert_eq!(error.kind, nodecraft_types::ErrorKind::Conflict);
        }
        assert_eq!(project, before);
        let redo = undo.apply(&mut project).unwrap();
        assert!(project.canvases().is_empty());
        redo.apply(&mut project).unwrap();
        assert_eq!(project, before);
        // It survives a serde round trip with its description.
        let back: Project = serde_json::from_str(&serde_json::to_string(&project).unwrap()).unwrap();
        assert_eq!(back, project);
        assert_eq!(back.canvases()[&id].technique_info().unwrap().version, 1);
    }

    #[test]
    fn each_version_of_a_technique_is_published_once() {
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "P").unwrap();
        Change::AddCanvas { canvas: version(7, 0, 1) }.apply(&mut project).unwrap();
        assert!(Change::AddCanvas { canvas: version(8, 10, 1) }.apply(&mut project).is_err());
        Change::AddCanvas { canvas: version(8, 10, 2) }.apply(&mut project).unwrap();
    }

    type Breakage = Box<dyn Fn(&mut Technique)>;

    #[test]
    fn a_technique_must_describe_its_canvas() {
        let make = |info: Technique, edges: Vec<Edge>| Canvas::technique(CanvasId::from_bytes([7; 16]), "T", info, vec![node(1, false), node(2, true)], edges);
        assert!(make(info(1), vec![wire()]).is_ok());
        let cases: Vec<Breakage> = vec![
            Box::new(|t| t.version = 0),
            Box::new(|t| t.name = " ".into()),
            Box::new(|t| t.name = "n".repeat(257)),
            Box::new(|t| t.outputs.clear()),
            Box::new(|t| t.outputs[0].node = NodeId::from_bytes([5; 16])),
            Box::new(|t| t.outputs[0].port = "missing".into()),
            Box::new(|t| t.outputs[0].port_type.many = true),
            Box::new(|t| t.outputs.push(port(1, "caption"))),
            Box::new(|t| t.outputs.push(port(2, "again"))),
            Box::new(|t| t.inputs[0] = port(2, "subject")),
            Box::new(|t| t.params[0].node = NodeId::from_bytes([5; 16])),
            Box::new(|t| t.params.push(t.params[0].clone())),
            Box::new(|t| t.tags = vec![String::new()]),
            Box::new(|t| t.tags = vec!["t".into(); 33]),
            Box::new(|t| t.description = "d".repeat(4097)),
        ];
        for (n, case) in cases.iter().enumerate() {
            let mut bad = info(1);
            case(&mut bad);
            assert!(make(bad, vec![wire()]).is_err(), "case {n} was accepted");
        }
        // A plain canvas can't carry a technique, nor a technique canvas go without one.
        let mut plain: serde_json::Value = serde_json::to_value(make(info(1), vec![wire()]).unwrap()).unwrap();
        plain["kind"] = "canvas".into();
        let plain: Canvas = serde_json::from_value(plain).unwrap();
        assert!(plain.validate().is_err());
        let mut bare: serde_json::Value = serde_json::to_value(make(info(1), vec![wire()]).unwrap()).unwrap();
        bare.as_object_mut().unwrap().remove("technique");
        let bare: Canvas = serde_json::from_value(bare).unwrap();
        assert!(bare.validate().is_err());
    }
}
