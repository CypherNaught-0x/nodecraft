//! The Layer Editor kind (#101): every image wired into `layers` becomes a layer, and text and
//! shape layers can be added on top; the output is one composited image. The layers live in the
//! node's `config["nodecraft.layers"]` as JSON (`{"layers": [...]}`, bottom first; the format is
//! `nodecraft-actions::layers`). The host checks them where they are set (`layers.*` commands)
//! and again before it draws; this kind only carries them, with the node each connected image
//! came from, so a layer stays with its image when wires are reordered.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Error, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Unsigned, Value};

/// The kind's id.
pub const ID: &str = "image.layers";
/// The config key holding the layers.
pub const CONFIG: &str = "nodecraft.layers";
/// The parameter the planned job carries the layers in (not a control).
pub const SPEC_PARAM: &str = "nodecraft.layers";
/// The parameter the planned job carries each input's origin node in (not a control).
pub const SOURCES_PARAM: &str = "nodecraft.layers.sources";
/// No layers, as stored.
pub const EMPTY: &str = r#"{"layers":[]}"#;
/// The input port.
pub const INPUT: &str = "layers";
/// The frame's edge range and default, in pixels.
pub const MIN_FRAME: u64 = 100;
pub const MAX_FRAME: u64 = 4096;
pub const DEFAULT_FRAME: u64 = 1024;

/// The layers `node` carries, as stored (no layers when it has none).
pub fn stored(node: &Node) -> Result<&str, Error> {
    match node.config.get(CONFIG) {
        None => Ok(EMPTY),
        Some(Value::Text(json)) => Ok(json),
        Some(_) => Err(invalid("the node's layers can't be read")),
    }
}

fn edge(name: &str, label: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some("In pixels, 100 to 4096.".into()),
        control: ParamControl::Unsigned { min: Some(Unsigned(MIN_FRAME)), max: Some(Unsigned(MAX_FRAME)), step: Unsigned(1) },
        required: true,
        default: Some(Value::Unsigned(Unsigned(DEFAULT_FRAME))),
        visible_if: None,
    }
}

pub struct LayersKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> LayersKind {
    LayersKind {
        descriptor: NodeDescriptor {
            id: ID.into(),
            label: "Layer Editor".into(),
            category: Category::Action,
            inputs: vec![Port { name: INPUT.into(), port_type: PortType { modality: Modality::Image, many: true } }],
            outputs: vec![Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            params: ParamSchema {
                params: vec![
                    edge("width", "Width"),
                    edge("height", "Height"),
                    ParamSpec {
                        name: "background".into(),
                        label: "Background".into(),
                        description: None,
                        control: ParamControl::Color,
                        required: true,
                        default: Some(Value::Text("#ffffff".into())),
                        visible_if: None,
                    },
                    ParamSpec {
                        name: "transparent".into(),
                        label: "Transparent background".into(),
                        description: Some("Leave the frame clear where no layer covers it.".into()),
                        control: ParamControl::Toggle,
                        required: true,
                        default: Some(Value::Bool(false)),
                        visible_if: None,
                    },
                ],
            },
            quick_add: None,
        },
    }
}

impl NodeKind for LayersKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.iter().all(|t| t.modality == Modality::Image).then(|| ModeId::new("action").ok()).flatten()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut params = self.validate_instance(ctx.node, true)?.into_visible_values();
        params.insert(SPEC_PARAM.into(), Value::Text(stored(ctx.node)?.into()));
        let mut inputs = Vec::new();
        let mut sources = Vec::new();
        for input in ctx.inputs.iter().filter(|i| i.port == INPUT) {
            let values = match &input.value {
                Value::List(values) => values.clone(),
                single => vec![single.clone()],
            };
            for (index, value) in values.into_iter().enumerate() {
                let origin = input.origins.get(index).map(ToString::to_string).unwrap_or_default();
                sources.push(Value::Text(origin));
                inputs.push(value);
            }
        }
        params.insert(SOURCES_PARAM.into(), Value::List(sources));
        Ok(vec![JobSpec::Action { node: ctx.node.id, action: ID.into(), params, inputs, output: "image".into() }])
    }
}

#[cfg(test)]
mod tests;
