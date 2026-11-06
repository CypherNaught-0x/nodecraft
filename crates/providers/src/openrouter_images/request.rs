//! The request body: `{model, prompt, resolution?, aspect_ratio?, seed?, input_references?}`.
use super::{Limits, invalid};
use crate::secrets::Secret;
use crate::{HttpMethod, HttpRequest, JobInput, ProviderError, ProviderJob};
use base64::Engine;
use serde_json::{Map, Value, json};

/// Settings the adapter passes through, as the model's listing names them.
pub const PARAMS: [&str; 3] = ["resolution", "aspect_ratio", "seed"];
/// Image types OpenRouter's providers read as references.
const REFERENCE_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
/// JSON around one reference (`{"type":"image_url","image_url":{"url":"data:…;base64,"}}`).
const REFERENCE_OVERHEAD: usize = 96;
/// Longest model id accepted (OpenRouter's are well under this).
const MAX_MODEL: usize = 256;

/// A model id as OpenRouter writes them (`tencent/hy-image-v3.5-preview`): no spaces, quotes or
/// control characters, so it can never smuggle anything into the request.
pub(super) fn model_ok(model: &str) -> bool {
    !model.is_empty() && model.len() <= MAX_MODEL && model.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._:-~".contains(&b))
}

/// A short enum value such as `2K`, `1.5K` or `16:9`.
fn choice<'a>(name: &str, value: &'a Value) -> Result<&'a str, ProviderError> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 16 && text.bytes().all(|b| b.is_ascii_alphanumeric() || b".:x".contains(&b)))
        .ok_or_else(|| invalid(format!("OpenRouter image setting `{name}` must be one of the values the model lists")))
}

fn seed(value: &Value) -> Result<u64, ProviderError> {
    let seed = match value {
        Value::Number(n) => n.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    };
    // The API reads a signed 32-bit integer on some providers; stay within it everywhere.
    seed.filter(|n| *n <= 2_147_483_647).ok_or_else(|| invalid("the OpenRouter image seed is a whole number from 0 to 2147483647"))
}

fn encoded_len(n: usize) -> Option<usize> {
    n.checked_add(2)?.checked_div(3)?.checked_mul(4)
}

/// The JSON body for `job`, refused before anything is sent when it doesn't fit the limits.
pub(super) fn body(job: &ProviderJob, limits: Limits) -> Result<Vec<u8>, ProviderError> {
    if !model_ok(&job.model) {
        return Err(invalid("the OpenRouter image model id is not one OpenRouter could name"));
    }
    if let Some(name) = job.params.keys().find(|name| !PARAMS.contains(&name.as_str())) {
        return Err(invalid(format!("OpenRouter image models take no setting called `{name}`")));
    }
    let mut prompt: Option<&str> = None;
    let mut references: Vec<(&str, &[u8])> = Vec::new();
    let mut total = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty()
                    || text.len() > limits.prompt_bytes
                    || text.chars().take(limits.prompt_chars.saturating_add(1)).count() > limits.prompt_chars
                {
                    return Err(invalid(format!("the prompt must be 1 to {} characters", limits.prompt_chars)));
                }
                prompt = Some(text);
            }
            JobInput::Media { role, mime, bytes } if role == "image" => {
                if references.len() >= limits.references {
                    return Err(invalid(format!("OpenRouter image models take at most {} reference images", limits.references)));
                }
                if !REFERENCE_TYPES.contains(&mime.as_str()) || bytes.len() > limits.input_bytes {
                    return Err(invalid("a reference image must be a PNG, JPEG or WebP of at most 8 MiB"));
                }
                total = total
                    .checked_add(bytes.len())
                    .filter(|n| *n <= limits.total_input_bytes)
                    .ok_or_else(|| invalid("the reference images are too large together"))?;
                let meta =
                    nodecraft_media::probe_image(bytes, &limits.image(true)).map_err(|_| invalid("a reference image is corrupt or too large to read"))?;
                if meta.mime != mime.as_str() {
                    return Err(invalid("a reference image's contents don't match its type"));
                }
                references.push((mime, bytes));
            }
            _ => return Err(invalid("OpenRouter image models take one prompt and optional reference images")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("OpenRouter image models need a prompt"))?;

    // Measured before any base64 is allocated.
    let mut size = prompt.len().saturating_mul(6).saturating_add(job.model.len()).saturating_add(256);
    for (mime, bytes) in &references {
        let encoded = encoded_len(bytes.len()).ok_or_else(|| invalid("a reference image is too large"))?;
        size = size.saturating_add(encoded).saturating_add(mime.len()).saturating_add(REFERENCE_OVERHEAD);
    }
    if size > limits.request_bytes {
        return Err(invalid("the OpenRouter image request would be too large; use fewer or smaller reference images"));
    }

    let mut body = Map::new();
    body.insert("model".into(), json!(job.model));
    body.insert("prompt".into(), json!(prompt));
    for name in ["resolution", "aspect_ratio"] {
        if let Some(value) = job.params.get(name) {
            body.insert(name.into(), json!(choice(name, value)?));
        }
    }
    if let Some(value) = job.params.get("seed") {
        body.insert("seed".into(), json!(seed(value)?));
    }
    if !references.is_empty() {
        let mut list = Vec::new();
        list.try_reserve_exact(references.len()).map_err(|_| invalid("not enough memory for the reference images"))?;
        for (mime, bytes) in references {
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            list.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}));
        }
        body.insert("input_references".into(), Value::Array(list));
    }
    let bytes = serde_json::to_vec(&Value::Object(body)).map_err(|_| invalid("the OpenRouter image request couldn't be written"))?;
    if bytes.len() > limits.request_bytes {
        return Err(invalid("the OpenRouter image request would be too large; use fewer or smaller reference images"));
    }
    Ok(bytes)
}

pub(super) fn http(url: &str, body: Vec<u8>, key: &Secret, limits: Limits) -> HttpRequest {
    HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(url) }
        .header("Content-Type", "application/json")
        .secret_header("Authorization", format!("Bearer {}", key.expose()))
        // Large images on busy providers can take minutes.
        .timeout_ms(300_000)
        .max_response_bytes(limits.response_bytes)
}
