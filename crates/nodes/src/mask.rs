//! The mask kind (#102): a brush mask painted over an image, for inpainting and outpainting. The
//! strokes live in the node's `config["nodecraft.mask"]` as JSON (`{"strokes": [{size, hardness,
//! erase, points}]}`, normalized to the image); the host renders them over the connected image
//! (`nodecraft-actions::mask`) into a mask the inpainting models read. The strokes are checked
//! where they are set (`node.mask_set`) and again before they render; this kind only carries
//! them. A node without strokes masks nothing, or only the image's transparent areas when
//! "Include transparent areas" is on.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Error, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Value};

/// The kind's id.
pub const ID: &str = "image.mask";
/// The config key holding the strokes.
pub const CONFIG: &str = "nodecraft.mask";
/// The parameter the planned job carries the strokes in (not a control).
pub const SPEC_PARAM: &str = "nodecraft.mask";
/// The empty mask, as stored.
pub const EMPTY: &str = r#"{"strokes":[]}"#;

/// The strokes `node` carries, as stored (the empty mask when it has none).
pub fn stored(node: &Node) -> Result<&str, Error> {
    match node.config.get(CONFIG) {
        None => Ok(EMPTY),
        Some(Value::Text(json)) => Ok(json),
        Some(_) => Err(invalid("the node's mask can't be read")),
    }
}

fn toggle(name: &str, label: &str, description: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some(description.into()),
        control: ParamControl::Toggle,
        required: true,
        default: Some(Value::Bool(false)),
        visible_if: None,
    }
}

pub struct MaskKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> MaskKind {
    MaskKind {
        descriptor: NodeDescriptor {
            id: ID.into(),
            label: "Mask".into(),
            category: Category::Action,
            inputs: vec![Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            outputs: vec![Port { name: "mask".into(), port_type: PortType { modality: Modality::Image, many: false } }],
            params: ParamSchema {
                params: vec![
                    toggle("transparent", "Include transparent areas", "Mask the image's transparent pixels too, such as an expanded canvas's new margins."),
                    toggle("invert", "Invert", "Paint what stays instead of what changes."),
                ],
            },
            quick_add: None,
        },
    }
}

impl NodeKind for MaskKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        (inputs.len() == 1 && inputs.iter().all(|t| t.modality == Modality::Image)).then(|| ModeId::new("action").ok()).flatten()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut params = self.validate_instance(ctx.node, true)?.into_visible_values();
        params.insert(SPEC_PARAM.into(), Value::Text(stored(ctx.node)?.into()));
        let input = ctx.inputs.iter().find(|i| i.port == "image").map(|i| i.value.clone()).ok_or_else(|| invalid("connect an image first"))?;
        Ok(vec![JobSpec::Action { node: ctx.node.id, action: ID.into(), params, inputs: vec![input], output: "mask".into() }])
    }
}
