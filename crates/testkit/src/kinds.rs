//! Generation node kinds that call the mock provider: `mock.text` (text in, text out) and
//! `mock.image` (text in, image out). Each takes a list of text inputs as its prompt, a `model`
//! (a catalog model id) and a free-text `style` parameter, and plans one model call.

use nodecraft_graph::Port;
use nodecraft_nodes::{
    Category, GenerateInput, GenerateOutput, JobSpec, ModeId, NodeDescriptor, NodeKind, ParamControl, ParamSchema, ParamSpec, PlanCtx, Registry,
};
use nodecraft_types::{Error, ErrorKind, Modality, PortType, Value};

/// The provider id the mock kinds call.
pub const PROVIDER: &str = "mock";

struct MockGenerator(NodeDescriptor, Modality);

impl NodeKind for MockGenerator {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.0
    }
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("generate").ok()
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let model = match ctx.node.params.get("model") {
            Some(Value::Text(model)) if !model.is_empty() => model.clone(),
            _ => return Err(Error::new(ErrorKind::InvalidInput, "choose a model")),
        };
        let catalog = crate::catalog::mock_catalog()?;
        let definition = catalog.get(&model).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "unknown mock model"))?;
        let params = definition.params.resolve(&nodecraft_exec::cost::model_params(ctx.node, definition), true)?.into_visible_values();
        let model = definition.provider_model.clone();
        let inputs = ctx.inputs.iter().map(|input| GenerateInput { role: "prompt".into(), value: input.value.clone() }).collect();
        let outputs = vec![GenerateOutput { port: "out".into(), modality: self.1, style_family: None }];
        Ok(vec![JobSpec::Generate {
            instance: ctx.targets.get(ctx.node.id, PROVIDER).and_then(|target| target.instance()),
            node: ctx.node.id,
            outputs,
            provider: PROVIDER.into(),
            model,
            params,
            inputs,
        }])
    }
}

fn descriptor(id: &str, label: &str, output: Modality, model: &str) -> NodeDescriptor {
    let text_list = PortType { modality: Modality::Text, many: true };
    let spec = |name: &str, control: ParamControl, default: &str| ParamSpec {
        name: name.into(),
        label: name.into(),
        description: None,
        control,
        required: false,
        default: Some(Value::Text(default.into())),
        visible_if: None,
    };
    NodeDescriptor {
        id: id.into(),
        label: label.into(),
        category: Category::Generation,
        inputs: vec![Port { name: "in".into(), port_type: text_list }],
        outputs: vec![Port { name: "out".into(), port_type: PortType { modality: output, many: false } }],
        params: ParamSchema {
            params: vec![spec("model", ParamControl::Model, model), spec("style", ParamControl::Text { multiline: false, max_bytes: 1024 }, "")],
        },
        quick_add: None,
    }
}

/// The built-in kinds plus `mock.text` and `mock.image`.
pub fn registry() -> Result<Registry, Error> {
    let mut kinds = Registry::builtins()?;
    kinds.register(Box::new(MockGenerator(descriptor("mock.text", "Mock text", Modality::Text, "mock-text"), Modality::Text)))?;
    kinds.register(Box::new(MockGenerator(descriptor("mock.image", "Mock image", Modality::Image, "mock-image"), Modality::Image)))?;
    Ok(kinds)
}
