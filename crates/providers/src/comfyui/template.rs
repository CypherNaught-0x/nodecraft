//! Built-in workflows for the catalog's ComfyUI models (#9).
//!
//! A catalog card names a template in `provider_model` (`flux-dev`, `sdxl-base`) and sends a prompt,
//! an optional image and the card's parameters, not a workflow. [`expand`] builds the API-format
//! workflow from core ComfyUI nodes only (no custom nodes) and returns the job the Workflow node
//! would have sent: `params.workflow` plus media inputs targeting `<node>.<input>`. The adapter
//! then checks, uploads and queues it exactly like an imported workflow.
//!
//! The model file is a checkpoint the user installs; when the server lists its checkpoints and the
//! chosen one isn't there, the run is refused with the file to install, before anything uploads.

use serde_json::{Map, Value, json};

use nodecraft_comfy::{InputKind, NodeCatalog};

use crate::provider::{JobInput, ProviderJob};
use crate::{ErrorKind, ProviderError};

pub(crate) mod mesh;

/// The prefix of saved outputs in the server's `output` folder.
const SAVE_PREFIX: &str = "NodeCraft";
/// How much of an input image image-to-image keeps changing (1 would ignore the image).
const IMAGE_DENOISE: f64 = 0.75;
/// Input images are scaled to about this many megapixels before encoding, so a large photo can't
/// exhaust the GPU's memory.
const IMAGE_MEGAPIXELS: f64 = 1.0;
/// The name the input image has in the workflow until the adapter uploads it and sets the real one.
const IMAGE_PLACEHOLDER: &str = "nodecraft-input.png";

/// The node ids of the built workflow.
const CHECKPOINT: &str = "1";
const POSITIVE: &str = "2";
const GUIDANCE: &str = "3";
const NEGATIVE: &str = "4";
const LOAD_IMAGE: &str = "5";
const SCALE_IMAGE: &str = "6";
const LATENT: &str = "7";
const SAMPLER: &str = "8";
const DECODE: &str = "9";
const SAVE: &str = "10";

struct Template {
    /// The checkpoint used when the card doesn't choose one.
    checkpoint: &'static str,
    /// FLUX reads its guidance from the conditioning and samples at CFG 1.
    flux: bool,
    steps: u64,
    cfg: f64,
    sampler: &'static str,
    scheduler: &'static str,
    empty_latent: &'static str,
}

fn template(name: &str) -> Option<Template> {
    match name {
        "flux-dev" => Some(Template {
            checkpoint: "flux1-dev-fp8.safetensors",
            flux: true,
            steps: 28,
            cfg: 1.0,
            sampler: "euler",
            scheduler: "simple",
            empty_latent: "EmptySD3LatentImage",
        }),
        "sdxl-base" => Some(Template {
            checkpoint: "sd_xl_base_1.0.safetensors",
            flux: false,
            steps: 30,
            cfg: 7.0,
            sampler: "euler",
            scheduler: "normal",
            empty_latent: "EmptyLatentImage",
        }),
        _ => None,
    }
}

fn refused(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}

/// Width and height for an aspect ratio, about one megapixel and multiples of 64.
fn size(aspect: Option<&str>) -> Result<(u32, u32), ProviderError> {
    match aspect.unwrap_or("1:1") {
        "1:1" => Ok((1024, 1024)),
        "4:3" => Ok((1152, 896)),
        "3:4" => Ok((896, 1152)),
        "16:9" => Ok((1344, 768)),
        "9:16" => Ok((768, 1344)),
        other => Err(refused(format!("the aspect ratio {other:?} isn't one this workflow offers"))),
    }
}

/// A whole-number param: absent is `None`; anything but a non-negative integer is refused, so a
/// pinned seed is never swapped for another.
fn whole(job: &ProviderJob, name: &str) -> Result<Option<u64>, ProviderError> {
    match job.params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| refused(format!("`{name}` must be a whole number of 0 or more"))),
    }
}

/// The card's seed, or one derived from the job so a retried submit builds the same workflow.
fn seed(job: &ProviderJob) -> Result<u64, ProviderError> {
    if let Some(seed) = whole(job, "seed")? {
        return Ok(seed);
    }
    let hash = nodecraft_media::ContentHash::of(job.idempotency_key.as_bytes()).0;
    let mut bytes = [0u8; 8];
    for (slot, byte) in bytes.iter_mut().zip(hash.iter()) {
        *slot = *byte;
    }
    // ComfyUI's seeds are unsigned 64-bit, but JSON readers that go through doubles lose the top
    // bits: keep it within 2^53.
    Ok(u64::from_le_bytes(bytes) & ((1u64 << 53) - 1))
}

/// The job input carrying a workflow card's API workflow as JSON text (#9).
pub(crate) const CARD_WORKFLOW: &str = "comfy.workflow";
/// The job input carrying a workflow card's values, as JSON text of `{"<node>.<input>": value}`.
pub(crate) const CARD_VALUES: &str = "comfy.values";
/// The job input naming a workflow card's output nodes, as JSON text of `["<node>", …]`.
pub(crate) const CARD_OUTPUTS: &str = "comfy.outputs";

/// The text of the job's `role` input, if it has one.
pub(crate) fn card_text<'a>(job: &'a ProviderJob, role: &str) -> Option<&'a str> {
    job.inputs.iter().find_map(|input| match input {
        JobInput::Text { role: r, text } if r == role => Some(text.as_str()),
        _ => None,
    })
}

/// Whether the job runs a built-in workflow rather than one it carries (in `params.workflow`,
/// or a workflow card's text input).
pub(crate) fn is_templated(job: &ProviderJob) -> bool {
    !job.params.contains_key("workflow") && card_text(job, CARD_WORKFLOW).is_none()
}

/// The job with the workflow of its catalog model: `params.workflow` set and its media inputs
/// targeting the workflow's loader. `catalog` is the server's `/object_info`, used to name a
/// missing checkpoint and to fill inputs newer servers require with their defaults. A missing
/// checkpoint is marked `true`, like the adapter's other errors a fresher catalog could fix.
pub(crate) fn expand(job: &ProviderJob, catalog: &NodeCatalog) -> Result<ProviderJob, (ProviderError, bool)> {
    if mesh::supports(&job.model) {
        return mesh::expand(job, catalog);
    }
    let plain = |e: ProviderError| (e, false);
    let t = template(&job.model)
        .ok_or_else(|| refused(format!("the ComfyUI model {:?} has no built-in workflow; import its workflow into a Workflow node", job.model)))
        .map_err(plain)?;
    let mut prompt = None;
    let mut image = None;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => prompt = Some(text.as_str()),
            JobInput::Media { role, mime, bytes } if role == "image" && image.is_none() => image = Some((mime.clone(), bytes.clone())),
            JobInput::Text { role, .. } | JobInput::Media { role, .. } => {
                return Err(plain(refused(format!("the {} workflow takes one prompt and at most one image, not {role:?}", job.model))));
            }
        }
    }
    let prompt = prompt.ok_or_else(|| plain(refused("the card has no prompt")))?;
    let checkpoint = job.params.get("checkpoint").and_then(Value::as_str).filter(|c| !c.trim().is_empty()).unwrap_or(t.checkpoint);
    // The server checks the list too, but an empty one passes its check and fails in the run with
    // a generic error: a fresh install has no checkpoints, so say what to do here.
    if let Some(installed) = catalog.options("CheckpointLoaderSimple", "ckpt_name")
        && !installed.iter().any(|c| c == checkpoint)
    {
        let message = format!("the server has no checkpoint {checkpoint:?}; put it in ComfyUI's models/checkpoints folder, or choose another checkpoint");
        return Err((refused(message), true));
    }
    let steps = whole(job, "steps").map_err(plain)?.unwrap_or(t.steps);
    let cfg = match job.params.get("cfg") {
        None | Some(Value::Null) => t.cfg,
        Some(value) => value.as_f64().filter(|c| c.is_finite()).ok_or_else(|| plain(refused("`cfg` must be a number")))?,
    };
    let seed = seed(job).map_err(plain)?;

    let mut nodes = Map::new();
    let mut node = |id: &str, class: &str, inputs: Value| nodes.insert(id.to_owned(), json!({"class_type": class, "inputs": inputs}));
    node(CHECKPOINT, "CheckpointLoaderSimple", json!({"ckpt_name": checkpoint}));
    node(POSITIVE, "CLIPTextEncode", json!({"text": prompt, "clip": [CHECKPOINT, 1]}));
    let conditioning = if t.flux {
        node(GUIDANCE, "FluxGuidance", json!({"conditioning": [POSITIVE, 0], "guidance": 3.5}));
        GUIDANCE
    } else {
        POSITIVE
    };
    node(NEGATIVE, "CLIPTextEncode", json!({"text": "", "clip": [CHECKPOINT, 1]}));
    let denoise = if image.is_some() {
        node(LOAD_IMAGE, "LoadImage", json!({"image": IMAGE_PLACEHOLDER}));
        node(SCALE_IMAGE, "ImageScaleToTotalPixels", json!({"image": [LOAD_IMAGE, 0], "upscale_method": "lanczos", "megapixels": IMAGE_MEGAPIXELS}));
        node(LATENT, "VAEEncode", json!({"pixels": [SCALE_IMAGE, 0], "vae": [CHECKPOINT, 2]}));
        IMAGE_DENOISE
    } else {
        let (width, height) = size(job.params.get("aspect_ratio").and_then(Value::as_str)).map_err(plain)?;
        node(LATENT, t.empty_latent, json!({"width": width, "height": height, "batch_size": 1}));
        1.0
    };
    node(
        SAMPLER,
        "KSampler",
        json!({
            "model": [CHECKPOINT, 0], "positive": [conditioning, 0], "negative": [NEGATIVE, 0], "latent_image": [LATENT, 0],
            "seed": seed, "steps": steps, "cfg": cfg, "sampler_name": t.sampler, "scheduler": t.scheduler, "denoise": denoise,
        }),
    );
    node(DECODE, "VAEDecode", json!({"samples": [SAMPLER, 0], "vae": [CHECKPOINT, 2]}));
    node(SAVE, "SaveImage", json!({"images": [DECODE, 0], "filename_prefix": SAVE_PREFIX}));
    fill_defaults(&mut nodes, catalog);

    let mut params = Map::new();
    params.insert("workflow".into(), Value::Object(nodes));
    let inputs = image.map(|(mime, bytes)| JobInput::Media { role: format!("{LOAD_IMAGE}.image"), mime, bytes }).into_iter().collect();
    Ok(ProviderJob { model: job.model.clone(), params, inputs, idempotency_key: job.idempotency_key.clone() })
}

/// Sets required inputs the template leaves out to the server's default, so a server that added a
/// required widget to a core node still runs the workflow.
fn fill_defaults(nodes: &mut Map<String, Value>, catalog: &NodeCatalog) {
    for node in nodes.values_mut() {
        let Some(info) = node.get("class_type").and_then(Value::as_str).and_then(|class| catalog.get(class)) else { continue };
        let Some(inputs) = node.get_mut("inputs").and_then(Value::as_object_mut) else { continue };
        for spec in &info.inputs {
            if !spec.required || inputs.contains_key(&spec.name) {
                continue;
            }
            let default = match &spec.kind {
                InputKind::Int { default: Some(d), .. } => i64::try_from(*d).ok().map(Value::from),
                InputKind::Float { default: Some(d), .. } => serde_json::Number::from_f64(*d).map(Value::Number),
                InputKind::String { default: Some(d), .. } => Some(Value::String(d.clone())),
                InputKind::Boolean { default: Some(d) } => Some(Value::Bool(*d)),
                InputKind::Combo { default, options, .. } => default.clone().or_else(|| options.first().cloned()).map(Value::String),
                _ => None,
            };
            if let Some(default) = default {
                inputs.insert(spec.name.clone(), default);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn catalog(checkpoints: &[&str]) -> NodeCatalog {
        let info = json!({
            "CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [checkpoints]}}, "output": ["MODEL", "CLIP", "VAE"]},
            "ImageScaleToTotalPixels": {"input": {"required": {
                "image": ["IMAGE"], "upscale_method": [["nearest-exact", "lanczos"]], "megapixels": ["FLOAT", {"default": 1.0}],
                "resolution_steps": ["INT", {"default": 1, "min": 1, "max": 256}]
            }}, "output": ["IMAGE"]},
        });
        NodeCatalog::parse(&serde_json::to_vec(&info).unwrap()).unwrap().0
    }

    /// A server whose catalog doesn't describe the checkpoint loader.
    fn bare() -> NodeCatalog {
        NodeCatalog::parse(b"{}").unwrap().0
    }

    fn job(model: &str, params: Value, inputs: Vec<JobInput>) -> ProviderJob {
        ProviderJob { model: model.into(), params: params.as_object().cloned().unwrap(), inputs, idempotency_key: "nodecraft:run:1".into() }
    }

    fn prompt(text: &str) -> JobInput {
        JobInput::Text { role: "prompt".into(), text: text.into() }
    }

    fn png() -> JobInput {
        JobInput::Media { role: "image".into(), mime: "image/png".into(), bytes: Arc::from(&b"\x89PNG\r\n\x1a\n"[..]) }
    }

    fn workflow(job: &ProviderJob) -> &Map<String, Value> {
        job.params["workflow"].as_object().unwrap()
    }

    #[test]
    fn flux_text_to_image_uses_the_card_settings() {
        let job = job("flux-dev", json!({"aspect_ratio": "16:9", "steps": 20, "seed": 42}), vec![prompt("a red fox")]);
        let out = expand(&job, &catalog(&["flux1-dev-fp8.safetensors"])).unwrap();
        let w = workflow(&out);
        assert_eq!(w["2"]["inputs"]["text"], "a red fox");
        assert_eq!(w["3"]["class_type"], "FluxGuidance");
        assert_eq!(w["7"]["class_type"], "EmptySD3LatentImage");
        assert_eq!((w["7"]["inputs"]["width"].as_u64(), w["7"]["inputs"]["height"].as_u64()), (Some(1344), Some(768)));
        let sampler = &w["8"]["inputs"];
        assert_eq!((sampler["seed"].as_u64(), sampler["steps"].as_u64(), sampler["cfg"].as_f64()), (Some(42), Some(20), Some(1.0)));
        assert_eq!(sampler["positive"], json!(["3", 0]));
        assert_eq!(w["10"]["class_type"], "SaveImage");
        assert!(out.inputs.is_empty());
        assert!(!is_templated(&out), "the expanded job carries its workflow");
    }

    #[test]
    fn image_to_image_loads_scales_and_encodes_the_input() {
        let job = job("flux-dev", json!({}), vec![prompt("as a watercolour"), png()]);
        let out = expand(&job, &catalog(&["flux1-dev-fp8.safetensors"])).unwrap();
        let w = workflow(&out);
        assert_eq!(w["5"]["class_type"], "LoadImage");
        assert_eq!(w["7"]["class_type"], "VAEEncode");
        assert_eq!(w["8"]["inputs"]["denoise"].as_f64(), Some(IMAGE_DENOISE));
        assert_eq!(w["6"]["inputs"]["resolution_steps"], json!(1), "a newer required widget gets the server's default");
        assert!(matches!(&out.inputs[..], [JobInput::Media { role, .. }] if role == "5.image"));
    }

    #[test]
    fn sdxl_uses_the_chosen_checkpoint_and_guidance() {
        let job = job("sdxl-base", json!({"checkpoint": "juggernaut.safetensors", "cfg": 5.5}), vec![prompt("a lighthouse")]);
        let w = expand(&job, &catalog(&["juggernaut.safetensors"])).unwrap();
        let w = workflow(&w);
        assert_eq!(w["1"]["inputs"]["ckpt_name"], "juggernaut.safetensors");
        assert_eq!(w["8"]["inputs"]["cfg"].as_f64(), Some(5.5));
        assert_eq!(w["8"]["inputs"]["positive"], json!(["2", 0]), "SDXL has no FLUX guidance node");
        assert!(!w.contains_key("3"));
        assert_eq!(w["7"]["class_type"], "EmptyLatentImage");
    }

    #[test]
    fn the_seed_is_stable_per_job_when_the_card_leaves_it_random() {
        let a = expand(&job("sdxl-base", json!({}), vec![prompt("x")]), &bare()).unwrap();
        let b = expand(&job("sdxl-base", json!({}), vec![prompt("x")]), &bare()).unwrap();
        let seed = workflow(&a)["8"]["inputs"]["seed"].as_u64().unwrap();
        assert_eq!(Some(seed), workflow(&b)["8"]["inputs"]["seed"].as_u64());
        assert!(seed < 1 << 53);
    }

    #[test]
    fn refusals_name_what_to_do() {
        let (missing, stale) = expand(&job("flux-dev", json!({}), vec![prompt("x")]), &catalog(&["other.safetensors"])).unwrap_err();
        assert!(stale, "a checkpoint installed since the catalog was fetched is found on a refetch");
        assert!(missing.message.contains("flux1-dev-fp8.safetensors") && missing.message.contains("models/checkpoints"), "{}", missing.message);
        let (unknown, _) = expand(&job("wan-video", json!({}), vec![prompt("x")]), &bare()).unwrap_err();
        assert!(unknown.message.contains("Workflow node"), "{}", unknown.message);
        assert!(expand(&job("sdxl-base", json!({}), vec![]), &bare()).is_err(), "no prompt");
        assert!(expand(&job("sdxl-base", json!({}), vec![prompt("x"), prompt("y")]), &bare()).is_err());
        assert!(expand(&job("flux-dev", json!({"aspect_ratio": "2:1"}), vec![prompt("x")]), &bare()).is_err());
        let (none, stale) = expand(&job("sdxl-base", json!({}), vec![prompt("x")]), &catalog(&[])).unwrap_err();
        assert!(stale && none.message.contains("models/checkpoints"), "a server with no checkpoints says where to put one");
        for bad in [json!({"seed": -1}), json!({"seed": 1.5}), json!({"steps": "20"}), json!({"cfg": "high"})] {
            let (e, stale) = expand(&job("sdxl-base", bad.clone(), vec![prompt("x")]), &bare()).unwrap_err();
            assert!(!stale && e.message.contains("must be"), "{bad}: {}", e.message);
        }
        let mask = JobInput::Media { role: "mask".into(), mime: "image/png".into(), bytes: Arc::from(&b""[..]) };
        assert!(expand(&job("flux-dev", json!({}), vec![prompt("x"), mask]), &bare()).is_err());
    }
}
