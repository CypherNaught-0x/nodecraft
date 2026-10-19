use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_types::{Error, ParamControl, ParamSchema, ParamSpec, PortType, Value};

/// A document annotation, intentionally producing no executable output or provider work.
pub struct NoteKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> NoteKind {
    NoteKind {
        descriptor: NodeDescriptor {
            id: "structure.note".into(),
            label: "Note".into(),
            category: Category::Structure,
            inputs: Vec::new(),
            outputs: Vec::new(),
            quick_add: None,
            params: ParamSchema {
                params: vec![ParamSpec {
                    name: "text".into(),
                    label: "Text".into(),
                    description: Some("Plain text annotation; notes do not produce outputs or run jobs.".into()),
                    control: ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 },
                    required: false,
                    default: Some(Value::Text(String::new())),
                    visible_if: None,
                }],
            },
        },
    }
}

impl NodeKind for NoteKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.is_empty().then(ModeId::annotation)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        if !ctx.inputs.is_empty() {
            return Err(invalid("notes do not accept connected inputs"));
        }
        self.validate_instance(ctx.node, true)?;
        Ok(Vec::new())
    }
}
