use super::*;
use base64::Engine;
use serde::Serialize;
mod alibaba;
mod grok;
mod hunyuan;
mod ideogram;
mod ltx;
mod luma;
mod rodin;
mod topaz;
#[derive(Serialize)]
struct Size {
    width: u64,
    height: u64,
}
#[derive(Serialize)]
struct Lora<'a> {
    path: &'a str,
    scale: f64,
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
    end_image_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio_url: Option<&'a str>,
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
    resolution: Option<Resolution<'a>>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    guidance_scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    loras: Option<[Lora<'a>; 1]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    images_data_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trigger_word: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    steps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_style: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    create_masks: Option<bool>,
}
const ZIP: &str = "application/zip";
/// Most bytes a stored zip entry adds to its image: its local and central headers, named
/// `NN.webp` at most, with room to spare.
const ZIP_ENTRY: usize = 128;
/// The zip's end record.
const ZIP_END: usize = 64;
/// Fewest and most images a training takes: fal recommends at least four.
pub(super) const DATASET: std::ops::RangeInclusive<usize> = 4..=12;
/// Whether `s` can be a trigger word: one short line, empty for none.
pub(super) fn trigger_word(s: &str) -> bool {
    s.len() <= 64 && !s.chars().any(char::is_control) && s.trim() == s
}
/// A training's trigger word; empty for none, and for every other model.
pub(super) fn trigger(j: &ProviderJob) -> Result<&str, ProviderError> {
    if j.model != FLUX_LORA_TRAINER {
        return Ok("");
    }
    // Spaces around a typed word are dropped rather than refused.
    let s = j.params.get("trigger_word").map(|v| v.as_str().ok_or_else(|| invalid("fal trigger word must be text"))).transpose()?.unwrap_or_default().trim();
    if !trigger_word(s) {
        return Err(invalid("fal trigger word is one line of at most 64 bytes"));
    }
    Ok(s)
}
fn number(j: &ProviderJob, k: &str, d: f64, min: f64, max: f64) -> Result<f64, ProviderError> {
    let n = j.params.get(k).map(|v| v.as_f64().ok_or_else(|| invalid("fal number required"))).transpose()?.unwrap_or(d);
    if !n.is_finite() || n < min || n > max {
        return Err(invalid("fal parameter outside supported range"));
    }
    Ok(n)
}
/// The training images as one uncompressed zip, named in order so fal reads them all.
pub(super) fn dataset(images: &[(&str, &[u8])], cap: usize) -> Result<Vec<u8>, ProviderError> {
    use std::io::Write;
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut out = Vec::new();
    reserve(&mut out, cap)?;
    let mut zip = ZipWriter::new(std::io::Cursor::new(out));
    for (n, (mime, bytes)) in images.iter().enumerate() {
        let extension = match *mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => "webp",
        };
        zip.start_file(format!("{n:02}.{extension}"), options).map_err(|_| invalid("fal training images can't be packed"))?;
        zip.write_all(bytes).map_err(|_| invalid("fal training images can't be packed"))?;
    }
    let out = zip.finish().map_err(|_| invalid("fal training images can't be packed"))?.into_inner();
    if out.len() > cap {
        return Err(invalid("fal training images exceed limit"));
    }
    Ok(out)
}
#[derive(Serialize)]
#[serde(untagged)]
enum Resolution<'a> {
    Video(&'a str),
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
/// A media input's data URI length: the `data:<mime>;base64,` prefix and the padded base64.
fn encoded(mime: &str, b: &[u8]) -> Result<usize, ProviderError> {
    encoded_len(mime, b.len())
}
fn encoded_len(mime: &str, n: usize) -> Result<usize, ProviderError> {
    n.checked_add(2)
        .and_then(|n| n.checked_div(3))
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| mime.len().checked_add(13).and_then(|prefix| n.checked_add(prefix)))
        .ok_or_else(|| invalid("fal encoded media overflow"))
}
fn uri(mime: &str, bytes: &[u8], len: usize) -> Result<String, ProviderError> {
    let mut s = String::new();
    s.try_reserve_exact(len).map_err(|_| invalid("fal encoding allocation refused"))?;
    s.push_str("data:");
    s.push_str(mime);
    s.push_str(";base64,");
    observe(1);
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut s);
    Ok(s)
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
    let l = l.for_model(model);
    if model == Model::Topaz {
        return topaz::prepare(j, l);
    }
    if model.rodin() {
        return rodin::prepare(j, l, model);
    }
    if model.alibaba() {
        return alibaba::prepare(j, l, model);
    }
    if model.hunyuan() {
        return hunyuan::prepare(j, l, model);
    }
    if model.grok() {
        return grok::prepare(j, l, model);
    }
    if matches!(model, Model::LtxText | Model::LtxFrames) {
        return ltx::prepare(j, l, model);
    }
    if model == Model::Ideogram {
        return ideogram::prepare(j, l);
    }
    if matches!(model, Model::RayText | Model::RayFrames | Model::Photon) {
        return luma::prepare(j, l, model);
    }
    let most = if model == Model::Train { *DATASET.end() } else { 4 };
    if j.inputs.len() > most || j.params.len() > 8 || j.idempotency_key.len() > 256 {
        return Err(invalid("fal input count/identity limit"));
    }
    let mut strings = j.model.len() + j.idempotency_key.len();
    let mut prompt = None;
    let mut negative = None;
    // The image (frame, portrait or source), the last frame, and the speech track.
    let mut image: Option<(&str, &[u8])> = None;
    let mut end: Option<(&str, &[u8])> = None;
    let mut audio: Option<(&str, &[u8])> = None;
    let mut style: Option<(&str, &[u8])> = None;
    let mut images: Vec<(&str, &[u8])> = Vec::new();
    for i in &j.inputs {
        match i {
            crate::JobInput::Text { role, text } => {
                strings = strings.checked_add(role.len()).and_then(|n| n.checked_add(text.len())).ok_or_else(|| invalid("fal input size overflow"))?;
                let target = match (model, role.as_str()) {
                    (Model::Image | Model::Styled | Model::Video | Model::Frames | Model::Speech | Model::Audio, "prompt") => &mut prompt,
                    (Model::Video | Model::Frames | Model::Speech, "negative_prompt") => &mut negative,
                    _ => return Err(invalid("fal unsupported text role")),
                };
                if target.replace(text.as_str()).is_some() {
                    return Err(invalid("fal duplicate input role"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.checked_add(role.len()).and_then(|n| n.checked_add(mime.len())).ok_or_else(|| invalid("fal metadata overflow"))?;
                if bytes.len() > l.input_bytes {
                    return Err(invalid("fal media input exceeds limit"));
                }
                if (model, role.as_str()) == (Model::Train, "images") {
                    reserve(&mut images, 1)?;
                    images.push((mime.as_str(), bytes.as_ref()));
                    continue;
                }
                let target = match (model, role.as_str()) {
                    (Model::Mesh | Model::Upscale | Model::RemoveBackground, "image") | (Model::Frames, "first_frame") | (Model::Speech, "portrait") => {
                        &mut image
                    }
                    (Model::Frames, "last_frame") => &mut end,
                    (Model::Speech, "speech") => &mut audio,
                    (Model::Styled, "style") => &mut style,
                    _ => return Err(invalid("fal unsupported media role")),
                };
                if target.replace((mime.as_str(), bytes.as_ref())).is_some() {
                    return Err(invalid("fal duplicate input role"));
                }
            }
        }
    }
    for (k, v) in &j.params {
        strings = strings.checked_add(k.len()).and_then(|n| n.checked_add(v.as_str().map_or(0, str::len))).ok_or_else(|| invalid("fal metadata overflow"))?;
        if k.len() > 64 || matches!(v, serde_json::Value::Array(_) | serde_json::Value::Object(_) | serde_json::Value::Null) {
            return Err(invalid("fal unsupported parameter value"));
        }
        let ok = match model {
            Model::RodinText
            | Model::RodinImages
            | Model::RodinFastText
            | Model::RodinFastImages
            | Model::QwenImage
            | Model::QwenEdit
            | Model::Wan3Text
            | Model::Wan3Frames
            | Model::HunyuanText
            | Model::HunyuanImage
            | Model::HunyuanSketch
            | Model::GrokImage
            | Model::GrokEdit
            | Model::GrokText
            | Model::GrokFrames
            | Model::RayText
            | Model::RayFrames
            | Model::Photon
            | Model::Ideogram
            | Model::LtxText
            | Model::LtxFrames
            | Model::Topaz => false,
            Model::Image => matches!(k.as_str(), "width" | "height" | "num_images" | "num_inference_steps" | "seed"),
            Model::Styled => matches!(k.as_str(), "width" | "height" | "num_images" | "num_inference_steps" | "guidance_scale" | "lora_scale" | "seed"),
            Model::Train => matches!(k.as_str(), "steps" | "trigger_word"),
            Model::Video => matches!(k.as_str(), "num_frames" | "aspect_ratio" | "seed"),
            Model::Frames => matches!(k.as_str(), "num_frames" | "resolution" | "aspect_ratio" | "seed"),
            Model::Speech => matches!(k.as_str(), "num_frames" | "resolution" | "seed"),
            Model::Audio => matches!(k.as_str(), "voice" | "speed"),
            Model::Mesh => k == "seed",
            Model::Upscale | Model::RemoveBackground => false,
        };
        if !ok {
            return Err(invalid("fal unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("fal aggregate text metadata exceeds limit"));
    }
    if matches!(model, Model::Image | Model::Styled | Model::Video | Model::Frames | Model::Speech | Model::Audio) && prompt.is_none_or(|s| s.trim().is_empty())
    {
        return Err(invalid("fal prompt required"));
    }
    if model == Model::Audio && prompt.is_some_and(|s| s.len() > 4096) {
        return Err(invalid("fal audio prompt exceeds 4096 bytes"));
    }
    if matches!(model, Model::Mesh | Model::Upscale | Model::RemoveBackground | Model::Frames | Model::Speech) && image.is_none() {
        return Err(invalid("fal image required"));
    }
    if model == Model::Speech && audio.is_none() {
        return Err(invalid("fal speech audio required"));
    }
    if model == Model::Train && !DATASET.contains(&images.len()) {
        return Err(invalid("fal style training takes 4 to 12 images"));
    }
    // A style is fal's own FLUX LoRA, named by a link to fal's storage: another provider's
    // weights, or another family's, are refused before anything is sent.
    let style = match style {
        Some((mime, bytes)) => {
            let style =
                nodecraft_media::style::decode(bytes).ok().filter(|_| mime == nodecraft_media::style::MIME).ok_or_else(|| invalid("fal style is damaged"))?;
            if style.provider != "fal" || style.trainer != FLUX_LORA_TRAINER || style.family != STYLE_FAMILY {
                return Err(invalid("fal takes only FLUX styles trained by fal"));
            }
            response::storage(&style.weights).map_err(|_| invalid("fal style weights aren't on fal's storage"))?;
            Some(style)
        }
        None if model == Model::Styled => return Err(invalid("fal style required")),
        None => None,
    };
    // The trigger word calls the style up; a prompt that already says it is left alone.
    let styled = style.as_ref().and_then(|s| {
        let prompt = prompt?;
        (!s.trigger.is_empty() && !prompt.contains(s.trigger.as_str())).then(|| format!("{}, {prompt}", s.trigger))
    });
    let prompt = styled.as_deref().or(prompt);
    let mut body = Body { prompt, negative_prompt: negative, ..Default::default() };
    let mut record = Record { model, requested: 1, width: 0, height: 0 };
    if let Some(v) = j.params.get("seed") {
        body.seed = Some(v.as_u64().filter(|n| *n <= u32::MAX as u64).ok_or_else(|| invalid("fal seed must fit u32"))?);
    }
    match model {
        Model::RodinText
        | Model::RodinImages
        | Model::RodinFastText
        | Model::RodinFastImages
        | Model::QwenImage
        | Model::QwenEdit
        | Model::Wan3Text
        | Model::Wan3Frames
        | Model::HunyuanText
        | Model::HunyuanImage
        | Model::HunyuanSketch
        | Model::GrokImage
        | Model::GrokEdit
        | Model::GrokText
        | Model::GrokFrames
        | Model::RayText
        | Model::RayFrames
        | Model::Photon
        | Model::Ideogram
        | Model::LtxText
        | Model::LtxFrames
        | Model::Topaz => {
            return Err(invalid("fal model requests require their own schema"));
        }
        Model::Image | Model::Styled => {
            let width = integer(j, "width", 1024, 512, 1024)?;
            let height = integer(j, "height", 1024, 512, 1024)?;
            if width % 64 != 0 || height % 64 != 0 {
                return Err(invalid("fal image dimensions require multiples of 64"));
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
            body.num_inference_steps =
                Some(if model == Model::Styled { integer(j, "num_inference_steps", 28, 1, 50)? } else { integer(j, "num_inference_steps", 4, 1, 4)? });
            body.output_format = Some("png");
            body.sync_mode = Some(false);
            if let Some(style) = &style {
                body.guidance_scale = Some(number(j, "guidance_scale", 3.5, 1.0, 20.0)?);
                body.loras = Some([Lora { path: &style.weights, scale: number(j, "lora_scale", 1.0, 0.0, 2.0)? }]);
            }
        }
        Model::Train => {
            body.steps = Some(integer(j, "steps", 1000, 1, 5000)?);
            body.trigger_word = Some(trigger(j)?).filter(|s| !s.is_empty());
            body.is_style = Some(true);
            body.create_masks = Some(false);
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
        Model::Frames => {
            body.num_frames = Some(integer(j, "num_frames", 81, 17, 161)?);
            body.frames_per_second = Some(16);
            body.resolution = Some(Resolution::Video(choice(j, "resolution", "480p", &["480p", "580p", "720p"])?));
            body.aspect_ratio = Some(choice(j, "aspect_ratio", "auto", &["auto", "16:9", "9:16", "1:1"])?);
            body.interpolator_model = Some("none");
            body.num_interpolated_frames = Some(0);
            body.adjust_fps_for_interpolation = Some(false);
        }
        Model::Speech => {
            let frames = integer(j, "num_frames", 80, 40, 120)?;
            if frames % 4 != 0 {
                return Err(invalid("fal speech-to-video frames require a multiple of 4"));
            }
            body.num_frames = Some(frames);
            body.frames_per_second = Some(16);
            body.resolution = Some(Resolution::Video(choice(j, "resolution", "480p", &["480p", "580p", "720p"])?));
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
        // Prompt-guided removal is a different price. This image-only mode accepts no text
        // roles or parameters, and keeps URL delivery through the ordinary bounded queue.
        Model::RemoveBackground => body.sync_mode = Some(false),
    }
    // Count the complete escaped JSON and base64 extent BEFORE codecs or owned encodings.
    body.image_url = image.map(|_| "");
    body.end_image_url = end.map(|_| "");
    body.audio_url = audio.map(|_| "");
    body.images_data_url = (model == Model::Train).then_some("");
    let mut count = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut count, &body).map_err(|_| invalid("fal escaped request exceeds limit"))?;
    let image_len = image.map(|(mime, b)| encoded(mime, b)).transpose()?;
    let end_len = end.map(|(mime, b)| encoded(mime, b)).transpose()?;
    let audio_len = audio.map(|(mime, b)| encoded(mime, b)).transpose()?;
    let mut length = [image_len, end_len, audio_len]
        .into_iter()
        .flatten()
        .try_fold(count.n, usize::checked_add)
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("fal encoded request exceeds limit"))?;
    // A dataset's zip is at most its images plus fixed headers, so one that won't fit is refused
    // before any image is decoded, and packing never outgrows its reservation.
    let zipped = images
        .iter()
        .try_fold(ZIP_END, |n, (_, b)| n.checked_add(b.len()).and_then(|n| n.checked_add(ZIP_ENTRY)))
        .ok_or_else(|| invalid("fal training images exceed limit"))?;
    if model == Model::Train
        && (zipped > l.dataset_bytes || encoded_len(ZIP, zipped).ok().and_then(|n| length.checked_add(n)).is_none_or(|n| n > l.training_request_bytes))
    {
        return Err(invalid("fal training images exceed limit"));
    }
    // The speech header is cheap, so it is checked before any image codec runs. Only the header:
    // the track itself isn't decoded.
    if let Some((mime, bytes)) = audio {
        let sniffed = nodecraft_media::sniff(bytes).filter(|m| m.kind == nodecraft_media::MediaKind::Audio);
        if !sniffed.is_some_and(|m| matches!(m.mime, "audio/wav" | "audio/mpeg" | "audio/ogg" | "audio/mp4") && m.mime == mime) {
            return Err(invalid("fal speech audio must be WAV, MP3, Ogg or M4A matching its declared type"));
        }
    }
    for (mime, bytes) in [image, end].into_iter().flatten().chain(images.iter().copied()) {
        let m = nodecraft_media::probe_image(bytes, &l.image(true))
            .map_err(|_| invalid("fal input images must be PNG, JPEG or WebP, at most 1 MiB and 1024×1024 px"))?;
        if !matches!(m.mime, "image/png" | "image/jpeg" | "image/webp") || m.mime != mime {
            return Err(invalid("fal input MIME mismatch"));
        }
        if model == Model::Upscale {
            record.width = m.width.checked_mul(2).ok_or_else(|| invalid("fal width overflow"))?;
            record.height = m.height.checked_mul(2).ok_or_else(|| invalid("fal height overflow"))?;
        } else if model == Model::RemoveBackground {
            // The service receives the original bytes; its treatment of EXIF is unverified.
            // Refuse before payment rather than risk rejecting a charged result's dimensions.
            if nodecraft_media::image_requires_orientation(bytes, &l.image(true)).map_err(|_| invalid("fal input image orientation refused"))? {
                return Err(invalid("Bria needs an upright image: apply its EXIF rotation or reflection and save it before removing the background"));
            }
            record.width = m.width;
            record.height = m.height;
        }
        let pixels = u64::from(record.width).checked_mul(u64::from(record.height)).ok_or_else(|| invalid("fal geometry overflow"))?;
        if matches!(model, Model::Upscale | Model::RemoveBackground)
            && (record.width > l.output_width || record.height > l.output_height || pixels > l.total_pixels)
        {
            return Err(invalid("fal image operation exceeds output geometry before submit"));
        }
        observe(0);
        let decoded = nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("fal input image codec refused"))?;
        drop(decoded);
    }
    let image_uri = image.zip(image_len).map(|((mime, b), n)| uri(mime, b, n)).transpose()?;
    let end_uri = end.zip(end_len).map(|((mime, b), n)| uri(mime, b, n)).transpose()?;
    let audio_uri = audio.zip(audio_len).map(|((mime, b), n)| uri(mime, b, n)).transpose()?;
    body.image_url = image_uri.as_deref();
    body.end_image_url = end_uri.as_deref();
    body.audio_url = audio_uri.as_deref();
    let dataset_uri = if model == Model::Train {
        let zip = dataset(&images, zipped)?;
        let n = encoded(ZIP, &zip)?;
        length = length.checked_add(n).filter(|n| *n <= l.training_request_bytes).ok_or_else(|| invalid("fal training images exceed limit"))?;
        Some(uri(ZIP, &zip, n)?)
    } else {
        None
    };
    body.images_data_url = dataset_uri.as_deref();
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("fal request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("fal request extent changed"));
    }
    Ok((record, out))
}
