//! Grok Imagine's current image 2.0 and video 1.5 contracts through the saved fal queue.
use super::*;

const IMAGE_RATIOS: &[&str] = &["2:1", "20:9", "19.5:9", "16:9", "4:3", "3:2", "1:1", "2:3", "3:4", "9:16", "9:19.5", "9:20", "1:2", "auto", "21:9", "5:2"];
const VIDEO_RATIOS: &[&str] = &["16:9", "4:3", "3:2", "1:1", "2:3", "3:4", "9:16"];

#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    resolution: &'a str,
    enable_safety_checker: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_images: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    quality: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_urls: Option<Vec<&'a str>>,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    let video = model.video();
    let edit = model == Model::GrokEdit;
    if j.inputs.len() > 6 || j.params.len() > 4 || j.idempotency_key.len() > 256 {
        return Err(invalid("Grok input count or identity exceeds limit"));
    }
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut images = Vec::new();
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                strings = strings.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Grok requires one named prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if !(edit && role == "images" || model == Model::GrokFrames && role == "first_frame") || bytes.len() > l.input_bytes {
                    return Err(invalid("Grok media role or input size refused"));
                }
                images.push((mime.as_str(), bytes.as_ref()));
            }
        }
    }
    if edit && !(1..=5).contains(&images.len()) || model == Model::GrokFrames && images.len() != 1 {
        return Err(invalid("Grok requires one first frame or one to five edit references"));
    }
    for (key, value) in &j.params {
        strings = strings.saturating_add(key.len()).saturating_add(value.as_str().map_or(0, str::len));
        let allowed = if video {
            matches!(key.as_str(), "duration_seconds" | "resolution") || model == Model::GrokText && key == "aspect_ratio"
        } else {
            matches!(key.as_str(), "image_preset" | "num_images" | "aspect_ratio" | "output_format")
        };
        if !allowed {
            return Err(invalid("Grok unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("Grok text metadata exceeds limit"));
    }
    let prompt = prompt.map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| invalid("Grok prompt required"))?;
    if prompt.chars().count() > if video { 4096 } else { 8000 } {
        return Err(invalid("Grok prompt exceeds character limit"));
    }
    let mut body = Body {
        prompt,
        resolution: "1k",
        enable_safety_checker: true,
        aspect_ratio: None,
        duration: None,
        num_images: None,
        quality: None,
        output_format: None,
        image_url: (model == Model::GrokFrames).then_some(""),
        image_urls: edit.then(|| vec![""; images.len()]),
    };
    let requested = if video {
        body.duration = Some(integer(j, "duration_seconds", 6, 1, 15)?);
        body.resolution = choice(j, "resolution", "720p", &["480p", "720p", "1080p"])?;
        if model == Model::GrokText {
            body.aspect_ratio = Some(choice(j, "aspect_ratio", "16:9", VIDEO_RATIOS)?);
        }
        1
    } else {
        let preset = choice(j, "image_preset", "medium-1k", &["low-1k", "low-2k", "medium-1k", "medium-2k"])?;
        let (quality, resolution) = preset.split_once('-').ok_or_else(|| invalid("Grok image preset refused"))?;
        body.quality = Some(quality);
        body.resolution = resolution;
        body.aspect_ratio = Some(choice(j, "aspect_ratio", if edit { "auto" } else { "1:1" }, IMAGE_RATIOS)?);
        body.output_format = Some(choice(j, "output_format", "jpeg", &["jpeg", "png", "webp"])?);
        let count = integer(j, "num_images", 1, 1, 10)?;
        body.num_images = Some(count);
        u8::try_from(count).map_err(|_| invalid("Grok output count refused"))?
    };
    // Count the entire escaped request before image codecs or data-URI allocations.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Grok escaped request exceeds limit"))?;
    let lengths: Vec<usize> = images.iter().map(|(mime, bytes)| encoded(mime, bytes)).collect::<Result<_, _>>()?;
    let length = lengths
        .iter()
        .try_fold(counter.n, |n, size| n.checked_add(*size))
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("Grok encoded request exceeds limit"))?;
    for (mime, bytes) in &images {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Grok reference exceeds image limits"))?;
        if meta.mime != *mime || !matches!(*mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(invalid("Grok reference MIME mismatch"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Grok reference codec refused"))?);
    }
    let uris: Vec<String> = images.iter().zip(lengths).map(|((mime, bytes), size)| uri(mime, bytes, size)).collect::<Result<_, _>>()?;
    body.image_url = (model == Model::GrokFrames).then(|| uris.first().map(String::as_str)).flatten();
    body.image_urls = edit.then(|| uris.iter().map(String::as_str).collect());
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Grok request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Grok encoded request extent changed"));
    }
    Ok((Record { model, requested, width: 0, height: 0, duration_ms: 0 }, out))
}
