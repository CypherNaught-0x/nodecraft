//! Action node kinds (#410): deterministic, local operations over the image and text actions
//! (`nodecraft-actions`). Each kind declares its ports and parameter schema, so its card,
//! inspector and MCP schema come from data. Planning emits one [`JobSpec::Action`] naming the
//! action and carrying the visible parameter values and the connected inputs; the host runs it
//! locally (no provider, no cost, no queue).
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::Port;
use nodecraft_types::{Choice, Error, Modality, Number, ParamControl, ParamSchema, ParamSpec, PortType, Unsigned, Value, VisibleIf};

/// An action node kind.
pub struct ActionKind {
    descriptor: NodeDescriptor,
}

fn port(name: &str, modality: Modality, many: bool) -> Port {
    Port { name: name.into(), port_type: PortType { modality, many } }
}

fn number(value: f64) -> Option<Value> {
    Number::new(value).ok().map(Value::Number)
}

fn slider(name: &str, label: &str, min: f64, max: f64, step: f64, default: f64) -> Result<ParamSpec, Error> {
    Ok(ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Slider { min: Some(Number::new(min)?), max: Some(Number::new(max)?), step: Number::new(step)? },
        required: true,
        default: number(default),
        visible_if: None,
    })
}

fn choice(name: &str, label: &str, options: &[(&str, &str)], default: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Choice { options: options.iter().map(|(v, l)| Choice { value: (*v).into(), label: (*l).into() }).collect() },
        required: true,
        default: Some(Value::Text(default.into())),
        visible_if: None,
    }
}

fn toggle(name: &str, label: &str, default: bool) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Toggle,
        required: true,
        default: Some(Value::Bool(default)),
        visible_if: None,
    }
}

fn text(name: &str, label: &str, default: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Text { multiline: false, max_bytes: 8192 },
        required: false,
        default: Some(Value::Text(default.into())),
        visible_if: None,
    }
}

fn color(name: &str, label: &str, default: &str) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Color,
        required: true,
        default: Some(Value::Text(default.into())),
        visible_if: None,
    }
}

fn count(name: &str, label: &str, min: u64, max: u64, default: u64) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: None,
        control: ParamControl::Unsigned { min: Some(Unsigned(min)), max: Some(Unsigned(max)), step: Unsigned(1) },
        required: true,
        default: Some(Value::Unsigned(Unsigned(default))),
        visible_if: None,
    }
}

fn when(mut spec: ParamSpec, parameter: &str, equals: &str) -> ParamSpec {
    spec.visible_if = Some(VisibleIf { parameter: parameter.into(), equals: Value::Text(equals.into()) });
    spec
}

const RESAMPLING: &[(&str, &str)] = &[("bilinear", "Smooth"), ("nearest", "Nearest"), ("area", "Area")];
/// Rotation resamples point by point: area averaging doesn't apply.
const ROTATE_RESAMPLING: &[(&str, &str)] = &[("bilinear", "Smooth"), ("nearest", "Nearest")];

fn image(id: &str, label: &str, params: Vec<ParamSpec>) -> ActionKind {
    kind(id, label, vec![port("image", Modality::Image, false)], vec![port("out", Modality::Image, false)], params)
}

fn kind(id: &str, label: &str, inputs: Vec<Port>, outputs: Vec<Port>, params: Vec<ParamSpec>) -> ActionKind {
    ActionKind {
        descriptor: NodeDescriptor {
            id: id.into(),
            label: label.into(),
            category: Category::Action,
            inputs,
            outputs,
            params: ParamSchema { params },
            quick_add: None,
        },
    }
}

/// Every action kind.
pub fn kinds() -> Result<Vec<ActionKind>, Error> {
    Ok(vec![
        image("image.blur", "Blur", vec![slider("sigma", "Strength", 0.0, 64.0, 0.5, 2.0)?]),
        image(
            "image.grade",
            "Color grade",
            vec![
                slider("brightness", "Brightness", -1.0, 1.0, 0.01, 0.0)?,
                slider("contrast", "Contrast", 0.0, 4.0, 0.01, 1.0)?,
                slider("saturation", "Saturation", 0.0, 4.0, 0.01, 1.0)?,
                slider("warmth", "Warmth", -1.0, 1.0, 0.01, 0.0)?,
            ],
        ),
        image(
            "image.filter",
            "Filter",
            vec![
                choice("preset", "Look", &[("grayscale", "Grayscale"), ("sepia", "Sepia"), ("noir", "Noir")], "grayscale"),
                slider("intensity", "Intensity", 0.0, 1.0, 0.01, 1.0)?,
            ],
        ),
        image(
            "image.tint",
            "Tint",
            vec![
                color("color", "Color", "#ff8800"),
                slider("intensity", "Intensity", 0.0, 1.0, 0.01, 0.5)?,
                choice(
                    "blend",
                    "Blend",
                    &[
                        ("normal", "Normal"),
                        ("multiply", "Multiply"),
                        ("screen", "Screen"),
                        ("overlay", "Overlay"),
                        ("darken", "Darken"),
                        ("lighten", "Lighten"),
                        ("difference", "Difference"),
                    ],
                    "normal",
                ),
            ],
        ),
        image("image.flip", "Flip", vec![choice("axis", "Axis", &[("horizontal", "Horizontal"), ("vertical", "Vertical"), ("both", "Both")], "horizontal")]),
        image(
            "image.rotate",
            "Rotate",
            vec![
                slider("angle", "Angle", -360.0, 360.0, 1.0, 90.0)?,
                toggle("expand", "Fit the whole image", true),
                choice("resampling", "Resampling", ROTATE_RESAMPLING, "bilinear"),
            ],
        ),
        image(
            "image.crop",
            "Crop",
            vec![
                slider("x", "Left", 0.0, 0.99, 0.01, 0.0)?,
                slider("y", "Top", 0.0, 0.99, 0.01, 0.0)?,
                slider("width", "Width", 0.01, 1.0, 0.01, 1.0)?,
                slider("height", "Height", 0.01, 1.0, 0.01, 1.0)?,
            ],
        ),
        image(
            "image.key",
            "Remove color",
            vec![
                color("target", "Color", "#00ff00"),
                slider("tolerance", "Tolerance", 0.0, 1.0, 0.01, 0.1)?,
                slider("softness", "Softness", 0.0, 1.0, 0.01, 0.1)?,
                choice("mode", "Keep", &[("remove_matched", "Everything else"), ("keep_matched", "Only this color")], "remove_matched"),
            ],
        ),
        image(
            "image.replace_color",
            "Replace color",
            vec![
                color("target", "Color", "#00ff00"),
                slider("tolerance", "Tolerance", 0.0, 1.0, 0.01, 0.1)?,
                slider("softness", "Softness", 0.0, 1.0, 0.01, 0.1)?,
                color("replacement", "With", "#000000"),
                toggle("replace_alpha", "Replace transparency too", false),
            ],
        ),
        image(
            "image.aspect",
            "Aspect ratio",
            vec![
                choice(
                    "ratio",
                    "Ratio",
                    &[("square", "1:1"), ("landscape4_3", "4:3"), ("portrait3_4", "3:4"), ("landscape16_9", "16:9"), ("portrait9_16", "9:16")],
                    "square",
                ),
                choice("mode", "Fit", &[("crop", "Crop"), ("pad", "Pad")], "crop"),
                when(color("padding", "Padding", "#000000"), "mode", "pad"),
                slider("x", "Horizontal anchor", 0.0, 1.0, 0.01, 0.5)?,
                slider("y", "Vertical anchor", 0.0, 1.0, 0.01, 0.5)?,
            ],
        ),
        kind(
            "image.overlay",
            "Overlay",
            vec![port("image", Modality::Image, false), port("overlay", Modality::Image, false)],
            vec![port("out", Modality::Image, false)],
            vec![
                choice(
                    "anchor",
                    "Position",
                    &[
                        ("center", "Center"),
                        ("top_left", "Top left"),
                        ("top", "Top"),
                        ("top_right", "Top right"),
                        ("left", "Left"),
                        ("right", "Right"),
                        ("bottom_left", "Bottom left"),
                        ("bottom", "Bottom"),
                        ("bottom_right", "Bottom right"),
                    ],
                    "bottom_right",
                ),
                slider("size", "Size", 0.02, 1.0, 0.01, 0.25)?,
                slider("opacity", "Opacity", 0.0, 1.0, 0.01, 1.0)?,
            ],
        ),
        kind(
            "image.duplicate",
            "Duplicate",
            vec![port("image", Modality::Image, false)],
            vec![port("out", Modality::Image, true)],
            vec![count("count", "Copies", 1, 64, 4)],
        ),
        image("image.resize", "Resize", vec![slider("factor", "Scale", 0.02, 8.0, 0.01, 0.5)?, choice("resampling", "Resampling", RESAMPLING, "bilinear")]),
        kind(
            "text.split",
            "Split text",
            vec![port("text", Modality::Text, false)],
            vec![port("out", Modality::Text, true)],
            vec![
                choice(
                    "mode",
                    "Split by",
                    &[("separator", "Separator"), ("lines", "Lines"), ("paragraphs", "Paragraphs"), ("chars", "Characters"), ("regex", "Pattern")],
                    "separator",
                ),
                when(text("separator", "Separator", ","), "mode", "separator"),
                when(text("pattern", "Pattern", "\\s+"), "mode", "regex"),
                when(count("lines", "Lines per part", 1, 100_000, 1), "mode", "lines"),
                when(count("chars", "Characters per part", 1, 1_000_000, 1000), "mode", "chars"),
                count("max_parts", "Most parts", 1, 16_384, 1000),
            ],
        ),
        kind(
            "text.replace",
            "Replace text",
            vec![port("text", Modality::Text, false)],
            vec![port("out", Modality::Text, false)],
            vec![
                text("find", "Find", ""),
                toggle("regex", "Pattern", false),
                text("replacement", "Replace with", ""),
                toggle("case_sensitive", "Match case", true),
                toggle("whole_word", "Whole words", false),
            ],
        ),
        kind(
            "text.template",
            "Template",
            vec![port("values", Modality::Text, true)],
            vec![port("out", Modality::Text, false)],
            vec![ParamSpec {
                name: "template".into(),
                label: "Template".into(),
                description: Some("{{1}}, {{2}}, … insert the connected texts in order.".into()),
                control: ParamControl::Text { multiline: true, max_bytes: 64 * 1024 },
                required: false,
                default: Some(Value::Text("{{1}}".into())),
                visible_if: None,
            }],
        ),
        kind(
            "text.concat",
            "Join text",
            vec![port("texts", Modality::Text, true)],
            vec![port("out", Modality::Text, false)],
            vec![
                text("separator", "Separator", "\n"),
                text("prefix", "Prefix", ""),
                text("suffix", "Suffix", ""),
                toggle("trim", "Trim each", false),
                toggle("skip_empty", "Skip empty", true),
            ],
        ),
    ])
}

impl NodeKind for ActionKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }

    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        let wanted = self.descriptor.inputs.first().map(|p| p.port_type.modality)?;
        let fits = !inputs.is_empty() && inputs.len() <= self.descriptor.inputs.len() && inputs.iter().all(|t| t.modality == wanted);
        fits.then(|| ModeId::new("action").ok()).flatten()
    }

    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        let params = self.validate_instance(ctx.node, true)?.into_visible_values();
        // One value per input port, in the order the kind declares them (a list port's values
        // all go in, in order); every port must be connected.
        let mut inputs: Vec<Value> = Vec::new();
        for port in &self.descriptor.inputs {
            let mut values = ctx.inputs.iter().filter(|i| i.port == port.name).map(|i| i.value.clone());
            if port.port_type.many {
                let all: Vec<Value> = values.collect();
                if all.is_empty() {
                    return Err(invalid("connect an input first"));
                }
                inputs.extend(all);
            } else {
                inputs.push(values.next().ok_or_else(|| invalid("connect every input first"))?);
            }
        }
        let output = self.descriptor.outputs.first().map(|p| p.name.clone()).unwrap_or_default();
        Ok(vec![JobSpec::Action { node: ctx.node.id, action: self.descriptor.id.clone(), params, inputs, output }])
    }
}
