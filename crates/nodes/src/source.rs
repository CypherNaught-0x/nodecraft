use crate::{Category, JobSpec, ModeContract, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::Port;
use nodecraft_types::{Affects, Modality, ParamControl, ParamRule, ParamSchema, ParamSpec, PortType, Value};

/// A configured source kind. Its descriptor is immutable after construction.
pub struct SourceKind {
    descriptor: NodeDescriptor,
    parameter: &'static str,
}

impl SourceKind {
    pub(crate) fn new(id: &str, label: &str, shortcut: &str, modality: Modality) -> Self {
        let (parameter, control, default) = match modality {
            Modality::Text => ("text", ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 }, Some(Value::Text(String::new()))),
            _ => ("asset", ParamControl::Asset { modality }, None),
        };
        let mut source = Self {
            descriptor: NodeDescriptor {
                id: id.into(),
                label: label.into(),
                category: Category::Source,
                inputs: Vec::new(),
                outputs: vec![Port { name: "out".into(), port_type: PortType { modality, many: false } }],
                params: ParamSchema {
                    params: vec![ParamSpec {
                        name: parameter.into(),
                        label: label.into(),
                        description: None,
                        control,
                        required: true,
                        default,
                        visible_if: None,
                    }],
                },
                quick_add: Some(shortcut.into()),
            },
            parameter,
        };
        if modality == Modality::Mesh {
            source.descriptor.params.params.extend(crate::mesh::view::schema());
        }
        source
    }
}

impl NodeKind for SourceKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.is_empty().then(ModeId::source)
    }
    /// A source has no inputs and one mode; its value is the result, so it shapes the output.
    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        let mut contract = NodeContract::derive(descriptor);
        for output in &mut contract.outputs {
            output.role = "value".into();
        }
        contract.modes = vec![ModeContract { mode: ModeId::source(), inputs: Vec::new(), params: None }];
        contract.params.rules = descriptor
            .params
            .params
            .iter()
            .map(|spec| ParamRule { affects: Affects { shape: true, ..Affects::default() }, ..ParamRule::implied(&spec.name) })
            .collect();
        if descriptor.id == crate::mesh::SOURCE {
            crate::mesh::view::contract(&mut contract.params);
        }
        contract
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, nodecraft_types::Error> {
        if !ctx.inputs.is_empty() {
            return Err(invalid("source nodes do not accept connected inputs"));
        }
        let mut params = self.validate_instance(ctx.node, true)?.into_visible_values();
        let value = params.remove(self.parameter).ok_or_else(|| invalid("source value is missing"))?;
        Ok(vec![JobSpec::Source { node: ctx.node.id, output: "out".into(), value }])
    }
}
