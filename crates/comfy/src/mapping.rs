//! Finding what a Workflow node can expose: the inputs worth turning into ports and parameters
//! (images to load, prompts, seeds, steps, model pickers, …) and the outputs it saves.
//!
//! This only proposes; the user picks, names and orders the exposed inputs in the mapping editor,
//! and `nodes` turns the result into typed ports and a parameter schema.

use std::collections::HashSet;

use serde_json::Value;

pub use crate::object_info::Media;
use crate::object_info::{InputKind, NodeCatalog};
use crate::workflow::{ApiWorkflow, InputValue};

/// What an exposed input carries.
#[derive(Debug, Clone, PartialEq)]
pub enum ExposedKind {
    /// A file the node uploads before queueing (a `LoadImage`-style input).
    Media(Media),
    Text {
        multiline: bool,
    },
    Int {
        min: Option<i128>,
        max: Option<i128>,
        step: Option<i128>,
    },
    Float {
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    Bool,
    /// One of the server's options (samplers, model files).
    Choice {
        options: Vec<String>,
    },
}

/// Why an input is interesting; the mapping editor uses it to order and pre-select inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InputRole {
    MediaInput,
    Prompt,
    NegativePrompt,
    Seed,
    Steps,
    Cfg,
    Sampler,
    Scheduler,
    Denoise,
    Width,
    Height,
    BatchSize,
    Checkpoint,
    Lora,
    LoraStrength,
    Model,
    Other,
}

impl InputRole {
    /// Pre-selected in the mapping editor.
    pub fn exposed_by_default(self) -> bool {
        matches!(self, Self::MediaInput | Self::Prompt | Self::NegativePrompt | Self::Seed | Self::Steps | Self::Cfg | Self::Width | Self::Height)
    }
}

/// An input the user may expose.
#[derive(Debug, Clone, PartialEq)]
pub struct ExposedInput {
    pub node: String,
    pub input: String,
    pub label: String,
    pub role: InputRole,
    pub kind: ExposedKind,
    /// The value in the workflow as imported.
    pub current: Option<Value>,
    /// The server's default for the input.
    pub default: Option<Value>,
    pub required: bool,
    pub tooltip: Option<String>,
    /// The editor offers "control after generate" (fixed / randomize / …) for this input; the
    /// imported mode is in [`crate::Conversion::controls`].
    pub control_after_generate: bool,
}

/// An output the node produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposedOutput {
    pub node: String,
    pub label: String,
    /// `None` when the output node's media kind isn't known (custom savers). The bytes the
    /// server returns are sniffed and decide the port at run time; this is the expected kind.
    pub media: Option<Media>,
    /// A preview node: it writes temporary files, which a run doesn't collect, so it gives no
    /// result. Shown so the mapping editor can suggest a Save node instead.
    pub preview: bool,
}

/// A paid ComfyUI API (partner) node in the workflow: running it spends the server's comfy.org
/// credits, which NodeCraft can't price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartnerNode {
    pub node: String,
    pub class_type: String,
}

/// Inputs and outputs proposed for a workflow.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exposed {
    pub inputs: Vec<ExposedInput>,
    pub outputs: Vec<ExposedOutput>,
    /// Its partner nodes, in node order.
    pub partners: Vec<PartnerNode>,
}

/// Proposes inputs and outputs for `workflow`. Only literal inputs are proposed (connected ones
/// are internal to the workflow); node classes missing from `catalog` are skipped (check the
/// workflow first). Inputs come in node order, then the server's input order. Returns an error
/// before copying values or repeated catalog metadata beyond the shared JSON resource limits.
pub fn discover(workflow: &ApiWorkflow, catalog: &NodeCatalog) -> Result<Exposed, crate::ComfyError> {
    let mut budget = crate::json_bounds::Budget::default();
    workflow.validate_budget(&mut budget, 0)?;
    // Catalog data is bounded when parsed, but a workflow may reuse its metadata many times.
    // Charge every proposed copy before building the exposure list.
    for (id, node) in &workflow.nodes {
        let Some(info) = catalog.get(&node.class_type) else { continue };
        let label = node.title.as_deref().unwrap_or(&info.display_name);
        if info.api_node {
            budget.container(2, 0)?;
            budget.text(id, 1)?;
            budget.text(&node.class_type, 1)?;
        }
        for spec in info.inputs.iter().filter(|spec| spec.is_widget()) {
            if matches!(node.inputs.get(&spec.name), Some(InputValue::Link { .. })) {
                continue;
            }
            if let Some(InputValue::Value(value)) = node.inputs.get(&spec.name) {
                budget.value(value, 1)?;
            }
            budget.container(12, 0)?;
            for text in [id.as_str(), spec.name.as_str(), label, spec.name.as_str(), spec.tooltip.as_deref().unwrap_or("")] {
                budget.text(text, 1)?;
            }
            match &spec.kind {
                InputKind::Combo { options, default, upload } => {
                    // Upload combos expose Media, so their directory listing is not copied.
                    if upload.is_none() {
                        budget.container(options.len(), 1)?;
                        for option in options {
                            budget.text(option, 2)?;
                        }
                    }
                    if let Some(default) = default {
                        budget.text(default, 1)?;
                    }
                }
                InputKind::String { default: Some(default), .. } => budget.text(default, 1)?,
                _ => {}
            }
        }
        if info.output_node {
            budget.container(3, 0)?;
            budget.text(id, 1)?;
            budget.text(label, 1)?;
        }
    }
    let negative = negative_encoders(workflow);
    let mut out = Exposed::default();
    for id in workflow.ids() {
        let Some(node) = workflow.nodes.get(id) else { continue };
        let Some(info) = catalog.get(&node.class_type) else { continue };
        let node_label = node.title.clone().unwrap_or_else(|| info.display_name.clone());
        if info.api_node {
            out.partners.push(PartnerNode { node: id.into(), class_type: node.class_type.clone() });
        }
        if info.output_node {
            out.outputs.push(ExposedOutput {
                node: id.into(),
                label: node_label.clone(),
                media: output_media(&node.class_type),
                preview: node.class_type.to_ascii_lowercase().starts_with("preview"),
            });
        }
        for spec in info.inputs.iter().filter(|s| s.is_widget()) {
            let current = match node.inputs.get(&spec.name) {
                Some(InputValue::Link { .. }) => continue,
                Some(InputValue::Value(v)) => Some(v.clone()),
                None => None,
            };
            let media = match &spec.kind {
                InputKind::Combo { upload: Some(media), .. } => Some(*media),
                _ => None,
            };
            let kind = match (&spec.kind, media) {
                (_, Some(m)) => ExposedKind::Media(m),
                (InputKind::Int { min, max, step, .. }, None) => ExposedKind::Int { min: *min, max: *max, step: *step },
                (InputKind::Float { min, max, step, .. }, None) => ExposedKind::Float { min: *min, max: *max, step: *step },
                (InputKind::String { multiline, .. }, None) => ExposedKind::Text { multiline: *multiline },
                (InputKind::Boolean { .. }, None) => ExposedKind::Bool,
                (InputKind::Combo { options, .. }, None) => ExposedKind::Choice { options: options.clone() },
                (InputKind::Link(_), None) => continue,
            };
            let role = role_of(&spec.name, &kind, negative.contains(id));
            let label = match role {
                InputRole::Prompt if node.title.is_none() => "Prompt".to_owned(),
                InputRole::NegativePrompt if node.title.is_none() => "Negative prompt".to_owned(),
                InputRole::Prompt | InputRole::NegativePrompt => node_label.clone(),
                _ => format!("{node_label} · {}", spec.name),
            };
            let default = match &spec.kind {
                InputKind::Int { default, .. } => {
                    default.and_then(|d| i64::try_from(d).ok().map(Value::from).or_else(|| u64::try_from(d).ok().map(Value::from)))
                }
                InputKind::Float { default, .. } => default.and_then(|d| serde_json::Number::from_f64(d).map(Value::Number)),
                InputKind::String { default, .. } => default.clone().map(Value::String),
                InputKind::Boolean { default } => default.map(Value::Bool),
                InputKind::Combo { default, .. } => default.clone().map(Value::String),
                InputKind::Link(_) => None,
            };
            out.inputs.push(ExposedInput {
                node: id.into(),
                input: spec.name.clone(),
                label,
                role,
                kind,
                current,
                default,
                required: spec.required,
                tooltip: spec.tooltip.clone(),
                control_after_generate: spec.control_after_generate,
            });
        }
    }
    Ok(out)
}

/// Nodes whose output feeds an input named `negative` (the negative prompt's encoder).
fn negative_encoders(workflow: &ApiWorkflow) -> HashSet<String> {
    workflow
        .nodes
        .values()
        .flat_map(|n| n.inputs.iter())
        .filter(|(name, _)| name.as_str() == "negative")
        .filter_map(|(_, v)| match v {
            InputValue::Link { node, .. } => Some(node.clone()),
            InputValue::Value(_) => None,
        })
        .collect()
}

fn role_of(name: &str, kind: &ExposedKind, negative: bool) -> InputRole {
    use InputRole::*;
    match (name, kind) {
        (_, ExposedKind::Media(_)) => MediaInput,
        ("negative_prompt", ExposedKind::Text { .. }) => NegativePrompt,
        ("prompt" | "positive_prompt" | "text" | "caption" | "instruction" | "instructions", ExposedKind::Text { .. }) if negative => NegativePrompt,
        ("prompt" | "positive_prompt" | "text" | "caption" | "instruction" | "instructions", ExposedKind::Text { .. }) => Prompt,
        (_, ExposedKind::Text { multiline: true }) if negative => NegativePrompt,
        (_, ExposedKind::Text { multiline: true }) => Prompt,
        ("seed" | "noise_seed", _) => Seed,
        ("steps", _) => Steps,
        ("cfg", _) => Cfg,
        ("sampler_name", _) => Sampler,
        ("scheduler", _) => Scheduler,
        ("denoise", _) => Denoise,
        ("width", _) => Width,
        ("height", _) => Height,
        ("batch_size", _) => BatchSize,
        ("ckpt_name", _) => Checkpoint,
        ("lora_name", _) => Lora,
        ("strength_model" | "strength_clip", _) => LoraStrength,
        ("unet_name" | "vae_name" | "clip_name" | "control_net_name" | "model_name", _) => Model,
        _ => Other,
    }
}

fn output_media(class: &str) -> Option<Media> {
    let c = class.to_ascii_lowercase();
    // An animated WebP or PNG sniffs as an image, and lands on an image port.
    if c.contains("animated") {
        Some(Media::Image)
    } else if c.contains("video") || c.contains("webm") {
        Some(Media::Video)
    } else if c.contains("audio") {
        Some(Media::Audio)
    } else if c.contains("glb") || c.contains("3d") || c.contains("mesh") {
        Some(Media::Mesh)
    } else if c.contains("image") {
        Some(Media::Image)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn setup() -> (ApiWorkflow, NodeCatalog) {
        let c = NodeCatalog::parse(fixtures::OBJECT_INFO.as_bytes()).unwrap().0;
        (ApiWorkflow::parse(fixtures::API_WORKFLOW.as_bytes(), None).unwrap(), c)
    }

    #[test]
    fn proposes_prompts_sampler_settings_and_models() {
        let (wf, c) = setup();
        let ex = discover(&wf, &c).unwrap();
        let find = |node: &str, input: &str| ex.inputs.iter().find(|i| i.node == node && i.input == input).unwrap();
        let pos = find("6", "text");
        assert_eq!((pos.role, pos.label.as_str()), (InputRole::Prompt, "Positive"));
        assert_eq!(pos.current, Some(serde_json::json!("a cat in a spacesuit")));
        let neg = find("7", "text");
        assert_eq!((neg.role, neg.label.as_str()), (InputRole::NegativePrompt, "Negative prompt"));
        assert_eq!(find("3", "seed").role, InputRole::Seed);
        assert!(find("3", "seed").control_after_generate);
        assert_eq!(find("3", "steps").default, Some(serde_json::json!(20)));
        assert!(find("3", "steps").required);
        assert_eq!(find("3", "steps").kind, ExposedKind::Int { min: Some(1), max: Some(10000), step: None });
        assert_eq!(find("3", "sampler_name").kind, ExposedKind::Choice { options: vec!["euler".into(), "euler_ancestral".into(), "dpmpp_2m".into()] });
        assert_eq!(find("4", "ckpt_name").role, InputRole::Checkpoint);
        assert_eq!(find("10", "lora_name").role, InputRole::Lora);
        assert_eq!(find("5", "width").label, "Empty Latent Image · width");
        assert!(ex.inputs.iter().all(|i| i.input != "model"), "connected inputs are internal");
        let defaults: Vec<&str> = ex.inputs.iter().filter(|i| i.role.exposed_by_default()).map(|i| i.label.as_str()).collect();
        assert_eq!(
            defaults,
            [
                "KSampler · seed",
                "KSampler · steps",
                "KSampler · cfg",
                "Empty Latent Image · width",
                "Empty Latent Image · height",
                "Positive",
                "Negative prompt"
            ]
        );
        assert_eq!(ex.outputs, vec![ExposedOutput { node: "9".into(), label: "Save Image".into(), media: Some(Media::Image), preview: false }]);
    }

    #[test]
    fn image_loaders_become_media_inputs() {
        let (_, c) = setup();
        let wf = ApiWorkflow::parse(br#"{"1":{"class_type":"LoadImage","inputs":{"image":"example.png"}},"2":{"class_type":"PreviewImage","inputs":{"images":["1",0]}},"3":{"class_type":"Unknown","inputs":{}}}"#, None).unwrap();
        let ex = discover(&wf, &c).unwrap();
        assert_eq!(ex.inputs.len(), 1);
        assert_eq!(ex.inputs[0].kind, ExposedKind::Media(Media::Image));
        assert_eq!(ex.inputs[0].role, InputRole::MediaInput);
        assert_eq!(ex.outputs[0].media, Some(Media::Image));
    }

    #[test]
    fn partner_nodes_are_listed_in_node_order() {
        let info = br#"{
          "PartnerImage": {"input": {"required": {"prompt": ["STRING", {}]}}, "output": ["IMAGE"], "api_node": true},
          "SaveImage": {"input": {"required": {"images": ["IMAGE"]}}, "output": [], "output_node": true}
        }"#;
        let c = NodeCatalog::parse(info).unwrap().0;
        let wf = ApiWorkflow::parse(
            br#"{"2":{"class_type":"SaveImage","inputs":{"images":["1",0]}},"1":{"class_type":"PartnerImage","inputs":{"prompt":"a"}}}"#,
            None,
        )
        .unwrap();
        let ex = discover(&wf, &c).unwrap();
        assert_eq!(ex.partners, [PartnerNode { node: "1".into(), class_type: "PartnerImage".into() }]);
        assert!(discover(&setup().0, &setup().1).unwrap().partners.is_empty());
    }

    #[test]
    fn video_and_audio_loaders_and_savers_are_mapped() {
        // Core loaders flag their upload; the custom video loader doesn't, and is known by name.
        let info = br#"{
          "LoadVideo": {"input": {"required": {"file": [["a.mp4"], {"video_upload": true}]}}, "output": ["VIDEO"], "output_node": false},
          "LoadAudio": {"input": {"required": {"audio": [["a.wav"], {"audio_upload": true}]}}, "output": ["AUDIO"], "output_node": false},
          "VHS_LoadVideo": {"input": {"required": {"video": [["b.mp4"]], "frame_load_cap": ["INT", {"default": 0}]}}, "output": ["IMAGE"], "output_node": false},
          "SaveVideo": {"input": {"required": {"video": ["VIDEO"], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
          "SaveAudio": {"input": {"required": {"audio": ["AUDIO"], "filename_prefix": ["STRING", {}]}}, "output": [], "output_node": true},
          "SaveAnimatedWEBP": {"input": {"required": {"images": ["IMAGE"]}}, "output": [], "output_node": true},
          "PreviewAudio": {"input": {"required": {"audio": ["AUDIO"]}}, "output": [], "output_node": true}
        }"#;
        let c = NodeCatalog::parse(info).unwrap().0;
        let wf = ApiWorkflow::parse(
            br#"{"1":{"class_type":"LoadVideo","inputs":{"file":"a.mp4"}},"2":{"class_type":"LoadAudio","inputs":{"audio":"a.wav"}},
                "3":{"class_type":"VHS_LoadVideo","inputs":{"video":"b.mp4","frame_load_cap":0}},
                "4":{"class_type":"SaveVideo","inputs":{"video":["1",0],"filename_prefix":"v"}},"5":{"class_type":"SaveAudio","inputs":{"audio":["2",0],"filename_prefix":"a"}},
                "6":{"class_type":"SaveAnimatedWEBP","inputs":{"images":["3",0]}},"7":{"class_type":"PreviewAudio","inputs":{"audio":["2",0]}}}"#,
            None,
        )
        .unwrap();
        let ex = discover(&wf, &c).unwrap();
        let media: Vec<_> = ex
            .inputs
            .iter()
            .filter_map(|i| match i.kind {
                ExposedKind::Media(m) => Some((i.node.as_str(), i.input.as_str(), m)),
                _ => None,
            })
            .collect();
        assert_eq!(media, [("1", "file", Media::Video), ("2", "audio", Media::Audio), ("3", "video", Media::Video)]);
        let outputs: Vec<_> = ex.outputs.iter().map(|o| (o.node.as_str(), o.media, o.preview)).collect();
        assert_eq!(
            outputs,
            [("4", Some(Media::Video), false), ("5", Some(Media::Audio), false), ("6", Some(Media::Image), false), ("7", Some(Media::Audio), true)]
        );
    }

    #[test]
    fn media_kinds_from_class_names() {
        assert_eq!(output_media("VHS_VideoCombine"), Some(Media::Video));
        assert_eq!(output_media("SaveAnimatedPNG"), Some(Media::Image), "an animated image sniffs as an image");
        assert_eq!(output_media("SaveAudio"), Some(Media::Audio));
        assert_eq!(output_media("SaveGLB"), Some(Media::Mesh));
        assert_eq!(output_media("ShowText"), None);
    }
}
