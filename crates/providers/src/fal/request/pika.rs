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

/// One keyframe transition: how long it lasts.
#[derive(Serialize)]
struct Transition {
    duration: u64,
}

#[derive(Serialize)]
struct FramesBody<'a> {
    prompt: &'a str,
    image_urls: Vec<String>,
    transitions: Vec<Transition>,
    resolution: &'static str,
}

/// The most keyframes Pikaframes takes, and the longest clip it makes, in seconds.
const MAX_KEYFRAMES: usize = 5;
const MAX_FRAMES_SECONDS: u64 = 25;

/// Current Pika 2.5 Pikaframes schema (#1061): 2 to 5 keyframes in order, a transition between
/// each pair. The clip's whole length is the priced setting and is split across the transitions
/// as evenly as whole seconds allow, so the quote is exact; resolution is fixed at 720p.
pub(super) fn prepare_frames(j: &ProviderJob, l: Limits) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.params.len() > 1 || j.idempotency_key.len() > 256 || j.params.keys().any(|key| key != "duration_seconds") {
        return Err(invalid("Pika keyframes take only a duration setting"));
    }
    if j.inputs.len() > MAX_KEYFRAMES + 1 {
        return Err(invalid("Pika keyframes take one prompt and 2 to 5 keyframes"));
    }
    let mut text_bytes = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut frames = Vec::new();
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                text_bytes = text_bytes.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Pika keyframes require one named prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                text_bytes = text_bytes.saturating_add(role.len()).saturating_add(mime.len());
                if role != "keyframes" || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") || bytes.len() > l.input_bytes {
                    return Err(invalid("Pika keyframes are PNG, JPEG or WebP images of at most 1 MiB each"));
                }
                frames.push((mime.as_str(), bytes.as_ref()));
            }
        }
    }
    if text_bytes > l.text_bytes {
        return Err(invalid("Pika request metadata exceeds limit"));
    }
    if !(2..=MAX_KEYFRAMES).contains(&frames.len()) {
        return Err(invalid("Pika keyframes need 2 to 5 keyframes"));
    }
    let prompt = prompt
        .filter(|text| !text.trim().is_empty() && text.chars().count() <= 2048)
        .ok_or_else(|| invalid("Pika keyframes need a nonempty prompt of at most 2048 characters"))?;
    let duration = integer(j, "duration_seconds", 5, 5, MAX_FRAMES_SECONDS)?;
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
    let transitions = split(duration, frames.len().saturating_sub(1))?;
    // Every frame's encoded extent is counted before any codec runs.
    let mut encoded_total = 0usize;
    for (mime, bytes) in &frames {
        encoded_total = encoded_total.checked_add(encoded(mime, bytes)?).ok_or_else(|| invalid("Pika encoded request exceeds limit"))?;
    }
    let shell = FramesBody { prompt, image_urls: vec![String::new(); frames.len()], transitions: Vec::new(), resolution: "720p" };
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &shell).map_err(|_| invalid("Pika escaped request exceeds limit"))?;
    // Each transition adds at most `{"duration":25},` to the body.
    let transitions_len = transitions.len().saturating_mul(16);
    let bound = counter
        .n
        .checked_add(encoded_total)
        .and_then(|n| n.checked_add(transitions_len))
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("Pika keyframes are too large together: make them smaller (Resize) or use fewer"))?;
    // Every frame is checked with its codec before any is encoded.
    for (mime, bytes) in &frames {
        raster::check(mime, bytes)?;
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Pika keyframe exceeds raster or 1024×1024 px limits"))?;
        if meta.mime != *mime {
            return Err(invalid("Pika keyframe MIME mismatch"));
        }
        if nodecraft_media::image_requires_orientation(bytes, &l.image(true)).map_err(|_| invalid("Pika keyframe orientation refused"))? {
            return Err(invalid("Pika needs upright keyframes: apply their EXIF orientation first"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Pika keyframe codec refused"))?);
    }
    let mut image_urls = Vec::with_capacity(frames.len());
    for (mime, bytes) in &frames {
        image_urls.push(uri(mime, bytes, encoded(mime, bytes)?)?);
    }
    let body = FramesBody { prompt, image_urls, transitions: transitions.into_iter().map(|duration| Transition { duration }).collect(), resolution: "720p" };
    let mut out = Vec::new();
    reserve(&mut out, bound)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Pika request encoding failed"))?;
    if out.len() > bound {
        return Err(invalid("Pika encoded request exceeds limit"));
    }
    Ok((Record { model: Model::PikaFrames, requested: 1, width: 0, height: 0, duration_ms: 0 }, out))
}

/// `seconds` split across `parts` transitions in whole seconds, the first ones a second longer
/// when it doesn't divide; each at least one second.
fn split(seconds: u64, parts: usize) -> Result<Vec<u64>, ProviderError> {
    let n = u64::try_from(parts).map_err(|_| invalid("Pika keyframes need 2 to 5 keyframes"))?;
    if n == 0 || seconds < n {
        return Err(invalid("Pika keyframes need at least a second per transition"));
    }
    let (each, extra) = (seconds / n, seconds % n);
    Ok((0..n).map(|i| each + u64::from(i < extra)).collect())
}
