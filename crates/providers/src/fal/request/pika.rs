//! Current Pika 2.5 first-frame schema; fixed720p keeps the published quote exact.
use super::*;
#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    image_url: &'a str,
    resolution: &'static str,
    duration: u64,
}
pub(super) fn prepare(j: &ProviderJob, l: Limits) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() != 2 || j.params.len() > 1 || j.idempotency_key.len() > 256 || j.params.keys().any(|key| key != "duration_seconds") {
        return Err(invalid("Pika needs one prompt, one first frame and only a duration setting"));
    }
    let mut text_bytes = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut frame = None;
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                text_bytes = text_bytes.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Pika requires one named prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                text_bytes = text_bytes.saturating_add(role.len()).saturating_add(mime.len());
                if role != "first_frame"
                    || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp")
                    || bytes.len() > l.input_bytes
                    || frame.replace((mime.as_str(), bytes.as_ref())).is_some()
                {
                    return Err(invalid("Pika needs one PNG, JPEG or WebP first frame of at most 1 MiB"));
                }
            }
        }
    }
    if text_bytes > l.text_bytes {
        return Err(invalid("Pika request metadata exceeds limit"));
    }
    let prompt = prompt
        .filter(|text| !text.trim().is_empty() && text.chars().count() <= 2048)
        .ok_or_else(|| invalid("Pika needs a nonempty prompt of at most 2048 characters"))?;
    let (mime, bytes) = frame.ok_or_else(|| invalid("Pika first frame required"))?;
    let duration = integer(j, "duration_seconds", 5, 5, 10)?;
    if !matches!(duration, 5 | 10) {
        return Err(invalid("Pika duration must be 5 or 10 seconds"));
    }
    // The schema does not define an exact aspect/encoded-size envelope for 720p. A smaller
    // caller budget cannot promise to retain the offered output, so new paid work requires
    // the complete local output envelope. This is admission policy, not a vendor minimum.
    let output = Limits::VIDEO_HARD;
    if l.output_width < output.output_width
        || l.output_height < output.output_height
        || l.total_pixels < output.total_pixels
        || l.output_bytes < output.output_bytes
        || l.total_output_bytes < output.total_output_bytes
        || l.metadata_bytes < output.metadata_bytes
    {
        return Err(invalid("Pika 720p submissions require the full local video output envelope"));
    }
    let mut body = Body { prompt, image_url: "", resolution: "720p", duration };
    // Complete escaped JSON and base64 extent precedes codecs and owned image encodings.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Pika escaped request exceeds limit"))?;
    let encoded_len = encoded(mime, bytes)?;
    let length = counter.n.checked_add(encoded_len).filter(|n| *n <= l.request_bytes).ok_or_else(|| invalid("Pika encoded request exceeds limit"))?;
    raster::check(mime, bytes)?;
    let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Pika frame exceeds raster or 1024×1024 px limits"))?;
    if meta.mime != mime {
        return Err(invalid("Pika first-frame MIME mismatch"));
    }
    if nodecraft_media::image_requires_orientation(bytes, &l.image(true)).map_err(|_| invalid("Pika frame orientation refused"))? {
        return Err(invalid("Pika needs an upright first frame: apply its EXIF orientation first"));
    }
    observe(0);
    drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Pika first-frame codec refused"))?);
    let image_url = uri(mime, bytes, encoded_len)?;
    body.image_url = &image_url;
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Pika request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Pika request extent changed"));
    }
    Ok((Record { model: Model::Pika, requested: 1, width: 0, height: 0, duration_ms: 0 }, out))
}
