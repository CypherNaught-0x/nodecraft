//! Luma's three explicit schemas share fal's queue and bounded media admission.
use super::*;

#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    aspect_ratio: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<&'static str>,
    #[serde(rename = "loop", skip_serializing_if = "Option::is_none")]
    looping: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hdr: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exr_export: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_image_url: Option<&'a str>,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    let photon = model == Model::Photon;
    if j.inputs.len() > 3 || j.params.len() > 4 || j.idempotency_key.len() > 256 {
        return Err(invalid("Luma input count or identity exceeds limit"));
    }
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut first = None;
    let mut last = None;
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                strings = strings.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Luma requires one prompt role"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if model != Model::RayFrames || bytes.len() > l.input_bytes {
                    return Err(invalid("Luma media role or input size refused"));
                }
                let target = match role.as_str() {
                    "first_frame" => &mut first,
                    "last_frame" => &mut last,
                    _ => return Err(invalid("Luma requires named first/last frames")),
                };
                if target.replace((mime.as_str(), bytes.as_ref())).is_some() {
                    return Err(invalid("Luma duplicate frame role"));
                }
            }
        }
    }
    for (key, value) in &j.params {
        strings = strings.saturating_add(key.len()).saturating_add(value.as_str().map_or(0, str::len));
        if key.len() > 64
            || matches!(value, serde_json::Value::Array(_) | serde_json::Value::Object(_) | serde_json::Value::Null)
            || !(key == "aspect_ratio" || (!photon && matches!(key.as_str(), "resolution" | "duration_seconds" | "loop")))
        {
            return Err(invalid("Luma unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("Luma text metadata exceeds limit"));
    }
    let prompt = prompt.map(str::trim).ok_or_else(|| invalid("Luma prompt required"))?;
    let count = prompt.chars().count();
    if !(if photon { 3..=5000 } else { 1..=6000 }).contains(&count) {
        return Err(invalid("Luma prompt length outside supported range"));
    }
    if model == Model::RayFrames && first.is_none() {
        return Err(invalid("Luma first frame required"));
    }
    let ratios: &[&str] = if photon { &["16:9", "9:16", "1:1", "4:3", "3:4", "21:9", "9:21"] } else { &["3:4", "4:3", "1:1", "9:16", "16:9", "21:9"] };
    let aspect_ratio = choice(j, "aspect_ratio", if photon { "1:1" } else { "16:9" }, ratios)?;
    let seconds = if photon { 5 } else { integer(j, "duration_seconds", 5, 5, 10)? };
    if !matches!(seconds, 5 | 10) || (model == Model::RayFrames && seconds != 5) {
        return Err(invalid("Luma text video takes 5 or 10 seconds; frame video takes 5"));
    }
    let looping = j.params.get("loop").map(|v| v.as_bool().ok_or_else(|| invalid("Luma loop must be boolean"))).transpose()?.unwrap_or(false);
    if looping && (seconds != 5 || last.is_some()) {
        return Err(invalid("Luma looping requires 5 seconds and no last frame"));
    }
    let resolution = if photon { None } else { Some(choice(j, "resolution", "540p", &["540p", "720p", "1080p"])?) };
    let mut body = Body {
        prompt,
        aspect_ratio,
        resolution,
        duration: (!photon).then_some(if seconds == 5 { "5s" } else { "10s" }),
        looping: (!photon).then_some(looping),
        hdr: (!photon).then_some(false),
        exr_export: (!photon).then_some(false),
        image_url: first.map(|_| ""),
        end_image_url: last.map(|_| ""),
    };
    // Refuse the complete escaped JSON + base64 extent before codecs or owned encodings.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Luma escaped request exceeds limit"))?;
    let first_len = first.map(|(mime, bytes)| encoded(mime, bytes)).transpose()?;
    let last_len = last.map(|(mime, bytes)| encoded(mime, bytes)).transpose()?;
    let length = [first_len, last_len]
        .into_iter()
        .flatten()
        .try_fold(counter.n, usize::checked_add)
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("Luma encoded request exceeds limit"))?;
    for (mime, bytes) in [first, last].into_iter().flatten() {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Luma frame exceeds image limits"))?;
        if meta.mime != mime || !matches!(meta.mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(invalid("Luma frame MIME mismatch"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Luma frame codec refused"))?);
    }
    let first_uri = first.zip(first_len).map(|((mime, bytes), length)| uri(mime, bytes, length)).transpose()?;
    let last_uri = last.zip(last_len).map(|((mime, bytes), length)| uri(mime, bytes, length)).transpose()?;
    body.image_url = first_uri.as_deref();
    body.end_image_url = last_uri.as_deref();
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Luma request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Luma encoded request extent changed"));
    }
    Ok((Record { model, requested: 1, width: 0, height: 0, duration_ms: 0 }, out))
}
