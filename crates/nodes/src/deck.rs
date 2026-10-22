//! The Deck kind (#122): a structure card holding a presentation. Its slides live in the node's
//! `config["nodecraft.deck"]` as JSON (the format is `nodecraft-actions::deck`); the `deck.*`
//! commands check and change them. Like a Note it has no ports and never runs: a slide's source
//! layers name canvas nodes and show what they show when the deck is presented.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::Node;
use nodecraft_types::{Error, ParamControl, ParamSchema, ParamSpec, PortType, Unsigned, Value};

/// The kind's id.
pub const ID: &str = "structure.deck";
/// The config key holding the slides.
pub const CONFIG: &str = "nodecraft.deck";
/// No slides, as stored.
pub const EMPTY: &str = r#"{"version":1,"slides":[]}"#;
/// The frame's edge range, in pixels, and its default size.
pub const MIN_FRAME: u64 = 100;
pub const MAX_FRAME: u64 = 4096;
pub const DEFAULT_SIZE: (u64, u64) = (1920, 1080);

/// The deck `node` carries, as stored (no slides when it has none).
pub fn stored(node: &Node) -> Result<&str, Error> {
    match node.config.get(CONFIG) {
        None => Ok(EMPTY),
        Some(Value::Text(json)) => Ok(json),
        Some(_) => Err(invalid("the deck's slides can't be read")),
    }
}

fn edge(name: &str, label: &str, default: u64) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some("Slide size in pixels, 100 to 4096.".into()),
        control: ParamControl::Unsigned { min: Some(Unsigned(MIN_FRAME)), max: Some(Unsigned(MAX_FRAME)), step: Unsigned(1) },
        required: true,
        default: Some(Value::Unsigned(Unsigned(default))),
        visible_if: None,
    }
}

pub struct DeckKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> DeckKind {
    DeckKind {
        descriptor: NodeDescriptor {
            id: ID.into(),
            label: "Deck".into(),
            category: Category::Structure,
            inputs: Vec::new(),
            outputs: Vec::new(),
            quick_add: None,
            params: ParamSchema {
                params: vec![
                    edge("width", "Width", DEFAULT_SIZE.0),
                    edge("height", "Height", DEFAULT_SIZE.1),
                    ParamSpec {
                        name: "background".into(),
                        label: "Background".into(),
                        description: Some("Painted under every slide.".into()),
                        control: ParamControl::Color,
                        required: true,
                        default: Some(Value::Text("#ffffff".into())),
                        visible_if: None,
                    },
                ],
            },
        },
    }
}

impl NodeKind for DeckKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.is_empty().then(ModeId::annotation)
    }
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        if !ctx.inputs.is_empty() {
            return Err(invalid("a deck does not accept connected inputs"));
        }
        self.validate_instance(ctx.node, true)?;
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests;
