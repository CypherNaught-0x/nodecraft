use super::*;
use crate::JobInput;
use base64::Engine as _;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Body<'a> {
    model: &'a str,
    prompt_text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_image: Option<String>,
    ratio: &'a str,
    pub duration: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<&'static str>,
}
pub(super) struct Prepared<'a> {
    pub endpoint: &'static str,
    pub body: Body<'a>,
}
fn count(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().filter(|s| s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())).and_then(|s| s.parse().ok()))
}
fn image(mime: &str, bytes: &[u8], limits: Limits) -> Result<String, ProviderError> {
    if bytes.is_empty() || bytes.len() > limits.image_bytes || !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
        return Err(invalid("Runway requires a PNG, JPEG or WebP image of at most 3 MiB"));
    }
    let cap = nodecraft_media::Limits { max_input_bytes: limits.image_bytes as u64, max_width: 4096, max_height: 4096, max_alloc: 128 * MIB as u64 };
    let meta = nodecraft_media::probe_image(bytes, &cap).map_err(|_| invalid("Runway input image could not be read within its bounds"))?;
    if meta.mime != mime {
        return Err(invalid("Runway image MIME does not match its bytes"));
    }
    nodecraft_media::decode_image(bytes, &cap).map_err(|_| invalid("Runway input image is damaged or exceeds its decode limit"))?;
    let size = bytes
        .len()
        .checked_add(2)
        .and_then(|v| v.checked_div(3))
        .and_then(|v| v.checked_mul(4))
        .and_then(|v| v.checked_add(mime.len().saturating_add(13)))
        .filter(|v| *v <= 5 * MIB)
        .ok_or_else(|| invalid("Runway encoded image exceeds its data URI limit"))?;
    let mut out = String::new();
    out.try_reserve_exact(size).map_err(|_| invalid("Runway image encoding allocation refused"))?;
    out.push_str("data:");
    out.push_str(mime);
    out.push_str(";base64,");
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut out);
    Ok(out)
}
pub(super) fn prepare(job: &ProviderJob, limits: Limits) -> Result<Prepared<'_>, ProviderError> {
    if !model(&job.model) || job.inputs.len() > 2 || job.params.len() > 3 || job.idempotency_key.len() > 256 {
        return Err(invalid("Runway requires a supported Gen-4 model and bounded inputs"));
    }
    if job.params.keys().any(|k| !matches!(k.as_str(), "ratio" | "duration_seconds" | "seed")) {
        return Err(invalid("unsupported Runway parameter"));
    }
    let duration = job
        .params
        .get("duration_seconds")
        .map(count)
        .unwrap_or(Some(5))
        .filter(|n| (2..=10).contains(n))
        .ok_or_else(|| invalid("Runway duration must be a whole number from 2 to 10 seconds"))?;
    let seed = job
        .params
        .get("seed")
        .map(|v| count(v).and_then(|n| u32::try_from(n).ok()).ok_or_else(|| invalid("Runway seed must fit an unsigned 32-bit integer")))
        .transpose()?;
    let mut prompt = None;
    let mut frame = None;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty() || text.len() > 4000 || text.encode_utf16().take(1001).count() > 1000 {
                    return Err(invalid("Runway prompt must contain 1 to 1000 UTF-16 units"));
                }
                prompt = Some(text.as_str());
            }
            JobInput::Media { role, mime, bytes } if role == "first_frame" && frame.is_none() => frame = Some((mime.as_str(), bytes.as_ref())),
            _ => return Err(invalid("Runway takes one prompt and at most one first-frame image")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("Runway requires a prompt"))?;
    if job.model == GEN4_TURBO && frame.is_none() {
        return Err(invalid("Runway Gen-4 Turbo requires a first-frame image"));
    }
    let ratios: &[&str] = if frame.is_some() { &["1280:720", "720:1280", "1104:832", "832:1104", "960:960", "1584:672"] } else { &["1280:720", "720:1280"] };
    let ratio = match job.params.get("ratio") {
        None => "1280:720",
        Some(v) => v.as_str().filter(|s| ratios.contains(s)).ok_or_else(|| invalid("Runway ratio is unsupported for this input mode"))?,
    };
    // Only after all scalar and role validation do we decode/encode the bounded image.
    let image = frame.map(|(mime, bytes)| image(mime, bytes, limits)).transpose()?;
    Ok(Prepared {
        endpoint: if frame.is_some() { "image_to_video" } else { "text_to_video" },
        body: Body {
            model: &job.model,
            prompt_text: prompt,
            prompt_image: image,
            ratio,
            duration,
            seed,
            output_format: (frame.is_some() && job.model == GEN4_5).then_some("mp4"),
        },
    })
}
