//! The custom shader action kind (#90): an image action whose effect is WGSL the person (or a
//! model) wrote. The shader and its declared parameters live in the node's
//! `config["nodecraft.shader"]` as JSON (`{"source": ..., "params": [{name, label, min, max,
//! default}]}`); its sliders are generated from the declarations. The shader is validated where
//! it is set (`node.shader_set`) and again before it runs; this kind only describes it. A node
//! without a shader inverts its image, so a fresh one does something visible.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Error, Modality, Number, ParamControl, ParamSchema, ParamSpec, PortType, Value};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// The kind's id.
pub const ID: &str = "image.shader";
/// The config key holding the shader.
pub const CONFIG: &str = "nodecraft.shader";
/// The parameter the planned job carries the shader in (not a control).
pub const SPEC_PARAM: &str = "nodecraft.shader";

/// A declared parameter, as stored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredParam {
    pub name: String,
    pub label: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

/// A shader, as stored in the node's config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredShader {
    pub source: String,
    pub params: Vec<StoredParam>,
}

impl Default for StoredShader {
    fn default() -> Self {
        Self {
            source: "fn effect(color: vec4<f32>, uv: vec2<f32>) -> vec4<f32> {\n    return vec4<f32>(mix(color.rgb, vec3<f32>(1.0) - color.rgb, amount()), color.a);\n}\n".into(),
            params: vec![StoredParam { name: "amount".into(), label: "Amount".into(), min: 0.0, max: 1.0, default: 1.0 }],
        }
    }
}

/// The shader `node` carries (the default when it has none). `Err` when its config is damaged.
pub fn stored(node: &Node) -> Result<StoredShader, Error> {
    match node.config.get(CONFIG) {
        None => Ok(StoredShader::default()),
        Some(Value::Text(json)) if json.len() <= 64 * 1024 => serde_json::from_str(json).map_err(|_| invalid("the node's shader can't be read")),
        Some(_) => Err(invalid("the node's shader can't be read")),
    }
}

/// The config value storing `shader`.
pub fn config_value(shader: &StoredShader) -> Result<Value, Error> {
    serde_json::to_string(shader).map(Value::Text).map_err(|_| invalid("the shader can't be stored"))
}

fn descriptor(params: Vec<ParamSpec>) -> NodeDescriptor {
    NodeDescriptor {
        id: ID.into(),
        label: "Custom shader".into(),
        category: Category::Action,
        inputs: vec![Port { name: "image".into(), port_type: PortType { modality: Modality::Image, many: false } }],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: Modality::Image, many: false } }],
        params: ParamSchema { params },
        quick_add: None,
    }
}

fn sliders(shader: &StoredShader) -> Result<Vec<ParamSpec>, Error> {
    shader
        .params
        .iter()
        .map(|p| {
            let range = (p.max - p.min).abs();
            Ok(ParamSpec {
                name: p.name.clone(),
                label: p.label.clone(),
                description: None,
                control: ParamControl::Slider {
                    min: Some(Number::new(p.min)?),
                    max: Some(Number::new(p.max)?),
                    step: Number::new((range / 100.0).max(f64::MIN_POSITIVE))?,
                },
                required: true,
                default: Some(Value::Number(Number::new(p.default)?)),
                visible_if: None,
            })
        })
        .collect()
}

pub struct ShaderKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> Result<ShaderKind, Error> {
    Ok(ShaderKind { descriptor: descriptor(sliders(&StoredShader::default())?) })
}

impl NodeKind for ShaderKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        if node.config.contains_key(CONFIG) { Ok(Cow::Owned(descriptor(sliders(&stored(node)?)?))) } else { Ok(Cow::Borrowed(&self.descriptor)) }
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        (inputs.len() == 1 && inputs.iter().all(|t| t.modality == Modality::Image)).then(|| ModeId::new("action").ok()).flatten()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let mut params = self.validate_instance(ctx.node, true)?.into_visible_values();
        let shader = stored(ctx.node)?;
        params.insert(SPEC_PARAM.into(), config_value(&shader)?);
        let input = ctx.inputs.iter().find(|i| i.port == "image").map(|i| i.value.clone()).ok_or_else(|| invalid("connect an image first"))?;
        Ok(vec![JobSpec::Action { node: ctx.node.id, action: ID.into(), params, inputs: vec![input], output: "out".into() }])
    }
}
