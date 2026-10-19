//! The router: a transparent pass-through hub. One set of sources goes in, any number of
//! consumers read from it, and the run planner resolves it away (it adds no step and no cost).
//!
//! A router carries one modality, chosen per instance in `config["modality"]` (`text` when
//! absent). Its input takes a list (any number of noodles, in order); its output is a single
//! port so it can feed any input of that modality.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Error, Modality, ParamSchema, PortType, Value};
use std::borrow::Cow;

/// The router's kind id.
pub const ID: &str = "structure.router";
/// The config key holding the router's modality.
pub const MODALITY: &str = "modality";

fn modality_name(modality: Modality) -> &'static str {
    match modality {
        Modality::Text => "text",
        Modality::Image => "image",
        Modality::Video => "video",
        Modality::Audio => "audio",
        Modality::Mesh => "mesh",
        Modality::Document => "document",
    }
}

/// The modality a router node carries.
pub fn modality(node: &Node) -> Result<Modality, Error> {
    match node.config.get(MODALITY) {
        None => Ok(Modality::Text),
        Some(Value::Text(name)) => [Modality::Text, Modality::Image, Modality::Video, Modality::Audio, Modality::Mesh, Modality::Document]
            .into_iter()
            .find(|m| modality_name(*m) == name)
            .ok_or_else(|| invalid("a router's modality must be text, image, video, audio, mesh or document")),
        Some(_) => Err(invalid("a router's modality must be a name")),
    }
}

/// The `config` that makes a router carry `modality`.
pub fn config(modality: Modality) -> std::collections::BTreeMap<String, Value> {
    [(MODALITY.to_owned(), Value::Text(modality_name(modality).to_owned()))].into_iter().collect()
}

fn descriptor(modality: Modality) -> NodeDescriptor {
    NodeDescriptor {
        id: ID.into(),
        label: "Router".into(),
        category: Category::Structure,
        inputs: vec![Port { name: "in".into(), port_type: PortType { modality, many: true } }],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality, many: false } }],
        params: ParamSchema::default(),
        quick_add: Some("R".into()),
    }
}

pub struct RouterKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> RouterKind {
    RouterKind { descriptor: descriptor(Modality::Text) }
}

impl NodeKind for RouterKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        match modality(node)? {
            Modality::Text => Ok(Cow::Borrowed(&self.descriptor)),
            other => Ok(Cow::Owned(descriptor(other))),
        }
    }
    /// A router passes anything through.
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("router").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Err(invalid("routers are resolved at plan time and never run"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::NodeId;

    #[test]
    fn modality_comes_from_config_and_sets_both_ports() {
        let kind = kind();
        let text = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
        assert_eq!(modality(&text).unwrap(), Modality::Text);
        assert_eq!(text.inputs.first().map(|p| p.port_type), Some(PortType { modality: Modality::Text, many: true }));
        let image = kind.instantiate_instance(NodeId::from_bytes([2; 16]), config(Modality::Image), &Default::default()).unwrap();
        assert_eq!(image.outputs.first().map(|p| p.port_type), Some(PortType { modality: Modality::Image, many: false }));
        kind.validate_instance(&image, true).unwrap();
        let mut bad = text.clone();
        bad.config.insert(MODALITY.into(), Value::Text("smell".into()));
        assert!(kind.resolve_instance(&bad).is_err());
        bad.config.insert(MODALITY.into(), Value::Bool(true));
        assert!(kind.resolve_instance(&bad).is_err());
        assert!(kind.plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &text, inputs: &[] }).is_err());
    }
}
