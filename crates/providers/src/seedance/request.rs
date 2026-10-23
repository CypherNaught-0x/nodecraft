//! A job checked and shaped into one task-create request, every image inline as a data URL.
//! Nothing here touches the network.
use super::*;
use crate::JobInput;
use base64::Engine as _;
use serde_json::{Value, json};

/// What one Seedance model takes.
pub(super) struct Model {
    pub id: &'static str,
    pub resolutions: &'static [&'static str],
    pub max_seconds: u64,
    pub max_references: usize,
    /// Seedance 2.5 keeps a first frame's own shape: its ratio must be `adaptive`.
    pub frames_keep_shape: bool,
}
pub(super) const MODELS: [Model; 4] = [
    Model { id: V2_5, resolutions: &["480p", "720p", "1080p"], max_seconds: 30, max_references: 30, frames_keep_shape: true },
    Model { id: V2_0, resolutions: &["480p", "720p", "1080p", "4k"], max_seconds: 15, max_references: 9, frames_keep_shape: false },
    Model { id: V2_0_FAST, resolutions: &["480p", "720p"], max_seconds: 15, max_references: 9, frames_keep_shape: false },
    Model { id: V2_0_MINI, resolutions: &["480p", "720p"], max_seconds: 15, max_references: 9, frames_keep_shape: false },
];
pub(super) fn model(id: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.id == id)
}
pub(super) const RATIOS: [&str; 7] = ["adaptive", "16:9", "4:3", "1:1", "3:4", "9:16", "21:9"];
/// The shortest clip; a model's own longest is [`Model::max_seconds`].
pub(super) const MIN_SECONDS: u64 = 4;
/// BytePlus's bounds on an input image's sides, in pixels.
const SIDE: std::ops::RangeInclusive<u32> = 300..=6000;

fn chars(text: &str, max: usize) -> bool {
    text.chars().take(max.saturating_add(1)).count() <= max
}
/// An image whose bytes match its type and whose shape BytePlus takes, as a content item.
fn image(mime: &str, bytes: &[u8], role: &str, l: Limits, total: &mut usize) -> Result<Value, ProviderError> {
    if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
        return Err(invalid("Seedance takes PNG, JPEG or WebP images"));
    }
    if bytes.is_empty() || bytes.len() >= l.image_bytes {
        return Err(invalid(format!("a Seedance image must be under {} MB", l.image_bytes.div_ceil(1_000_000))));
    }
    if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
        return Err(invalid("a Seedance image's bytes don't match its type"));
    }
    // The header alone: the size and EXIF orientation, nothing decoded.
    let probe = nodecraft_media::Limits { max_input_bytes: l.image_bytes as u64, max_width: 16_384, max_height: 16_384, max_alloc: 64 * MIB as u64 };
    let meta = nodecraft_media::probe_image(bytes, &probe).map_err(|_| invalid("a Seedance image couldn't be read"))?;
    let (w, h) = (u64::from(meta.width), u64::from(meta.height));
    // Width over height within [0.4, 2.5].
    if !SIDE.contains(&meta.width) || !SIDE.contains(&meta.height) || 5 * w < 2 * h || 2 * w > 5 * h {
        return Err(invalid("a Seedance image is 300 to 6000 pixels a side, at most 2.5 times as wide as tall or tall as wide"));
    }
    *total = total.checked_add(bytes.len()).filter(|n| *n <= l.total_image_bytes).ok_or_else(|| invalid("the Seedance images are too large together"))?;
    let url = format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes));
    Ok(json!({"type": "image_url", "image_url": {"url": url}, "role": role}))
}
fn choice<'a>(job: &'a ProviderJob, name: &str, allowed: &[&str]) -> Result<Option<&'a str>, ProviderError> {
    match job.params.get(name) {
        None => Ok(None),
        Some(value) => value.as_str().filter(|v| allowed.contains(v)).map(Some).ok_or_else(|| invalid(format!("unknown Seedance {name} for this model"))),
    }
}
/// The catalog's whole numbers arrive as decimal text.
fn count(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
fn flag(value: &Value) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_str() {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    })
}

pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Value, ProviderError> {
    let model = model(&job.model).ok_or_else(|| invalid("choose a Seedance 2.5 or 2.0 model"))?;
    if let Some(name) = job.params.keys().find(|k| !matches!(k.as_str(), "resolution" | "ratio" | "duration_seconds" | "generate_audio" | "seed")) {
        return Err(invalid(format!("unknown Seedance option {name}")));
    }
    if job.inputs.len() > 1 + model.max_references {
        return Err(invalid(format!("this Seedance model takes one prompt and up to {} images", model.max_references)));
    }
    // Always sent: the quote is per second at this resolution, so nothing is left to the model.
    let resolution = choice(job, "resolution", model.resolutions)?.unwrap_or("720p");
    let ratio = choice(job, "ratio", &RATIOS)?.unwrap_or("adaptive");
    let duration = match job.params.get("duration_seconds") {
        None => 5,
        Some(value) => count(value)
            .filter(|n| (MIN_SECONDS..=model.max_seconds).contains(n))
            .ok_or_else(|| invalid(format!("this Seedance video is {MIN_SECONDS} to {} seconds long", model.max_seconds)))?,
    };
    let audio = match job.params.get("generate_audio") {
        None => true,
        Some(value) => flag(value).ok_or_else(|| invalid("Seedance generate_audio is on or off"))?,
    };
    let mut body = json!({"model": model.id, "resolution": resolution, "ratio": ratio, "duration": duration, "generate_audio": audio, "watermark": false});
    if let Some(seed) = job.params.get("seed") {
        let seed = count(seed).filter(|n| i32::try_from(*n).is_ok()).ok_or_else(|| invalid("a Seedance seed is a whole number from 0 to 2147483647"))?;
        body["seed"] = json!(seed);
    }
    let mut prompt = None;
    let (mut first, mut last) = (None, None);
    let mut references = Vec::new();
    let mut total = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty() || !chars(text, l.prompt_chars) {
                    return Err(invalid(format!("a Seedance prompt has 1 to {} characters", l.prompt_chars)));
                }
                prompt = Some(text.as_str());
            }
            JobInput::Media { role, mime, bytes } if role == "first_frame" && first.is_none() => {
                first = Some(image(mime, bytes, "first_frame", l, &mut total)?)
            }
            JobInput::Media { role, mime, bytes } if role == "last_frame" && last.is_none() => last = Some(image(mime, bytes, "last_frame", l, &mut total)?),
            JobInput::Media { role, mime, bytes } if role == "reference" && references.len() < model.max_references => {
                references.push(image(mime, bytes, "reference_image", l, &mut total)?)
            }
            _ => return Err(invalid("Seedance takes one prompt, a first and last frame, or reference images")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("a Seedance video needs a prompt"))?;
    if first.is_some() && !references.is_empty() {
        return Err(invalid("a Seedance video starts from a frame or from references, not both"));
    }
    if last.is_some() && first.is_none() {
        return Err(invalid("a Seedance video's last frame needs a first frame"));
    }
    if first.is_some() && model.frames_keep_shape && ratio != "adaptive" {
        return Err(invalid("Seedance 2.5 keeps the first frame's shape: choose the adaptive ratio"));
    }
    let mut content = vec![json!({"type": "text", "text": prompt})];
    content.extend([first, last].into_iter().flatten().chain(references));
    body["content"] = Value::Array(content);
    Ok(body)
}
