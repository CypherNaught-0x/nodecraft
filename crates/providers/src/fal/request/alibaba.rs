//! Qwen-Image 3 and Wan 3.0 use separate, bounded request contracts and explicit priced sizes.
use super::*;

const VIDEO_RATIOS: &[&str] = &["adaptive", "16:9", "4:3", "1:1", "3:4", "9:16"];
const MAX_SEED: u64 = 2_147_483_647;

#[derive(Serialize)]
struct QwenBody<'a> {
    prompt: &'a str,
    negative_prompt: &'a str,
    image_size: Size,
    num_images: u64,
    output_format: &'a str,
    enable_prompt_expansion: bool,
    enable_safety_checker: bool,
    sync_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_urls: Option<Vec<&'a str>>,
}

#[derive(Serialize)]
struct WanBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<&'a str>,
    duration: u64,
    resolution: &'a str,
    aspect_ratio: &'a str,
    audio: bool,
    enable_prompt_expansion: bool,
    enable_thinking: bool,
    enable_safety_checker: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    end_image_url: Option<&'a str>,
}

fn boolean(j: &ProviderJob, name: &str, default: bool) -> Result<bool, ProviderError> {
    j.params.get(name).map(|v| v.as_bool().ok_or_else(|| invalid("Alibaba boolean required"))).transpose().map(|v| v.unwrap_or(default))
}

fn seed(j: &ProviderJob) -> Result<Option<u64>, ProviderError> {
    j.params.get("seed").map(|v| v.as_u64().filter(|n| *n <= MAX_SEED).ok_or_else(|| invalid("Alibaba seed must be 0 to 2147483647"))).transpose()
}

fn negative_prompt(j: &ProviderJob) -> Result<&str, ProviderError> {
    let text = j.params.get("negative_prompt").map(|v| v.as_str().ok_or_else(|| invalid("Qwen negative prompt must be text"))).transpose()?.unwrap_or("");
    if text.chars().count() > 500 {
        return Err(invalid("Qwen negative prompt exceeds 500 characters"));
    }
    Ok(text)
}

/// Count placeholders and every complete data URI before any codec or base64 allocation.
fn extent(body: &impl Serialize, images: &[(&str, &[u8])], l: Limits) -> Result<(usize, Vec<usize>), ProviderError> {
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, body).map_err(|_| invalid("Alibaba escaped request exceeds limit"))?;
    let lengths: Vec<usize> = images.iter().map(|(mime, bytes)| encoded(mime, bytes)).collect::<Result<_, _>>()?;
    let size = lengths
        .iter()
        .try_fold(counter.n, |n, size| n.checked_add(*size))
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("Alibaba encoded request exceeds limit"))?;
    Ok((size, lengths))
}

fn verify_images(images: &[(&str, &[u8])], qwen_edit: bool, l: Limits) -> Result<(), ProviderError> {
    for (mime, bytes) in images {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Alibaba reference exceeds image limits"))?;
        if meta.mime != *mime || !matches!(*mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(invalid("Alibaba reference MIME mismatch"));
        }
        if qwen_edit && (meta.width < 384 || meta.height < 384 || meta.mime == "image/png" && meta.has_alpha) {
            return Err(invalid("Qwen edit references require at least 384 pixels per axis and PNG without an alpha channel"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Alibaba reference codec refused"))?);
    }
    Ok(())
}

fn encode(body: &impl Serialize, size: usize) -> Result<Vec<u8>, ProviderError> {
    let mut bytes = Vec::new();
    reserve(&mut bytes, size)?;
    serde_json::to_writer(&mut bytes, body).map_err(|_| invalid("Alibaba request encoding failed"))?;
    if bytes.len() != size {
        return Err(invalid("Alibaba encoded request extent changed"));
    }
    Ok(bytes)
}

pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    let qwen = matches!(model, Model::QwenImage | Model::QwenEdit);
    let edit = model == Model::QwenEdit;
    let frames = model == Model::Wan3Frames;
    let max_inputs = if edit {
        4
    } else if frames {
        3
    } else {
        1
    };
    let max_params = if qwen { 6 } else { 7 };
    if j.inputs.len() > max_inputs || j.params.len() > max_params || j.idempotency_key.len() > 256 {
        return Err(invalid("Alibaba input count or identity exceeds limit"));
    }
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut images = Vec::new();
    let mut first = None;
    let mut last = None;
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                strings = strings.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Alibaba requires at most one named prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if bytes.len() > l.input_bytes {
                    return Err(invalid("Alibaba reference exceeds input byte limit"));
                }
                let value = (mime.as_str(), bytes.as_ref());
                if edit && role == "images" {
                    images.push(value);
                } else if frames && role == "first_frame" && first.is_none() {
                    first = Some(value);
                } else if frames && role == "last_frame" && last.is_none() {
                    last = Some(value);
                } else {
                    return Err(invalid("Alibaba media role or count refused"));
                }
            }
        }
    }
    for (name, value) in &j.params {
        strings = strings.saturating_add(name.len()).saturating_add(value.as_str().map_or(0, str::len));
        let allowed = if qwen {
            matches!(name.as_str(), "image_preset" | "num_images" | "output_format" | "negative_prompt" | "enable_prompt_expansion" | "seed")
        } else {
            matches!(name.as_str(), "duration_seconds" | "resolution" | "aspect_ratio" | "audio" | "enable_prompt_expansion" | "enable_thinking" | "seed")
        };
        if !allowed {
            return Err(invalid("Alibaba unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("Alibaba text metadata exceeds limit"));
    }
    let prompt = prompt.map(str::trim);
    if !frames && prompt.is_none_or(str::is_empty) {
        return Err(invalid("Alibaba prompt required"));
    }
    if prompt.is_some_and(|text| text.chars().count() > if qwen { 5000 } else { 20000 }) {
        return Err(invalid("Alibaba prompt exceeds character limit"));
    }
    if edit && !(1..=3).contains(&images.len()) {
        return Err(invalid("Qwen edit requires one to three ordered image references"));
    }
    if frames && first.is_none() {
        return Err(invalid("Wan first frame required before an optional last frame"));
    }
    let seed = seed(j)?;
    if qwen {
        let prompt = prompt.ok_or_else(|| invalid("Qwen prompt required"))?;
        let side = match choice(j, "image_preset", "1k-square", &["1k-square", "2k-square"])? {
            "1k-square" => 1024u64,
            "2k-square" => 2048u64,
            _ => return Err(invalid("Qwen image preset refused")),
        };
        let count = integer(j, "num_images", 1, 1, 6)?;
        let pixels = side.checked_mul(side).and_then(|n| n.checked_mul(count)).ok_or_else(|| invalid("Qwen image extent overflow"))?;
        if side > u64::from(l.output_width) || side > u64::from(l.output_height) || pixels > l.total_pixels {
            return Err(invalid("Qwen requested images exceed host geometry limits"));
        }
        let mut body = QwenBody {
            prompt,
            negative_prompt: negative_prompt(j)?,
            image_size: Size { width: side, height: side },
            num_images: count,
            output_format: choice(j, "output_format", "png", &["png", "jpeg", "webp"])?,
            enable_prompt_expansion: boolean(j, "enable_prompt_expansion", true)?,
            enable_safety_checker: true,
            sync_mode: false,
            seed,
            image_urls: edit.then(|| vec![""; images.len()]),
        };
        let (size, lengths) = extent(&body, &images, l)?;
        verify_images(&images, edit, l)?;
        let uris: Vec<String> = images.iter().zip(lengths).map(|((mime, bytes), size)| uri(mime, bytes, size)).collect::<Result<_, _>>()?;
        body.image_urls = edit.then(|| uris.iter().map(String::as_str).collect());
        let requested = u8::try_from(count).map_err(|_| invalid("Qwen image count refused"))?;
        Ok((Record { model, requested, width: 0, height: 0 }, encode(&body, size)?))
    } else {
        images.extend(first);
        images.extend(last);
        let mut body = WanBody {
            prompt,
            duration: integer(j, "duration_seconds", 5, 2, 30)?,
            resolution: choice(j, "resolution", "720p", &["480p", "720p", "1080p"])?,
            aspect_ratio: choice(j, "aspect_ratio", "16:9", VIDEO_RATIOS)?,
            audio: boolean(j, "audio", true)?,
            enable_prompt_expansion: boolean(j, "enable_prompt_expansion", true)?,
            enable_thinking: boolean(j, "enable_thinking", false)?,
            enable_safety_checker: true,
            seed,
            start_image_url: first.map(|_| ""),
            end_image_url: last.map(|_| ""),
        };
        let (size, lengths) = extent(&body, &images, l)?;
        verify_images(&images, false, l)?;
        let uris: Vec<String> = images.iter().zip(lengths).map(|((mime, bytes), size)| uri(mime, bytes, size)).collect::<Result<_, _>>()?;
        body.start_image_url = first.and_then(|_| uris.first().map(String::as_str));
        body.end_image_url = last.and_then(|_| uris.get(1).map(String::as_str));
        Ok((Record { model, requested: 1, width: 0, height: 0 }, encode(&body, size)?))
    }
}
