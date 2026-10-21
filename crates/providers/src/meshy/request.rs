//! A job checked and shaped into Meshy's task body, every input inline as a data URI. Nothing
//! here touches the network.
use super::*;
use crate::JobInput;
use base64::Engine as _;
use serde_json::{Map, Value, json};

/// The views a multi-image task takes, front first as Meshy expects.
pub(super) const VIEWS: [&str; 4] = ["front", "left", "back", "right"];
/// The generation models offered: Meshy 6 and 7.1 share a price, unlike the cheaper 6 Lite.
pub(super) const MODELS: [&str; 2] = ["meshy-6", "meshy-7.1"];
/// The retexture models offered, at the same price for 2K textures.
pub(super) const TEXTURE_MODELS: [&str; 2] = ["meshy-6", "latest"];

pub(super) struct Prepared {
    pub task: &'static str,
    pub body: Value,
}
fn image_mime(m: &str) -> Option<&'static str> {
    match m {
        "image/png" => Some("image/png"),
        "image/jpeg" => Some("image/jpeg"),
        "image/webp" => Some("image/webp"),
        _ => None,
    }
}
fn chars(text: &str, max: usize) -> bool {
    text.chars().take(max.saturating_add(1)).count() <= max
}
fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}
/// An image whose bytes match its type, within the limit, as a data URI.
fn image(mime: &str, bytes: &[u8], l: Limits) -> Result<String, ProviderError> {
    let mime = image_mime(mime).ok_or_else(|| invalid("Meshy takes PNG, JPEG or WebP images"))?;
    if bytes.is_empty() || bytes.len() > l.image_bytes {
        return Err(invalid(format!("a Meshy image must be under {} MiB", l.image_bytes.div_ceil(MIB))));
    }
    if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
        return Err(invalid("a Meshy image's bytes don't match its type"));
    }
    Ok(data_uri(mime, bytes))
}
/// A wired model: a whole GLB within the limit, sent as Meshy asks for data URIs.
fn model(mime: &str, bytes: &[u8], l: Limits) -> Result<String, ProviderError> {
    if mime != GLB {
        return Err(invalid("Meshy takes a GLB model; convert the 3D model to GLB first"));
    }
    if bytes.len() > l.model_bytes {
        return Err(invalid(format!("a Meshy input model must be under {} MiB", l.model_bytes.div_ceil(MIB))));
    }
    crate::model_file::glb(bytes, "the wired model").map_err(|_| invalid("the wired model isn't a whole GLB file"))?;
    Ok(data_uri("application/octet-stream", bytes))
}
fn choice<'a>(value: &'a Value, allowed: &[&str], what: &str) -> Result<&'a str, ProviderError> {
    value.as_str().filter(|v| allowed.contains(v)).ok_or_else(|| invalid(format!("unknown Meshy {what}")))
}
/// The settings a task takes, checked; everything that would change the price is fixed here.
fn options(job: &ProviderJob, task: &str) -> Result<Map<String, Value>, ProviderError> {
    let mut out = Map::new();
    for (name, value) in &job.params {
        match (task, name.as_str()) {
            (TEXT | IMAGE | MULTI_IMAGE, "ai_model") => {
                out.insert(name.clone(), json!(choice(value, &MODELS, "model")?));
            }
            (TEXT | IMAGE | MULTI_IMAGE, "pose_mode") => {
                let pose = choice(value, &["none", "a-pose", "t-pose"], "pose")?;
                if pose != "none" {
                    out.insert(name.clone(), json!(pose));
                }
            }
            (RETEXTURE, "ai_model") => {
                out.insert(name.clone(), json!(choice(value, &TEXTURE_MODELS, "model")?));
            }
            (RETEXTURE, "uv") => {
                out.insert("enable_original_uv".into(), json!(choice(value, &["new", "original"], "UV choice")? == "original"));
            }
            (REMESH, "topology") => {
                out.insert(name.clone(), json!(choice(value, &["triangle", "quad"], "topology")?));
            }
            (REMESH, "target_polycount") => {
                let faces = value.as_u64().filter(|n| (100..=300_000).contains(n)).ok_or_else(|| invalid("Meshy target polycount must be 100 to 300000"))?;
                out.insert(name.clone(), json!(faces));
            }
            _ => return Err(invalid("unknown Meshy option")),
        }
    }
    match task {
        // The preview stage: an untextured mesh at standard geometry.
        TEXT => {
            out.insert("mode".into(), json!("preview"));
        }
        // Textured PBR models with 2K textures at standard geometry: what the catalog price covers.
        IMAGE | MULTI_IMAGE => {
            out.insert("should_texture".into(), json!(true));
            out.insert("enable_pbr".into(), json!(true));
            out.insert("texture_resolution".into(), json!("2k"));
        }
        RETEXTURE => {
            out.insert("enable_pbr".into(), json!(true));
            out.insert("texture_resolution".into(), json!("2k"));
        }
        _ => {}
    }
    out.insert("target_formats".into(), json!(["glb"]));
    Ok(out)
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared, ProviderError> {
    let task = TASKS.iter().copied().find(|t| *t == job.model).ok_or_else(|| invalid("choose a Meshy text, image, multi-image, retexture or remesh model"))?;
    if job.inputs.len() > 5 || job.params.len() > 8 {
        return Err(invalid("Meshy takes one prompt, a model and up to four images"));
    }
    let mut body = options(job, task)?;
    let mut prompt = None;
    let mut views: [Option<String>; 4] = Default::default();
    let mut single = None;
    let mut mesh = None;
    for input in &job.inputs {
        match (task, input) {
            (TEXT | RETEXTURE, JobInput::Text { role, text }) if role == "prompt" && prompt.is_none() => {
                if text.trim().is_empty() || !chars(text, l.prompt_chars) {
                    return Err(invalid(format!("a Meshy prompt has 1 to {} characters", l.prompt_chars)));
                }
                prompt = Some(text.as_str());
            }
            (IMAGE, JobInput::Media { role, mime, bytes }) | (RETEXTURE, JobInput::Media { role, mime, bytes })
                if (role == "image" && task == IMAGE) || (role == "style" && task == RETEXTURE) =>
            {
                if single.is_some() {
                    return Err(invalid("this Meshy model takes one image"));
                }
                single = Some(image(mime, bytes, l)?);
            }
            (MULTI_IMAGE, JobInput::Media { role, mime, bytes }) => {
                let slot = VIEWS.iter().position(|v| v == role).ok_or_else(|| invalid("a Meshy view is front, left, back or right"))?;
                let view = views.get_mut(slot).ok_or_else(|| invalid("a Meshy view is front, left, back or right"))?;
                if view.is_some() {
                    return Err(invalid("each Meshy view takes one image"));
                }
                *view = Some(image(mime, bytes, l)?);
            }
            (RETEXTURE | REMESH, JobInput::Media { role, mime, bytes }) if role == "mesh" => {
                if mesh.is_some() {
                    return Err(invalid("this Meshy model takes one 3D model"));
                }
                mesh = Some(model(mime, bytes, l)?);
            }
            _ => return Err(invalid("this Meshy model doesn't take that input")),
        }
    }
    match task {
        TEXT => {
            body.insert("prompt".into(), json!(prompt.ok_or_else(|| invalid("a Meshy text model needs a prompt"))?));
        }
        IMAGE => {
            body.insert("image_url".into(), json!(single.ok_or_else(|| invalid("a Meshy image model needs one image"))?));
        }
        MULTI_IMAGE => {
            let [front, left, back, right] = views;
            let front = front.ok_or_else(|| invalid("a Meshy multi-image model needs the front view"))?;
            let images: Vec<String> = std::iter::once(front).chain([left, back, right].into_iter().flatten()).collect();
            body.insert("image_urls".into(), json!(images));
        }
        RETEXTURE => {
            body.insert("model_url".into(), json!(mesh.ok_or_else(|| invalid("Meshy retexture needs a 3D model"))?));
            match (prompt, single) {
                (Some(prompt), None) => body.insert("text_style_prompt".into(), json!(prompt)),
                (None, Some(style)) => body.insert("image_style_url".into(), json!(style)),
                _ => return Err(invalid("Meshy retexture takes either a prompt or a style image")),
            };
        }
        _ => {
            body.insert("model_url".into(), json!(mesh.ok_or_else(|| invalid("Meshy remesh needs a 3D model"))?));
        }
    }
    Ok(Prepared { task, body: Value::Object(body) })
}
