//! LTX Pro's explicit schema uses the existing saved queue and bounded frame admission.
use super::*;

#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    duration: u64,
    resolution: &'a str,
    aspect_ratio: &'a str,
    fps: u64,
    generate_audio: bool,
    enable_safety_checker: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_image_url: Option<&'a str>,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() > 3 || j.params.len() > 5 || j.idempotency_key.len() > 256 {
        return Err(invalid("LTX input count or identity exceeds limit"));
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
                    return Err(invalid("LTX requires one named prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if model != Model::LtxFrames || bytes.len() > l.input_bytes {
                    return Err(invalid("LTX media role or input size refused"));
                }
                let target = match role.as_str() {
                    "first_frame" => &mut first,
                    "last_frame" => &mut last,
                    _ => return Err(invalid("LTX requires named first/last frames")),
                };
                if target.replace((mime.as_str(), bytes.as_ref())).is_some() {
                    return Err(invalid("LTX duplicate frame role"));
                }
            }
        }
    }
    for (key, value) in &j.params {
        strings = strings.saturating_add(key.len()).saturating_add(value.as_str().map_or(0, str::len));
        if !matches!(key.as_str(), "duration_seconds" | "resolution" | "aspect_ratio" | "fps" | "generate_audio") {
            return Err(invalid("LTX unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("LTX text metadata exceeds limit"));
    }
    let prompt = prompt.map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| invalid("LTX prompt required"))?;
    if prompt.chars().count() > 5000 {
        return Err(invalid("LTX prompt exceeds 5000 characters"));
    }
    if model == Model::LtxFrames && first.is_none() {
        return Err(invalid("LTX first frame required"));
    }
    let duration = integer(j, "duration_seconds", 6, 6, 10)?;
    let fps = choice(j, "fps", "25", &["24", "25", "48", "50"])?.parse::<u64>().map_err(|_| invalid("LTX frame rate must be a supported choice"))?;
    if !matches!(duration, 6 | 8 | 10) {
        return Err(invalid("LTX duration or frame rate is outside the supported choices"));
    }
    let resolution = choice(j, "resolution", "1080p", &["1080p", "1440p", "2160p"])?;
    let aspect_ratio = choice(j, "aspect_ratio", "16:9", &["16:9", "9:16"])?;
    let (width, height) = match resolution {
        "1080p" => (1920u32, 1080u32),
        "1440p" => (2560, 1440),
        _ => (3840, 2160),
    };
    let (width, height) = if aspect_ratio == "9:16" { (height, width) } else { (width, height) };
    if width > l.output_width || height > l.output_height || u64::from(width).saturating_mul(u64::from(height)) > l.total_pixels {
        return Err(invalid("LTX requested output exceeds host geometry limits"));
    }
    let generate_audio =
        j.params.get("generate_audio").map(|v| v.as_bool().ok_or_else(|| invalid("LTX audio switch must be boolean"))).transpose()?.unwrap_or(true);
    let mut body = Body {
        prompt,
        duration,
        resolution,
        aspect_ratio,
        fps,
        generate_audio,
        enable_safety_checker: true,
        image_url: first.map(|_| ""),
        end_image_url: last.map(|_| ""),
    };
    // Admit escaped JSON and both encoded images before codecs or base64 allocations.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("LTX escaped request exceeds limit"))?;
    let first_len = first.map(|(mime, bytes)| encoded(mime, bytes)).transpose()?;
    let last_len = last.map(|(mime, bytes)| encoded(mime, bytes)).transpose()?;
    let length = [first_len, last_len]
        .into_iter()
        .flatten()
        .try_fold(counter.n, usize::checked_add)
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("LTX encoded request exceeds limit"))?;
    for (mime, bytes) in [first, last].into_iter().flatten() {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("LTX frame exceeds image limits"))?;
        if meta.mime != mime || !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(invalid("LTX frame MIME mismatch"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("LTX frame codec refused"))?);
    }
    let first_uri = first.zip(first_len).map(|((mime, bytes), length)| uri(mime, bytes, length)).transpose()?;
    let last_uri = last.zip(last_len).map(|((mime, bytes), length)| uri(mime, bytes, length)).transpose()?;
    body.image_url = first_uri.as_deref();
    body.end_image_url = last_uri.as_deref();
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("LTX request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("LTX encoded request extent changed"));
    }
    Ok((Record { model, requested: 1, width: 0, height: 0 }, out))
}
