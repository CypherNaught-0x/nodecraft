//! `/object_info`: the node classes a server has, with their inputs and outputs in order.
//!
//! Input order matters: the UI workflow format stores widget values by position, so converting it
//! needs the order the server declares (`input_order` when present, otherwise the JSON order).

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

use crate::ComfyError;
use crate::ordered::OValue;

/// Cap on an `/object_info` reply. Large installs with many custom nodes send tens of MB.
pub const MAX_OBJECT_INFO_BYTES: usize = 128 * 1024 * 1024;

/// A kind of media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Media {
    Image,
    Video,
    Audio,
    Mesh,
}

/// What kind of value an input takes.
#[derive(Debug, Clone, PartialEq)]
pub enum InputKind {
    /// Whole numbers; `i128` because seeds range over all of `u64`.
    Int {
        default: Option<i128>,
        min: Option<i128>,
        max: Option<i128>,
        step: Option<i128>,
    },
    Float {
        default: Option<f64>,
        min: Option<f64>,
        max: Option<f64>,
        step: Option<f64>,
    },
    String {
        default: Option<String>,
        multiline: bool,
    },
    Boolean {
        default: Option<bool>,
    },
    /// One of a list of strings: samplers, schedulers, model files, input files. `upload` is set
    /// for loaders whose file is uploaded first (`LoadImage`, `LoadVideo`, `LoadAudio`).
    Combo {
        options: Vec<String>,
        default: Option<String>,
        upload: Option<Media>,
    },
    /// Connection-only input of a ComfyUI type such as `MODEL`, `LATENT` or `IMAGE`.
    Link(String),
}

/// One input of a node class.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSpec {
    pub name: String,
    pub required: bool,
    pub kind: InputKind,
    /// The UI adds a "control after generate" widget after this input (seeds), whose value is
    /// stored in UI workflows but not sent to the server.
    pub control_after_generate: bool,
    pub tooltip: Option<String>,
}

impl InputSpec {
    /// True for inputs edited as a widget (stored in `widgets_values` in UI workflows).
    pub fn is_widget(&self) -> bool {
        !matches!(self.kind, InputKind::Link(_))
    }

    /// Checks `value` fits this input; returns a reason when it doesn't.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        crate::json_bounds::Budget::default().value(value, 0).map_err(|error| error.to_string())?;
        let kind = crate::json_bounds::kind(value);
        match &self.kind {
            InputKind::Int { min, max, .. } => {
                let n = whole_number(value).ok_or_else(|| format!("expected a whole number, got {kind}"))?;
                in_range(n, *min, *max)
            }
            InputKind::Float { min, max, .. } => {
                let n = value.as_f64().filter(|n| n.is_finite()).ok_or_else(|| format!("expected a number, got {kind}"))?;
                in_range(n, *min, *max)
            }
            InputKind::String { .. } => value.is_string().then_some(()).ok_or_else(|| format!("expected text, got {kind}")),
            InputKind::Boolean { .. } => value.is_boolean().then_some(()).ok_or_else(|| format!("expected true or false, got {kind}")),
            InputKind::Combo { options, upload, .. } => {
                let text = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => return Err(format!("expected one of the options, got {kind}")),
                };
                // Uploaded files are named by the upload reply, so they need not be listed yet; an
                // empty list means the options are fetched elsewhere (remote combos).
                if upload.is_some() || options.is_empty() || options.contains(&text) {
                    Ok(())
                } else {
                    Err("the selected option is not available on this server".into())
                }
            }
            InputKind::Link(_) => Err("this input takes a connection, not a value".into()),
        }
    }
}

/// A whole number from JSON: any integer up to `u64::MAX`, or a float without a fraction (API
/// JSON written from Python often has `1024.0`).
pub(crate) fn whole_number(v: &Value) -> Option<i128> {
    if let Some(n) = v.as_i64() {
        return Some(i128::from(n));
    }
    if let Some(n) = v.as_u64() {
        return Some(i128::from(n));
    }
    let f = v.as_f64()?;
    (f.is_finite() && f.fract() == 0.0 && f.abs() < 1.8e19).then_some(f as i128)
}

fn in_range<T: PartialOrd + std::fmt::Display + Copy>(n: T, min: Option<T>, max: Option<T>) -> Result<(), String> {
    if let Some(min) = min
        && n < min
    {
        return Err(format!("{n} is below the minimum {min}"));
    }
    if let Some(max) = max
        && n > max
    {
        return Err(format!("{n} is above the maximum {max}"));
    }
    Ok(())
}

/// One output of a node class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSpec {
    pub name: String,
    /// ComfyUI type such as `IMAGE` or `LATENT`.
    pub kind: String,
    pub is_list: bool,
}

/// A node class from `/object_info`.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeInfo {
    pub class_type: String,
    pub display_name: String,
    pub category: String,
    pub inputs: Vec<InputSpec>,
    pub outputs: Vec<OutputSpec>,
    /// Output nodes (savers, previews) are what a prompt executes towards.
    pub output_node: bool,
    /// A paid Comfy API node: running it costs credits.
    pub api_node: bool,
}

impl NodeInfo {
    pub fn input(&self, name: &str) -> Option<&InputSpec> {
        self.inputs.iter().find(|i| i.name == name)
    }
}

/// Every node class a server has, by class name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeCatalog {
    nodes: BTreeMap<String, NodeInfo>,
}

impl NodeCatalog {
    /// Parses an `/object_info` reply. Classes whose description can't be read are skipped (a
    /// broken custom node must not hide the rest); their names are returned as the second value.
    pub fn parse(bytes: &[u8]) -> Result<(Self, Vec<String>), ComfyError> {
        let root: OValue = crate::error::parse_json("/object_info", bytes, MAX_OBJECT_INFO_BYTES)?;
        let OValue::Object(entries) = root else {
            return Err(ComfyError::Malformed { what: "/object_info", message: "not a JSON object".into() });
        };
        let mut nodes = BTreeMap::new();
        let mut skipped = Vec::new();
        for (class, raw) in entries {
            match parse_node(&class, &raw) {
                Some(info) => {
                    nodes.insert(class, info);
                }
                None => skipped.push(class),
            }
        }
        Ok((Self { nodes }, skipped))
    }

    /// Adds (or replaces) node classes, e.g. from a second `/object_info` call.
    pub fn extend(&mut self, other: NodeCatalog) {
        self.nodes.extend(other.nodes);
    }

    pub fn get(&self, class_type: &str) -> Option<&NodeInfo> {
        self.nodes.get(class_type)
    }

    pub fn contains(&self, class_type: &str) -> bool {
        self.nodes.contains_key(class_type)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &NodeInfo> {
        self.nodes.values()
    }

    /// The choices of a combo input, e.g. `("CheckpointLoaderSimple", "ckpt_name")` lists the
    /// checkpoints installed on the server.
    pub fn options(&self, class_type: &str, input: &str) -> Option<&[String]> {
        match &self.get(class_type)?.input(input)?.kind {
            InputKind::Combo { options, .. } => Some(options),
            _ => None,
        }
    }
}

fn parse_node(class: &str, ordered: &OValue) -> Option<NodeInfo> {
    let raw = ordered.to_value();
    let obj = raw.as_object()?;
    let text = |key: &str| obj.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    let input = obj.get("input").and_then(Value::as_object);
    let order = obj.get("input_order").and_then(Value::as_object);
    let mut inputs = Vec::new();
    for (section, required) in [("required", true), ("optional", false)] {
        let Some(specs) = input.and_then(|i| i.get(section)).and_then(Value::as_object) else { continue };
        // `input_order` (newer servers) is authoritative; otherwise the document order.
        let names: Vec<String> = match order.and_then(|o| o.get(section)).and_then(Value::as_array) {
            Some(list) => list.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            None => ordered.get("input").and_then(|i| i.get(section)).map(OValue::keys).unwrap_or_default(),
        };
        // A malformed input_order may repeat one large spec thousands of times. Keep
        // the first occurrence before copying defaults/options, preserving tolerant parsing.
        let mut seen = HashSet::new();
        for name in names.iter().filter(|name| seen.insert(name.as_str())) {
            if let Some(spec) = specs.get(name).and_then(|s| parse_input(name, s, required)) {
                inputs.push(spec);
            }
        }
    }
    let types = obj.get("output").and_then(Value::as_array).cloned().unwrap_or_default();
    let names = obj.get("output_name").and_then(Value::as_array);
    let lists = obj.get("output_is_list").and_then(Value::as_array);
    let outputs = types
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let kind = match t {
                Value::String(s) => s.clone(),
                Value::Array(_) => "COMBO".into(),
                other => other.to_string(),
            };
            let name = names.and_then(|n| n.get(i)).and_then(Value::as_str).map_or_else(|| kind.clone(), str::to_owned);
            let is_list = lists.and_then(|l| l.get(i)).and_then(Value::as_bool).unwrap_or(false);
            OutputSpec { name, kind, is_list }
        })
        .collect();
    let display_name = obj.get("display_name").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(class).to_owned();
    Some(NodeInfo {
        class_type: class.to_owned(),
        display_name,
        category: text("category"),
        inputs,
        outputs,
        output_node: obj.get("output_node").and_then(Value::as_bool).unwrap_or(false),
        api_node: obj.get("api_node").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn parse_input(name: &str, spec: &Value, required: bool) -> Option<InputSpec> {
    let parts = spec.as_array()?;
    let ty = parts.first()?;
    let empty = serde_json::Map::new();
    let opts = parts.get(1).and_then(Value::as_object).unwrap_or(&empty);
    let i = |k: &str| opts.get(k).and_then(whole_number);
    let f = |k: &str| opts.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    let strings = |v: &Value| v.as_array().map(|a| a.iter().filter_map(scalar_text).collect::<Vec<_>>());
    let force_input = opts.get("forceInput").and_then(Value::as_bool).unwrap_or(false);
    let kind = match ty {
        Value::Array(options) => combo(strings(&Value::Array(options.clone())).unwrap_or_default(), opts),
        Value::String(t) if force_input => InputKind::Link(t.clone()),
        Value::String(t) => match t.as_str() {
            "INT" => InputKind::Int { default: i("default"), min: i("min"), max: i("max"), step: i("step") },
            "FLOAT" => InputKind::Float { default: f("default"), min: f("min"), max: f("max"), step: f("step") },
            "STRING" => InputKind::String {
                default: opts.get("default").and_then(Value::as_str).map(str::to_owned),
                multiline: opts.get("multiline").and_then(Value::as_bool).unwrap_or(false),
            },
            "BOOLEAN" => InputKind::Boolean { default: opts.get("default").and_then(Value::as_bool) },
            "COMBO" => combo(opts.get("options").and_then(strings).unwrap_or_default(), opts),
            other => InputKind::Link(other.to_owned()),
        },
        _ => return None,
    };
    Some(InputSpec {
        name: name.to_owned(),
        required,
        control_after_generate: opts.get("control_after_generate").and_then(Value::as_bool).unwrap_or(false),
        tooltip: opts.get("tooltip").and_then(Value::as_str).map(str::to_owned),
        kind,
    })
}

fn combo(options: Vec<String>, opts: &serde_json::Map<String, Value>) -> InputKind {
    InputKind::Combo {
        default: opts.get("default").and_then(scalar_text),
        upload: [("image_upload", Media::Image), ("video_upload", Media::Video), ("audio_upload", Media::Audio)]
            .into_iter()
            .find(|(flag, _)| opts.get(*flag).and_then(Value::as_bool).unwrap_or(false))
            .map(|(_, media)| media),
        options,
    }
}

/// Combo options are usually strings, sometimes numbers.
fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    #[test]
    fn parses_the_fixture_catalog() {
        let (catalog, skipped) = NodeCatalog::parse(fixtures::OBJECT_INFO.as_bytes()).unwrap();
        assert_eq!(skipped, vec!["BrokenCustomNode".to_string()]);
        let ks = catalog.get("KSampler").unwrap();
        let names: Vec<&str> = ks.inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["model", "seed", "steps", "cfg", "sampler_name", "scheduler", "positive", "negative", "latent_image", "denoise"]);
        assert!(ks.input("seed").unwrap().control_after_generate);
        assert_eq!(ks.input("steps").unwrap().kind, InputKind::Int { default: Some(20), min: Some(1), max: Some(10000), step: None });
        assert_eq!(ks.input("model").unwrap().kind, InputKind::Link("MODEL".into()));
        assert_eq!(ks.outputs, vec![OutputSpec { name: "LATENT".into(), kind: "LATENT".into(), is_list: false }]);
        assert!(!ks.output_node);
        assert!(catalog.get("SaveImage").unwrap().output_node);
        assert_eq!(catalog.options("CheckpointLoaderSimple", "ckpt_name").unwrap(), ["sd_xl_base_1.0.safetensors", "flux1-dev.safetensors"]);
        let load = catalog.get("LoadImage").unwrap().input("image").unwrap();
        assert!(matches!(load.kind, InputKind::Combo { upload: Some(Media::Image), .. }));
        let seed = ks.input("seed").unwrap();
        assert_eq!(seed.kind, InputKind::Int { default: Some(0), min: Some(0), max: Some(i128::from(u64::MAX)), step: None });
        assert!(seed.check(&serde_json::json!(u64::MAX)).is_ok(), "seeds use all of u64");
        assert!(ks.input("steps").unwrap().check(&serde_json::json!(30.0)).is_ok(), "integral floats are whole numbers");
        assert!(ks.input("steps").unwrap().check(&serde_json::json!(30.5)).is_err());
        assert_eq!(catalog.get("CLIPTextEncode").unwrap().input("text").unwrap().kind, InputKind::String { default: None, multiline: true });
    }

    #[test]
    fn input_order_wins_over_json_order() {
        let json = r#"{"N":{"input":{"required":{"b":["INT",{}],"a":["INT",{}]}},"input_order":{"required":["a","b"]},"output":[]}}"#;
        let (c, _) = NodeCatalog::parse(json.as_bytes()).unwrap();
        let names: Vec<&str> = c.get("N").unwrap().inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
        let json = r#"{"N":{"input":{"required":{"b":["INT",{}],"a":["INT",{}]}},"output":[]}}"#;
        let (c, _) = NodeCatalog::parse(json.as_bytes()).unwrap();
        let names: Vec<&str> = c.get("N").unwrap().inputs.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["b", "a"], "document order without input_order");
    }

    #[test]
    fn new_combo_format_and_force_input() {
        let json = r#"{"N":{"input":{"required":{"s":["COMBO",{"options":["x",1]}],"n":["INT",{"forceInput":true}]}},"output":["IMAGE"],"output_name":["out"],"output_is_list":[true]}}"#;
        let (c, _) = NodeCatalog::parse(json.as_bytes()).unwrap();
        let n = c.get("N").unwrap();
        assert_eq!(n.input("s").unwrap().kind, InputKind::Combo { options: vec!["x".into(), "1".into()], default: None, upload: None });
        assert_eq!(n.input("n").unwrap().kind, InputKind::Link("INT".into()));
        assert!(!n.input("n").unwrap().is_widget());
        assert_eq!(n.outputs, vec![OutputSpec { name: "out".into(), kind: "IMAGE".into(), is_list: true }]);
        assert_eq!(n.display_name, "N");
    }

    #[test]
    fn upload_flags_api_nodes_and_numeric_combos() {
        let json = r#"{"V":{"input":{"required":{"file":[["a.mp4"],{"video_upload":true}],"n":[[1,2,4],{}],"remote":["COMBO",{"options":[]}]}},"output":[],"api_node":true}}"#;
        let (c, _) = NodeCatalog::parse(json.as_bytes()).unwrap();
        let v = c.get("V").unwrap();
        assert!(v.api_node);
        assert!(matches!(v.input("file").unwrap().kind, InputKind::Combo { upload: Some(Media::Video), .. }));
        assert!(v.input("file").unwrap().check(&serde_json::json!("just_uploaded.mp4")).is_ok());
        assert!(v.input("n").unwrap().check(&serde_json::json!(2)).is_ok());
        assert!(v.input("n").unwrap().check(&serde_json::json!(3)).is_err());
        assert!(v.input("remote").unwrap().check(&serde_json::json!("anything")).is_ok());
    }

    #[test]
    fn checks_values_against_specs() {
        let (c, _) = NodeCatalog::parse(fixtures::OBJECT_INFO.as_bytes()).unwrap();
        let ks = c.get("KSampler").unwrap();
        let steps = ks.input("steps").unwrap();
        assert!(steps.check(&serde_json::json!(30)).is_ok());
        assert!(steps.check(&serde_json::json!(0)).is_err());
        assert!(steps.check(&serde_json::json!(2.5)).is_err());
        assert!(ks.input("cfg").unwrap().check(&serde_json::json!(7)).is_ok());
        assert!(ks.input("sampler_name").unwrap().check(&serde_json::json!("euler")).is_ok());
        assert!(ks.input("sampler_name").unwrap().check(&serde_json::json!("nope")).is_err());
        assert!(ks.input("model").unwrap().check(&serde_json::json!("x")).is_err());
        assert!(c.get("LoadImage").unwrap().input("image").unwrap().check(&serde_json::json!("uploaded.png")).is_ok());
    }

    #[test]
    fn hostile_replies_are_errors_not_panics() {
        for bad in ["", "[]", "{", "null", r#"{"N":5}"#, r#"{"N":{"input":{"required":{"a":5,"b":[],"c":[{}]}}}}"#] {
            let _ = NodeCatalog::parse(bad.as_bytes());
        }
        let (c, skipped) = NodeCatalog::parse(br#"{"N":5,"M":{"input":{"required":{"a":5,"b":[]}}}}"#).unwrap();
        assert_eq!(skipped, ["N"]);
        assert!(c.get("M").unwrap().inputs.is_empty());
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(NodeCatalog::parse(deep.as_bytes()).is_err());
    }
}
