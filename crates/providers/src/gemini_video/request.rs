//! A job checked and shaped into one Interactions request, every image inline. Nothing here
//! touches the network.
use super::*;
use crate::JobInput;
use base64::Engine as _;
use serde_json::{Value, json};

/// Omni Flash's two shapes; landscape is its default.
pub(super) const ASPECTS: [&str; 2] = ["16:9", "9:16"];
/// The resolutions offered. 1080p is upscaled; 4K is left out until its token rate is published.
pub(super) const RESOLUTIONS: [&str; 3] = ["360p", "720p", "1080p"];
/// Subject references a job may wire.
pub(super) const MAX_REFERENCES: usize = 3;

fn chars(text: &str, max: usize) -> bool {
    text.chars().take(max.saturating_add(1)).count() <= max
}
/// An image whose bytes match its type, within the limit, as an input item.
fn image(mime: &str, bytes: &[u8], l: Limits, total: &mut usize) -> Result<Value, ProviderError> {
    if !matches!(mime, "image/png" | "image/jpeg") {
        return Err(invalid("Gemini video takes PNG or JPEG images"));
    }
    if bytes.is_empty() || bytes.len() > l.image_bytes {
        return Err(invalid(format!("a Gemini video image must be under {} MiB", l.image_bytes.div_ceil(MIB))));
    }
    if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
        return Err(invalid("a Gemini video image's bytes don't match its type"));
    }
    *total = total.checked_add(bytes.len()).filter(|n| *n <= l.total_image_bytes).ok_or_else(|| invalid("the Gemini video images are too large together"))?;
    Ok(json!({"type": "image", "mime_type": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes)}))
}
fn choice(job: &ProviderJob, name: &str, allowed: &[&str]) -> Result<Option<String>, ProviderError> {
    match job.params.get(name) {
        None => Ok(None),
        Some(value) => {
            value.as_str().filter(|v| allowed.contains(v)).map(|v| Some(v.to_owned())).ok_or_else(|| invalid(format!("unknown Gemini video {name}")))
        }
    }
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Value, ProviderError> {
    if job.model != OMNI_FLASH {
        return Err(invalid("choose the Gemini Omni Flash video model"));
    }
    if job.inputs.len() > 1 + MAX_REFERENCES || job.params.len() > 2 {
        return Err(invalid("Gemini video takes one prompt and up to three images"));
    }
    if let Some(name) = job.params.keys().find(|k| !matches!(k.as_str(), "aspect_ratio" | "resolution")) {
        return Err(invalid(format!("unknown Gemini video option {name}")));
    }
    let mut format = json!({"type": "video", "delivery": "uri"});
    if let Some(aspect) = choice(job, "aspect_ratio", &ASPECTS)? {
        format["aspect_ratio"] = json!(aspect);
    }
    if let Some(resolution) = choice(job, "resolution", &RESOLUTIONS)? {
        format["resolution"] = json!(resolution);
    }
    let mut prompt = None;
    let (mut first, mut last) = (None, None);
    let mut references = Vec::new();
    let mut total = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty() || !chars(text, l.prompt_chars) {
                    return Err(invalid(format!("a Gemini video prompt has 1 to {} characters", l.prompt_chars)));
                }
                prompt = Some(text.as_str());
            }
            JobInput::Media { role, mime, bytes } if role == "first_frame" && first.is_none() => first = Some(image(mime, bytes, l, &mut total)?),
            JobInput::Media { role, mime, bytes } if role == "last_frame" && last.is_none() => last = Some(image(mime, bytes, l, &mut total)?),
            JobInput::Media { role, mime, bytes } if role == "reference" && references.len() < MAX_REFERENCES => {
                references.push(image(mime, bytes, l, &mut total)?)
            }
            _ => return Err(invalid("Gemini video takes one prompt, a first and last frame, or up to three references")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("a Gemini video needs a prompt"))?;
    if first.is_some() && !references.is_empty() {
        return Err(invalid("a Gemini video starts from a frame or from references, not both"));
    }
    if last.is_some() && first.is_none() {
        return Err(invalid("a Gemini video's last frame needs a first frame"));
    }
    // Frames go first in order, then references, then the prompt, as Google's examples do.
    let mut items: Vec<Value> = [first, last].into_iter().flatten().chain(references).collect();
    items.push(json!({"type": "text", "text": prompt}));
    Ok(json!({
        "model": OMNI_FLASH,
        "store": false,
        "background": false,
        "stream": false,
        "response_format": format,
        "input": items,
    }))
}
