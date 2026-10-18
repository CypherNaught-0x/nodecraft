use super::*;
use base64::Engine;
use serde::Serialize;
#[derive(Serialize)]
struct Size {
    width: u64,
    height: u64,
}
#[derive(Serialize, Default)]
struct Body<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    negative_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_size: Option<Size>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_images: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_inference_steps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_frames: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frames_per_second: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<Resolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interpolator_model: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_interpolated_frames: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adjust_fps_for_interpolation: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scale: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    face: Option<bool>,
}
#[derive(Serialize)]
#[serde(untagged)]
enum Resolution {
    Video(&'static str),
    Mesh(u16),
}
struct Count {
    n: usize,
    cap: usize,
}
impl std::io::Write for Count {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.n = self.n.checked_add(b.len()).filter(|n| *n <= self.cap).ok_or_else(|| std::io::Error::other("request cap"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn integer(j: &ProviderJob, k: &str, d: u64, min: u64, max: u64) -> Result<u64, ProviderError> {
    let n = j.params.get(k).map(|v| v.as_u64().ok_or_else(|| invalid("fal integer required"))).transpose()?.unwrap_or(d);
    if n < min || n > max {
        return Err(invalid("fal parameter outside supported range"));
    }
    Ok(n)
}
fn choice<'a>(j: &'a ProviderJob, k: &str, d: &'a str, allowed: &[&str]) -> Result<&'a str, ProviderError> {
    let s = j.params.get(k).map(|v| v.as_str().ok_or_else(|| invalid("fal choice required"))).transpose()?.unwrap_or(d);
    if !allowed.contains(&s) {
        return Err(invalid("fal unsupported choice"));
    }
    Ok(s)
}
pub(super) fn prepare(j: &ProviderJob, l: Limits) -> Result<(Record, Vec<u8>), ProviderError> {
    let model = Model::read(&j.model)?;
    if j.inputs.len() > 2 || j.params.len() > 8 || j.idempotency_key.len() > 256 {
        return Err(invalid("fal input count/identity limit"));
    }
    let mut strings = j.model.len() + j.idempotency_key.len();
    let mut prompt = None;
    let mut negative = None;
    let mut media = None;
    for i in &j.inputs {
        match i {
            crate::JobInput::Text { role, text } => {
                strings = strings.checked_add(role.len()).and_then(|n| n.checked_add(text.len())).ok_or_else(|| invalid("fal input size overflow"))?;
                let target = match (model, role.as_str()) {
                    (Model::Image | Model::Video | Model::Audio, "prompt") => &mut prompt,
                    (Model::Video, "negative_prompt") => &mut negative,
                    _ => return Err(invalid("fal unsupported text role")),
                };
                if target.replace(text.as_str()).is_some() {
                    return Err(invalid("fal duplicate input role"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.checked_add(role.len()).and_then(|n| n.checked_add(mime.len())).ok_or_else(|| invalid("fal metadata overflow"))?;
                if !matches!(model, Model::Mesh | Model::Upscale) || role != "image" || media.is_some() || bytes.len() > l.input_bytes {
                    return Err(invalid("fal invalid image input"));
                }
                media = Some((mime.as_str(), bytes.as_ref()));
            }
        }
    }
    for (k, v) in &j.params {
        strings = strings.checked_add(k.len()).and_then(|n| n.checked_add(v.as_str().map_or(0, str::len))).ok_or_else(|| invalid("fal metadata overflow"))?;
        if k.len() > 64 || matches!(v, serde_json::Value::Array(_) | serde_json::Value::Object(_) | serde_json::Value::Null) {
            return Err(invalid("fal unsupported parameter value"));
        }
        let ok = match model {
            Model::Image => matches!(k.as_str(), "width" | "height" | "num_images" | "num_inference_steps" | "seed"),
            Model::Video => matches!(k.as_str(), "num_frames" | "aspect_ratio" | "seed"),
            Model::Audio => matches!(k.as_str(), "voice" | "speed"),
            Model::Mesh => k == "seed",
            Model::Upscale => false,
        };
        if !ok {
            return Err(invalid("fal unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("fal aggregate text metadata exceeds limit"));
    }
    if matches!(model, Model::Image | Model::Video | Model::Audio) && prompt.is_none_or(|s| s.trim().is_empty()) {
        return Err(invalid("fal prompt required"));
    }
    if model == Model::Audio && prompt.is_some_and(|s| s.len() > 4096) {
        return Err(invalid("fal audio prompt exceeds4096bytes"));
    }
    if matches!(model, Model::Mesh | Model::Upscale) && media.is_none() {
        return Err(invalid("fal image required"));
    }
    let mut body = Body { prompt, negative_prompt: negative, ..Default::default() };
    let mut record = Record { model, requested: 1, width: 0, height: 0 };
    if let Some(v) = j.params.get("seed") {
        body.seed = Some(v.as_u64().filter(|n| *n <= u32::MAX as u64).ok_or_else(|| invalid("fal seed must fit u32"))?);
    }
    match model {
        Model::Image => {
            let width = integer(j, "width", 1024, 512, 1024)?;
            let height = integer(j, "height", 1024, 512, 1024)?;
            if width % 64 != 0 || height % 64 != 0 {
                return Err(invalid("fal image dimensions require multiples of64"));
            }
            let output_pixels = width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(integer(j, "num_images", 1, 1, 4).ok()?))
                .ok_or_else(|| invalid("fal requested geometry overflow"))?;
            if width > u64::from(l.output_width) || height > u64::from(l.output_height) || output_pixels > l.total_pixels {
                return Err(invalid("fal requested output exceeds host geometry before submit"));
            }
            body.image_size = Some(Size { width, height });
            body.num_images = Some(integer(j, "num_images", 1, 1, 4)?);
            record.requested = u8::try_from(body.num_images.unwrap_or(1)).map_err(|_| invalid("fal count"))?;
            body.num_inference_steps = Some(integer(j, "num_inference_steps", 4, 1, 4)?);
            body.output_format = Some("png");
            body.sync_mode = Some(false);
        }
        Model::Video => {
            body.num_frames = Some(integer(j, "num_frames", 81, 17, 81)?);
            body.frames_per_second = Some(16);
            body.resolution = Some(Resolution::Video("480p"));
            body.aspect_ratio = Some(choice(j, "aspect_ratio", "16:9", &["16:9", "9:16", "1:1"])?);
            body.interpolator_model = Some("none");
            body.num_interpolated_frames = Some(0);
            body.adjust_fps_for_interpolation = Some(false);
        }
        Model::Audio => {
            body.voice = Some(choice(j, "voice", "af_heart", &["af_heart", "am_adam"])?);
            let speed = j.params.get("speed").map(|v| v.as_f64().ok_or_else(|| invalid("fal numeric speed required"))).transpose()?.unwrap_or(1.0);
            if !speed.is_finite() || !(0.5..=2.0).contains(&speed) {
                return Err(invalid("fal speed outside supported range"));
            }
            body.speed = Some(speed);
        }
        Model::Mesh => body.resolution = Some(Resolution::Mesh(512)),
        Model::Upscale => {
            body.model = Some("RealESRGAN_x4plus");
            body.scale = Some(2);
            body.face = Some(false);
            body.output_format = Some("png");
        }
    }
    // Count the complete escaped JSON and base64 extent BEFORE codecs or owned encodings.
    body.image_url = media.map(|_| "");
    let mut count = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut count, &body).map_err(|_| invalid("fal escaped request exceeds limit"))?;
    let image_len = media
        .map(|(mime, b)| {
            b.len()
                .checked_add(2)
                .and_then(|n| n.checked_div(3))
                .and_then(|n| n.checked_mul(4))
                .and_then(|n| n.checked_add(mime.len() + 13))
                .ok_or_else(|| invalid("fal encoded image overflow"))
        })
        .transpose()?
        .unwrap_or(0);
    let length = count.n.checked_add(image_len).filter(|n| *n <= l.request_bytes).ok_or_else(|| invalid("fal encoded request exceeds limit"))?;
    if let Some((mime, bytes)) = media {
        let m = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("fal input image probe refused"))?;
        if !matches!(m.mime, "image/png" | "image/jpeg" | "image/webp") || m.mime != mime {
            return Err(invalid("fal input MIME mismatch"));
        }
        if model == Model::Upscale {
            record.width = m.width.checked_mul(2).ok_or_else(|| invalid("fal width overflow"))?;
            record.height = m.height.checked_mul(2).ok_or_else(|| invalid("fal height overflow"))?;
        }
        let pixels = u64::from(record.width).checked_mul(u64::from(record.height)).ok_or_else(|| invalid("fal geometry overflow"))?;
        if model == Model::Upscale && (record.width > l.output_width || record.height > l.output_height || pixels > l.total_pixels) {
            return Err(invalid("fal upscale exceeds output geometry before submit"));
        }
        observe(0);
        let decoded = nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("fal input image codec refused"))?;
        drop(decoded);
    }
    let uri = media
        .map(|(mime, bytes)| {
            let mut s = String::new();
            s.try_reserve_exact(image_len).map_err(|_| invalid("fal encoding allocation refused"))?;
            s.push_str("data:");
            s.push_str(mime);
            s.push_str(";base64,");
            observe(1);
            base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut s);
            Ok::<_, ProviderError>(s)
        })
        .transpose()?;
    body.image_url = uri.as_deref();
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("fal request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("fal request extent changed"));
    }
    Ok((record, out))
}
