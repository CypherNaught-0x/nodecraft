use super::*;
use crate::JobInput;

pub(super) struct Prepared {
    pub request: HttpRequest,
    pub model: Model,
    pub duration: u16,
}
fn text<'a>(job: &'a ProviderJob, name: &str, default: &'a str, max: usize) -> Result<&'a str, ProviderError> {
    let value = job.params.get(name).map(|v| v.as_str().ok_or_else(invalid)).transpose()?.unwrap_or(default);
    if value.len() > max {
        return Err(invalid());
    }
    Ok(value)
}
fn choice<'a>(job: &'a ProviderJob, name: &str, default: &'a str, choices: &[&str]) -> Result<&'a str, ProviderError> {
    let value = text(job, name, default, 64)?;
    if !choices.contains(&value) {
        return Err(invalid());
    }
    Ok(value)
}
fn uint(job: &ProviderJob, name: &str, default: u64, min: u64, max: u64) -> Result<u64, ProviderError> {
    let value = job.params.get(name).map(|value| value.as_u64().ok_or_else(invalid)).transpose()?.unwrap_or(default);
    if !(min..=max).contains(&value) {
        return Err(invalid());
    }
    Ok(value)
}
fn number(job: &ProviderJob, name: &str, default: f64, min: f64, max: f64) -> Result<f64, ProviderError> {
    let value = job.params.get(name).map(|value| value.as_f64().ok_or_else(invalid)).transpose()?.unwrap_or(default);
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(invalid());
    }
    Ok(value)
}
pub(super) fn prepare(job: &ProviderJob) -> Result<Prepared, ProviderError> {
    let model = Model::read(&job.model)?;
    if job.inputs.len() != 1 || job.params.len() > 6 || job.idempotency_key.len() > 256 {
        return Err(invalid());
    }
    let allowed: &[&str] = match model {
        Model::Core => &["aspect_ratio", "negative_prompt", "seed", "style_preset", "output_format"],
        Model::Audio => &["duration_seconds", "seed", "steps", "cfg_scale"],
        Model::Mesh => &["texture_resolution", "foreground_ratio", "remesh", "vertex_count"],
    };
    if job.params.keys().any(|name| !allowed.contains(&name.as_str())) {
        return Err(invalid());
    }
    let mut fields: Vec<(&str, String)> = Vec::new();
    let mut duration = 0;
    let mut file = None;
    match (model, job.inputs.as_slice()) {
        (Model::Core | Model::Audio, [JobInput::Text { role, text }]) => {
            if role != "prompt" || text.trim().is_empty() || text.len() > 40_000 || text.chars().count() > 10_000 {
                return Err(invalid());
            }
            fields.push(("prompt", text.clone()));
            fields.push(("seed", uint(job, "seed", 0, 0, 4_294_967_294)?.to_string()));
            if model == Model::Core {
                let negative = self::text(job, "negative_prompt", "", 40_000)?;
                if negative.chars().count() > 10_000 {
                    return Err(invalid());
                }
                fields.push(("negative_prompt", negative.into()));
                fields.push(("aspect_ratio", choice(job, "aspect_ratio", "1:1", &["21:9", "16:9", "3:2", "5:4", "1:1", "4:5", "2:3", "9:16", "9:21"])?.into()));
                fields.push(("output_format", choice(job, "output_format", "png", &["png", "jpeg", "webp"])?.into()));
                if job.params.contains_key("style_preset") {
                    fields.push((
                        "style_preset",
                        choice(
                            job,
                            "style_preset",
                            "photographic",
                            &[
                                "enhance",
                                "anime",
                                "photographic",
                                "digital-art",
                                "comic-book",
                                "fantasy-art",
                                "line-art",
                                "analog-film",
                                "neon-punk",
                                "isometric",
                                "low-poly",
                                "origami",
                                "modeling-compound",
                                "cinematic",
                                "3d-model",
                                "pixel-art",
                                "tile-texture",
                            ],
                        )?
                        .into(),
                    ));
                }
            } else {
                duration = u16::try_from(uint(job, "duration_seconds", 30, 1, 380)?).map_err(|_| invalid())?;
                fields.extend([
                    ("model", AUDIO.into()),
                    ("output_format", "wav".into()),
                    ("duration", duration.to_string()),
                    ("steps", uint(job, "steps", 8, 4, 8)?.to_string()),
                    ("cfg_scale", number(job, "cfg_scale", 1.0, 1.0, 25.0)?.to_string()),
                ]);
            }
        }
        (Model::Mesh, [JobInput::Media { role, mime, bytes }]) => {
            if role != "image" || bytes.is_empty() || bytes.len() > 4 * MIB || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
                return Err(invalid());
            }
            let limits = nodecraft_media::Limits { max_input_bytes: 4 * MIB as u64, max_width: 2048, max_height: 2048, max_alloc: 64 * MIB as u64 };
            let (meta, image) = nodecraft_media::decode_image(bytes, &limits).map_err(|_| invalid())?;
            if meta.mime != mime || meta.width < 64 || meta.height < 64 {
                return Err(invalid());
            }
            drop(image);
            file = Some(("image", "reference", mime.as_str(), bytes.as_ref()));
            fields.push(("texture_resolution", choice(job, "texture_resolution", "1024", &["512", "1024", "2048"])?.into()));
            fields.push(("foreground_ratio", number(job, "foreground_ratio", 0.85, 0.1, 1.0)?.to_string()));
            fields.push(("remesh", choice(job, "remesh", "none", &["none", "triangle", "quad"])?.into()));
            if job.params.contains_key("vertex_count") {
                // Omission preserves the API's -1 default; an exposed explicit count is positive.
                fields.push(("vertex_count", uint(job, "vertex_count", 1000, 1, 20_000)?.to_string()));
            }
        }
        _ => return Err(invalid()),
    }
    let refs: Vec<_> = fields.iter().map(|(name, value)| (*name, value.as_str())).collect();
    let files: Vec<_> = file.into_iter().collect();
    let request = HttpRequest::post_multipart(format!("{API}/{}", model.path()), &refs, &files)?
        .header(
            "Accept",
            match model {
                Model::Core => "image/*",
                Model::Audio => "audio/*",
                Model::Mesh => "model/gltf-binary",
            },
        )
        .max_response_bytes(if model == Model::Audio { METADATA } else { model.cap() });
    let boundary = request
        .headers
        .iter()
        .find(|(name, _)| name == "Content-Type")
        .and_then(|(_, value)| value.strip_prefix("multipart/form-data; boundary="))
        .ok_or_else(invalid)?;
    if fields.iter().any(|(_, value)| value.contains(boundary)) {
        return Err(invalid());
    }
    Ok(Prepared { request, model, duration })
}
