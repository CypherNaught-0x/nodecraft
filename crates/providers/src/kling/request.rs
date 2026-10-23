//! A job checked and shaped into one Kling create request, every image inline. Nothing here
//! touches the network.
use super::*;
use crate::JobInput;
use base64::Engine as _;
use nodecraft_media::ContentHash;
use serde_json::{Map, Value, json};

pub(super) const DURATIONS: [&str; 2] = ["5", "10"];
pub(super) const MODES: [&str; 2] = ["std", "pro"];
pub(super) const VIDEO_ASPECTS: [&str; 3] = ["16:9", "9:16", "1:1"];
pub(super) const IMAGE_ASPECTS: [&str; 8] = ["16:9", "9:16", "1:1", "4:3", "3:4", "3:2", "2:3", "21:9"];
/// Reference images a multi-image video takes.
pub(super) const MAX_REFERENCES: usize = 4;

/// The create request: where it goes and what it says.
pub(super) struct Prepared {
    pub(super) path: &'static str,
    pub(super) body: Value,
}

fn chars(text: &str, max: usize) -> bool {
    text.chars().take(max.saturating_add(1)).count() <= max
}
/// An image whose bytes match its type, within the limits, as Kling takes it: bare base64.
fn image(mime: &str, bytes: &[u8], l: Limits, total: &mut usize) -> Result<Value, ProviderError> {
    if !matches!(mime, "image/png" | "image/jpeg") {
        return Err(invalid("Kling takes PNG or JPEG images"));
    }
    if bytes.is_empty() || bytes.len() > l.image_bytes {
        return Err(invalid(format!("a Kling image must be under {} MiB", l.image_bytes.div_ceil(MIB))));
    }
    if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
        return Err(invalid("a Kling image's bytes don't match its type"));
    }
    *total = total.checked_add(bytes.len()).filter(|n| *n <= l.total_image_bytes).ok_or_else(|| invalid("the Kling images are too large together"))?;
    Ok(json!(base64::engine::general_purpose::STANDARD.encode(bytes)))
}
fn choice<'p>(params: &'p Map<String, Value>, name: &str, allowed: &[&str]) -> Result<Option<&'p str>, ProviderError> {
    match params.get(name) {
        None => Ok(None),
        Some(value) => value.as_str().filter(|v| allowed.contains(v)).map(Some).ok_or_else(|| invalid(format!("unknown Kling {name}"))),
    }
}
/// The job's key as an `external_task_id`: a stable hash, so any key fits Kling's id rules and a
/// second create for the same job is refused as a duplicate rather than billed again.
pub(super) fn external_id(key: &str) -> String {
    ContentHash::of(key.as_bytes()).to_hex()
}

pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared, ProviderError> {
    let model = MODELS.iter().find(|m| m.name == job.model).ok_or_else(|| invalid("choose a Kling model"))?;
    if job.inputs.len() > 2 + MAX_REFERENCES || job.params.len() > 4 {
        return Err(invalid("too many inputs or options for Kling"));
    }
    let allowed = |name: &str| match name {
        "aspect_ratio" => true,
        "duration" => !model.image,
        "mode" => model.modes,
        "cfg_scale" => model.cfg_scale,
        _ => false,
    };
    if let Some(name) = job.params.keys().find(|k| !allowed(k)) {
        return Err(invalid(format!("unknown Kling option {name} for {}", model.name)));
    }
    let (mut prompt, mut negative) = (None, None);
    let (mut first, mut last) = (None, None);
    let mut references = Vec::new();
    let mut total = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => prompt = Some(text.as_str()),
            JobInput::Text { role, text } if role == "negative_prompt" && negative.is_none() => negative = Some(text.as_str()),
            JobInput::Media { role, mime, bytes } if role == "first_frame" && first.is_none() && !model.image => {
                first = Some(image(mime, bytes, l, &mut total)?)
            }
            JobInput::Media { role, mime, bytes } if role == "last_frame" && last.is_none() && model.end_frame => {
                last = Some(image(mime, bytes, l, &mut total)?)
            }
            JobInput::Media { role, mime, bytes } if role == "reference" && model.references && references.len() < MAX_REFERENCES => {
                references.push(json!({"image": image(mime, bytes, l, &mut total)?}))
            }
            _ => return Err(invalid(format!("{} takes a prompt, a negative prompt and the images its mode names", model.name))),
        }
    }
    for text in [prompt, negative].into_iter().flatten() {
        if !chars(text, l.prompt_chars) {
            return Err(invalid(format!("a Kling prompt has at most {} characters", l.prompt_chars)));
        }
    }
    let prompt = prompt.filter(|p| !p.trim().is_empty());
    let negative = negative.filter(|p| !p.trim().is_empty());
    if first.is_some() && !references.is_empty() {
        return Err(invalid("a Kling video starts from a frame or from references, not both"));
    }
    if last.is_some() && first.is_none() {
        return Err(invalid("a Kling end frame needs a start frame"));
    }
    let mode = choice(&job.params, "mode", &MODES)?;
    if last.is_some() && mode != Some("pro") {
        return Err(invalid("a Kling end frame needs Professional mode"));
    }
    // A start frame sets the shape of an image-to-video clip; Kling takes no aspect ratio there.
    let aspects: &[&str] = if model.image { &IMAGE_ASPECTS } else { &VIDEO_ASPECTS };
    let aspect = choice(&job.params, "aspect_ratio", aspects)?.filter(|_| first.is_none());

    let mut body = Map::new();
    body.insert("model_name".into(), json!(model.name));
    let path = if model.image {
        body.insert("n".into(), json!(1));
        "images/generations"
    } else {
        body.insert("external_task_id".into(), json!(external_id(&job.idempotency_key)));
        if let Some(duration) = choice(&job.params, "duration", &DURATIONS)? {
            body.insert("duration".into(), json!(duration));
        }
        if let Some(mode) = mode {
            body.insert("mode".into(), json!(mode));
        }
        if let Some(cfg) = job.params.get("cfg_scale") {
            let cfg = cfg.as_f64().filter(|c| (0.0..=1.0).contains(c)).ok_or_else(|| invalid("Kling prompt adherence is from 0 to 1"))?;
            body.insert("cfg_scale".into(), json!(cfg));
        }
        match (first, last, references.is_empty()) {
            (Some(first), last, true) => {
                body.insert("image".into(), first);
                if let Some(last) = last {
                    body.insert("image_tail".into(), last);
                }
                "videos/image2video"
            }
            (None, None, false) => {
                body.insert("image_list".into(), Value::Array(references));
                "videos/multi-image2video"
            }
            _ => "videos/text2video",
        }
    };
    // Text to video and text to image need a prompt; a start frame or references can go without.
    if prompt.is_none() && (path == "videos/text2video" || path == "images/generations") {
        return Err(invalid(format!("a Kling prompt has 1 to {} characters", l.prompt_chars)));
    }
    if path == "videos/multi-image2video" && prompt.is_none() {
        return Err(invalid("a Kling video from references needs a prompt"));
    }
    if let Some(prompt) = prompt {
        body.insert("prompt".into(), json!(prompt));
    }
    if let Some(negative) = negative {
        body.insert("negative_prompt".into(), json!(negative));
    }
    if let Some(aspect) = aspect {
        body.insert("aspect_ratio".into(), json!(aspect));
    }
    Ok(Prepared { path, body: Value::Object(body) })
}
