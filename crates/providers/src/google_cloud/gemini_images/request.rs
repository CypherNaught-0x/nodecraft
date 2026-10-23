//! A job checked and shaped into one `generateContent` request, every reference image inline.
//! Nothing is sent for a job this refuses.
use super::*;
use crate::JobInput;
use base64::Engine as _;
use serde_json::{Map, Value, json};

/// The aspect ratios the Gemini 3 image models take.
const ASPECTS: &[&str] = &["1:1", "3:2", "2:3", "3:4", "1:4", "4:1", "4:3", "4:5", "5:4", "1:8", "8:1", "9:16", "16:9", "21:9"];
/// Gemini 2.5 Flash Image's shorter list.
const ASPECTS_2_5: &[&str] = &["1:1", "3:2", "2:3", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9"];

/// What each model takes: its image sizes (none: it has no size setting) and how many references.
struct Model {
    sizes: &'static [&'static str],
    aspects: &'static [&'static str],
    references: usize,
}
fn model(id: &str) -> Option<Model> {
    Some(match id {
        // Google also lists 512 for Flash, but `imageConfig.imageSize` documents 1K, 2K and 4K.
        FLASH | NANO_BANANA_2_1 | PRO => Model { sizes: &["1K", "2K", "4K"], aspects: ASPECTS, references: 14 },
        FLASH_LITE => Model { sizes: &[], aspects: ASPECTS, references: 14 },
        FLASH_2_5 => Model { sizes: &[], aspects: ASPECTS_2_5, references: 3 },
        _ => return None,
    })
}

/// A choice setting, or `None` when it isn't set.
fn choice<'a>(job: &'a ProviderJob, name: &str, allowed: &[&str]) -> Result<Option<&'a str>, ProviderError> {
    match job.params.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if allowed.contains(&s.as_str()) => Ok(Some(s.as_str())),
        Some(_) => Err(invalid(format!("this Gemini image model's {name} is one of {}", allowed.join(", ")))),
    }
}

/// A reference image whose bytes match its type, within the limits, as a request part.
fn image(mime: &str, bytes: &[u8], l: Limits, total: &mut usize) -> Result<Value, ProviderError> {
    if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
        return Err(invalid("Gemini image models take PNG, JPEG or WebP references"));
    }
    if bytes.is_empty() || bytes.len() > l.image_bytes {
        return Err(invalid(format!("a Gemini reference image must be under {} MB", l.image_bytes.div_ceil(1_000_000))));
    }
    if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
        return Err(invalid("a Gemini reference image's bytes don't match its type"));
    }
    *total =
        total.checked_add(bytes.len()).filter(|n| *n <= l.total_image_bytes).ok_or_else(|| invalid("the Gemini reference images are too large together"))?;
    Ok(json!({"inlineData": {"mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes)}}))
}

pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Value, ProviderError> {
    let m = model(&job.model).ok_or_else(|| invalid("Google Cloud has no such Gemini image model in this adapter"))?;
    if let Some(name) = job.params.keys().find(|k| !matches!(k.as_str(), "image_size" | "aspect_ratio")) {
        return Err(invalid(format!("Gemini image models have no setting {name:.32}")));
    }
    let size = if m.sizes.is_empty() {
        // A model without sizes makes 1K images; only that may be asked for.
        choice(job, "image_size", &["1K"])?;
        None
    } else {
        choice(job, "image_size", m.sizes)?
    };
    let aspect = choice(job, "aspect_ratio", m.aspects)?;
    let mut prompt = None;
    let mut images = Vec::new();
    let mut total = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                let text = text.trim();
                if text.is_empty() {
                    return Err(invalid("a Gemini image needs a prompt"));
                }
                if text.chars().take(l.prompt_chars.saturating_add(1)).count() > l.prompt_chars {
                    return Err(invalid(format!("a Gemini image prompt is at most {} characters", l.prompt_chars)));
                }
                prompt = Some(text);
            }
            JobInput::Media { role, mime, bytes } if matches!(role.as_str(), "image" | "reference") => {
                if images.len() >= m.references {
                    return Err(invalid(format!("this Gemini image model takes at most {} reference images", m.references)));
                }
                images.push(image(mime, bytes, l, &mut total)?);
            }
            _ => return Err(invalid("a Gemini image takes one prompt and reference images")),
        }
    }
    let prompt = prompt.ok_or_else(|| invalid("a Gemini image needs a prompt"))?;
    let mut parts = Vec::new();
    parts.try_reserve_exact(images.len().saturating_add(1)).map_err(|_| invalid("the Gemini request is too large"))?;
    parts.push(json!({"text": prompt}));
    parts.extend(images);
    let mut image_config = Map::new();
    if let Some(aspect) = aspect {
        image_config.insert("aspectRatio".into(), aspect.into());
    }
    if let Some(size) = size {
        image_config.insert("imageSize".into(), size.into());
    }
    let mut config = Map::new();
    config.insert("responseModalities".into(), json!(["TEXT", "IMAGE"]));
    config.insert("candidateCount".into(), 1.into());
    if !image_config.is_empty() {
        config.insert("imageConfig".into(), image_config.into());
    }
    Ok(json!({"contents": [{"role": "user", "parts": parts}], "generationConfig": config}))
}
