//! A job checked and shaped into one `predictLongRunning` request, every image inline. Nothing
//! here touches the network.
use super::*;
use crate::JobInput;
use base64::Engine as _;
use serde_json::{Value, json};

pub(super) const ASPECTS: [&str; 2] = ["16:9", "9:16"];
pub(super) const DURATIONS: [u64; 3] = [4, 6, 8];
/// Subject references a job may wire.
pub(super) const MAX_REFERENCES: usize = 3;

/// The resolutions a model makes: 4K is Veo 3.1's alone.
pub(super) fn resolutions(model: &str) -> &'static [&'static str] {
    if model == GENERATE { &["720p", "1080p", "4k"] } else { &["720p", "1080p"] }
}
/// Lite takes a prompt only: Google's model card lists no image input for it.
fn takes_images(model: &str) -> bool {
    model != LITE
}

fn chars(text: &str, max: usize) -> bool {
    text.chars().take(max.saturating_add(1)).count() <= max
}
/// An image whose bytes match its type, within the limit, as a request image.
fn image(mime: &str, bytes: &[u8], l: Limits, total: &mut usize) -> Result<Value, ProviderError> {
    if !matches!(mime, "image/png" | "image/jpeg") {
        return Err(invalid("Veo takes PNG or JPEG images"));
    }
    if bytes.is_empty() || bytes.len() > l.image_bytes {
        return Err(invalid(format!("a Veo image must be under {} MiB", l.image_bytes.div_ceil(MIB))));
    }
    if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
        return Err(invalid("a Veo image's bytes don't match its type"));
    }
    *total = total.checked_add(bytes.len()).filter(|n| *n <= l.total_image_bytes).ok_or_else(|| invalid("the Veo images are too large together"))?;
    Ok(json!({"mimeType": mime, "bytesBase64Encoded": base64::engine::general_purpose::STANDARD.encode(bytes)}))
}
fn choice(job: &ProviderJob, name: &str, allowed: &[&str]) -> Result<Option<String>, ProviderError> {
    match job.params.get(name) {
        None => Ok(None),
        Some(value) => value.as_str().filter(|v| allowed.contains(v)).map(|v| Some(v.to_owned())).ok_or_else(|| invalid(format!("unknown Veo {name}"))),
    }
}
fn count(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Value, ProviderError> {
    let model = job.model.as_str();
    if ![GENERATE, FAST, LITE].contains(&model) {
        return Err(invalid("choose a Veo 3.1 model"));
    }
    if let Some(name) = job.params.keys().find(|k| !matches!(k.as_str(), "aspect_ratio" | "resolution" | "duration_seconds" | "seed")) {
        return Err(invalid(format!("unknown Veo option {name}")));
    }
    if job.inputs.len() > 1 + MAX_REFERENCES {
        return Err(invalid("Veo takes one prompt and up to three images"));
    }
    let duration = match job.params.get("duration_seconds") {
        None => 8,
        // The catalog's whole numbers arrive as decimal text.
        Some(value) => count(value).filter(|n| DURATIONS.contains(n)).ok_or_else(|| invalid("a Veo video is 4, 6 or 8 seconds long"))?,
    };
    // Generating audio is Veo's default and what the catalog prices; it's sent so that stays true.
    let mut parameters = json!({"sampleCount": 1, "durationSeconds": duration, "generateAudio": true});
    if let Some(aspect) = choice(job, "aspect_ratio", &ASPECTS)? {
        parameters["aspectRatio"] = json!(aspect);
    }
    if let Some(resolution) = choice(job, "resolution", resolutions(model))? {
        parameters["resolution"] = json!(resolution);
    }
    if let Some(seed) = job.params.get("seed") {
        let seed = count(seed).filter(|n| u32::try_from(*n).is_ok()).ok_or_else(|| invalid("a Veo seed is a whole number from 0 to 4294967295"))?;
        parameters["seed"] = json!(seed);
    }
    let mut prompt = None;
    let (mut first, mut last) = (None, None);
    let mut references = Vec::new();
    let mut total = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty() || !chars(text, l.prompt_chars) {
                    return Err(invalid(format!("a Veo prompt has 1 to {} characters", l.prompt_chars)));
                }
                prompt = Some(text.as_str());
            }
            JobInput::Media { .. } if !takes_images(model) => return Err(invalid("Veo 3.1 Lite takes a prompt only")),
            JobInput::Media { role, mime, bytes } if role == "first_frame" && first.is_none() => first = Some(image(mime, bytes, l, &mut total)?),
            JobInput::Media { role, mime, bytes } if role == "last_frame" && last.is_none() => last = Some(image(mime, bytes, l, &mut total)?),
            JobInput::Media { role, mime, bytes } if role == "reference" && references.len() < MAX_REFERENCES => {
                references.push(json!({"image": image(mime, bytes, l, &mut total)?, "referenceType": "asset"}))
            }
            _ => return Err(invalid("Veo takes one prompt, a first and last frame, or up to three references")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("a Veo video needs a prompt"))?;
    if first.is_some() && !references.is_empty() {
        return Err(invalid("a Veo video starts from a frame or from references, not both"));
    }
    if last.is_some() && first.is_none() {
        return Err(invalid("a Veo video's last frame needs a first frame"));
    }
    if !references.is_empty() && duration != 8 {
        return Err(invalid("a Veo video made from references is 8 seconds long"));
    }
    let mut instance = json!({"prompt": prompt});
    if let Some(first) = first {
        instance["image"] = first;
    }
    if let Some(last) = last {
        instance["lastFrame"] = last;
    }
    if !references.is_empty() {
        instance["referenceImages"] = Value::Array(references);
    }
    Ok(json!({"instances": [instance], "parameters": parameters}))
}
