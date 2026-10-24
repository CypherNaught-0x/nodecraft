//! The Element kind (#104): a reusable subject (a person, a product, a style…) made of up to
//! [`MAX_IMAGES`] ordered reference images, a category and optional guidelines, under a name a
//! prompt can mention as `#name`.
//!
//! An Element passes images through: its `reference` output is its generated reference sheet
//! when one is wired into `sheet` (the sheet card's history and active result are the Element's
//! reference history and default), otherwise its images in order. Mentions are resolved by the
//! planner (`nodecraft_exec::mentions`) into feeds from that output.
use crate::{Category, JobSpec, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Choice, Error, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Value};

/// The kind's id.
pub const ID: &str = "element";
/// The ordered reference images.
pub const IMAGES: &str = "images";
/// The generated reference sheet, when there is one.
pub const SHEET: &str = "sheet";
/// What the Element gives: the sheet, or the images.
pub const REFERENCE: &str = "reference";
/// The parameters.
pub const NAME: &str = "name";
pub const CATEGORY: &str = "category";
pub const GUIDELINES: &str = "guidelines";
/// Most reference images an Element holds.
pub const MAX_IMAGES: usize = 8;
/// Longest name, in bytes.
pub const MAX_NAME: usize = 40;
/// Most distinct names one prompt mentions; later ones stay plain text.
pub const MAX_MENTIONS: usize = 64;
/// Longest guidelines, in bytes.
pub const MAX_GUIDELINES: usize = 4096;

/// The categories, as stored and as shown.
pub const CATEGORIES: [(&str, &str); 9] = [
    ("general", "General"),
    ("person", "Person"),
    ("character", "Character"),
    ("product", "Product"),
    ("object", "Object"),
    ("material", "Material"),
    ("setting", "Setting"),
    ("style", "Style"),
    ("moodboard", "Moodboard"),
];

/// Whether `c` can be part of a name.
pub fn name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Whether `name` is a valid Element name: 1 to [`MAX_NAME`] letters, digits, `_` or `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_NAME && name.chars().all(name_char)
}

/// The name `node` goes by, when it is an Element with a valid one.
pub fn name(node: &Node) -> Option<&str> {
    match node.params.get(NAME) {
        Some(Value::Text(name)) if node.kind == ID && valid_name(name) => Some(name),
        _ => None,
    }
}

/// The names `text` mentions as `#name`, in order, each once (compared ignoring case), at most
/// [`MAX_MENTIONS`]. A `#` right after a letter or digit (`a#b`) is not a mention, and a mention
/// ends at the first character a name can't hold. Tokens too long to be a name are skipped.
pub fn mentions(text: &str) -> Vec<&str> {
    let mut found: Vec<&str> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut prev: Option<char> = None;
    for (at, c) in text.char_indices() {
        let starts = c == '#' && !prev.is_some_and(name_char);
        prev = Some(c);
        if !starts {
            continue;
        }
        let rest = text.get(at + 1..).unwrap_or_default();
        let len = rest.find(|c: char| !name_char(c)).unwrap_or(rest.len());
        let token = rest.get(..len).unwrap_or_default();
        if valid_name(token) && seen.insert(token.to_ascii_lowercase()) {
            found.push(token);
            if found.len() >= MAX_MENTIONS {
                break;
            }
        }
    }
    found
}

fn images(many: bool) -> PortType {
    PortType { modality: Modality::Image, many }
}

pub struct ElementKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> ElementKind {
    let text = |name: &str, label: &str, description: &str, multiline: bool, max_bytes: usize, default: Option<&str>| ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some(description.into()),
        control: ParamControl::Text { multiline, max_bytes },
        required: default.is_some(),
        default: default.map(|text| Value::Text(text.into())),
        visible_if: None,
    };
    ElementKind {
        descriptor: NodeDescriptor {
            id: ID.into(),
            label: "Element".into(),
            category: Category::Action,
            inputs: vec![Port { name: IMAGES.into(), port_type: images(true) }, Port { name: SHEET.into(), port_type: images(true) }],
            outputs: vec![Port { name: REFERENCE.into(), port_type: images(true) }],
            params: ParamSchema {
                params: vec![
                    text(NAME, "Name", "What prompts call it: #name. Letters, digits, - and _.", false, MAX_NAME, Some("element")),
                    ParamSpec {
                        name: CATEGORY.into(),
                        label: "Category".into(),
                        description: Some("What the images show; it shapes the reference sheet.".into()),
                        control: ParamControl::Choice {
                            options: CATEGORIES.iter().map(|(value, label)| Choice { value: (*value).into(), label: (*label).into() }).collect(),
                        },
                        required: true,
                        default: Some(Value::Text("general".into())),
                        visible_if: None,
                    },
                    text(GUIDELINES, "Guidelines", "What to keep the same wherever it appears.", true, MAX_GUIDELINES, None),
                ],
            },
            quick_add: None,
        },
    }
}

impl NodeKind for ElementKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        let mut contract = NodeContract::derive(descriptor);
        for input in &mut contract.inputs {
            if input.port == IMAGES {
                input.max = Some(MAX_IMAGES as u16);
            }
        }
        contract
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.iter().all(|t| t.modality == Modality::Image).then(|| ModeId::new("element").ok()).flatten()
    }
    /// One source job per reference: the sheet's images when it has any, else the reference
    /// images, in order.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let params = self.validate_instance(ctx.node, true)?.into_visible_values();
        if !matches!(params.get(NAME), Some(Value::Text(name)) if valid_name(name)) {
            return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
        }
        let values = |port: &str| -> Vec<Value> {
            ctx.inputs
                .iter()
                .filter(|input| input.port == port)
                .flat_map(|input| match &input.value {
                    Value::List(items) => items.clone(),
                    value => vec![value.clone()],
                })
                .collect()
        };
        let images = values(IMAGES);
        if images.len() > MAX_IMAGES {
            return Err(invalid("an Element holds at most 8 reference images"));
        }
        let sheet = values(SHEET);
        let chosen = if sheet.is_empty() { images } else { sheet };
        if chosen.is_empty() {
            return Err(invalid("this Element has no reference images"));
        }
        Ok(chosen.into_iter().map(|value| JobSpec::Source { node: ctx.node.id, output: REFERENCE.into(), value }).collect())
    }
}

#[cfg(test)]
mod tests;
