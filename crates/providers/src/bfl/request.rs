use super::*;
use crate::JobInput;
use serde_json::{Value, json};

pub(super) fn geometry(width: u64, height: u64) -> Result<(), ProviderError> {
    if !(64..=2048).contains(&width)
        || !(64..=2048).contains(&height)
        || !width.is_multiple_of(16)
        || !height.is_multiple_of(16)
        || width.checked_mul(height).is_none_or(|pixels| pixels > PIXELS)
    {
        return Err(invalid("Black Forest Labs dimensions must be multiples of 16 from 64 to 2048, at most 4 million pixels"));
    }
    Ok(())
}
/// The most bytes of the one image an edit sends: base64 makes the request a third larger.
const INPUT_BYTES: usize = 10 * 1024 * 1024;

fn prompt(job: &ProviderJob) -> Result<&str, ProviderError> {
    let prompts: Vec<&str> = job
        .inputs
        .iter()
        .filter_map(|input| match input {
            JobInput::Text { role, text } if role == "prompt" => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let [text] = prompts.as_slice() else { return Err(invalid("Black Forest Labs requires one text prompt")) };
    if text.len() > 16 * 1024 || text.trim().is_empty() || text.contains('\0') {
        return Err(invalid("Black Forest Labs prompt must be nonempty text up to 16 KiB without NUL"));
    }
    Ok(text)
}

fn seed(job: &ProviderJob, body: &mut Value) -> Result<(), ProviderError> {
    let Some(value) = job.params.get("seed") else { return Ok(()) };
    let seed =
        value.as_u64().filter(|seed| *seed <= u64::from(u32::MAX)).ok_or_else(|| invalid("Black Forest Labs seed must be an unsigned 32-bit integer"))?;
    if let Some(object) = body.as_object_mut() {
        object.insert("seed".into(), Value::from(seed));
    }
    Ok(())
}

/// The request body for `spec`, and the output size it fixes (0 × 0 for an edit, whose size
/// follows its input image).
pub(super) fn body(job: &ProviderJob, spec: &Spec) -> Result<(Value, u64, u64), ProviderError> {
    if job.model != spec.model || job.params.len() > 3 || job.idempotency_key.len() > 256 {
        return Err(invalid("Black Forest Labs supports one direct model's prompt and bounded parameters"));
    }
    if spec.edit {
        return edit(job);
    }
    if job.inputs.len() != 1 {
        return Err(invalid("Black Forest Labs text-to-image takes one text prompt and nothing else"));
    }
    let text = prompt(job)?;
    if job.params.keys().any(|key| !matches!(key.as_str(), "width" | "height" | "seed")) {
        return Err(invalid("Black Forest Labs supports only width, height and seed parameters"));
    }
    let integer = |key, default| match job.params.get(key) {
        Some(value) => value.as_u64().ok_or_else(|| invalid("Black Forest Labs parameters must be unsigned integers")),
        None => Ok(default),
    };
    let width = integer("width", 1024)?;
    let height = integer("height", 1024)?;
    geometry(width, height)?;
    let mut body = json!({"prompt":text,"width":width,"height":height,"output_format":"png"});
    seed(job, &mut body)?;
    Ok((body, width, height))
}

/// A FLUX.1 Kontext edit: the prompt and one PNG, JPEG or WebP image, sent base64-encoded as the
/// schema's `input_image` ("Base64 encoded image or URL"); only `seed` is a setting.
fn edit(job: &ProviderJob) -> Result<(Value, u64, u64), ProviderError> {
    if job.inputs.len() != 2 {
        return Err(invalid("Black Forest Labs Kontext edits take one text prompt and one image"));
    }
    if job.params.keys().any(|key| key != "seed") {
        return Err(invalid("Black Forest Labs Kontext edits support only a seed parameter"));
    }
    let text = prompt(job)?;
    let images: Vec<(&str, &[u8])> = job
        .inputs
        .iter()
        .filter_map(|input| match input {
            JobInput::Media { role, mime, bytes } if role == "image" => Some((mime.as_str(), &bytes[..])),
            _ => None,
        })
        .collect();
    let [(mime, bytes)] = images.as_slice() else { return Err(invalid("Black Forest Labs Kontext edits take one image")) };
    if bytes.is_empty() || bytes.len() > INPUT_BYTES {
        return Err(invalid("Black Forest Labs Kontext edits take an image of at most 10 MiB"));
    }
    if !matches!(*mime, "image/png" | "image/jpeg" | "image/webp") || nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(*mime) {
        return Err(invalid("Black Forest Labs Kontext edits take a PNG, JPEG or WebP image whose bytes match its type"));
    }
    // Its header must read as a whole, bounded image: a truncated or absurdly large one would be
    // sent to a paid endpoint for nothing. Only the header is read.
    let limits = nodecraft_media::Limits { max_input_bytes: INPUT_BYTES as u64, max_width: 8192, max_height: 8192, max_alloc: 256 * 1024 * 1024 };
    let meta = nodecraft_media::probe_image(bytes, &limits)
        .map_err(|_| invalid("Black Forest Labs Kontext edits take a readable image of at most 8192 pixels a side"))?;
    if meta.mime != *mime {
        return Err(invalid("Black Forest Labs Kontext edits take a PNG, JPEG or WebP image whose bytes match its type"));
    }
    use base64::Engine as _;
    let image = base64::engine::general_purpose::STANDARD.encode(bytes);
    let mut body = json!({"prompt":text,"input_image":image,"output_format":"png"});
    seed(job, &mut body)?;
    Ok((body, 0, 0))
}
