//! A job checked and shaped into Tripo's task body. Nothing here touches the network.
use super::*;
use crate::JobInput;
use serde_json::{Map, Value, json};

/// The views a multiview task takes, in Tripo's order; the front is required.
pub(super) const VIEWS: [&str; 4] = ["front", "left", "back", "right"];
/// The model versions offered (Tripo's H3 and H2 lines, which share a price).
pub(super) const VERSIONS: [&str; 3] = ["v3.1-20260211", "v3.0-20250812", "v2.5-20250123"];

pub(super) struct Image<'a> {
    /// The role it came in on: `image`, or one of [`VIEWS`].
    pub role: &'a str,
    pub mime: &'static str,
    pub bytes: &'a [u8],
}
impl Image<'_> {
    fn kind(&self) -> &'static str {
        match self.mime {
            "image/png" => "png",
            "image/webp" => "webp",
            _ => "jpg",
        }
    }
    pub fn file_name(&self) -> &'static str {
        match self.mime {
            "image/png" => "image.png",
            "image/webp" => "image.webp",
            _ => "image.jpg",
        }
    }
}
pub(super) struct Prepared<'a> {
    pub task: &'static str,
    prompt: Option<&'a str>,
    options: Map<String, Value>,
    pub images: Vec<Image<'a>>,
}
fn mime(m: &str) -> Option<&'static str> {
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
fn options(job: &ProviderJob, task: &str, l: Limits) -> Result<Map<String, Value>, ProviderError> {
    let mut out = Map::new();
    for (name, value) in &job.params {
        match name.as_str() {
            "model_version" => {
                let version = value.as_str().filter(|v| VERSIONS.contains(v)).ok_or_else(|| invalid("unknown Tripo model version"))?;
                out.insert(name.clone(), json!(version));
            }
            "model_seed" | "texture_seed" => {
                let seed = value.as_u64().filter(|n| *n <= u64::from(u32::MAX)).ok_or_else(|| invalid("a Tripo seed must fit 32 bits"))?;
                out.insert(name.clone(), json!(seed));
            }
            "face_limit" => {
                let faces = value.as_u64().filter(|n| (1_000..=1_500_000).contains(n)).ok_or_else(|| invalid("Tripo face limit must be 1000 to 1500000"))?;
                out.insert(name.clone(), json!(faces));
            }
            "negative_prompt" if task == TEXT => {
                let text = value.as_str().ok_or_else(|| invalid("Tripo negative prompt must be text"))?;
                if !chars(text, l.negative_chars) {
                    return Err(invalid("Tripo negative prompt exceeds 255 characters"));
                }
                if !text.is_empty() {
                    out.insert(name.clone(), json!(text));
                }
            }
            _ => return Err(invalid("unknown Tripo option")),
        }
    }
    // Textured PBR models at standard quality: the settings the catalog price covers.
    out.insert("texture".into(), json!(true));
    out.insert("pbr".into(), json!(true));
    Ok(out)
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared<'_>, ProviderError> {
    let task = match job.model.as_str() {
        TEXT => TEXT,
        IMAGE => IMAGE,
        MULTIVIEW => MULTIVIEW,
        _ => return Err(invalid("choose a Tripo text, image or multiview model")),
    };
    if job.inputs.len() > 5 || job.params.len() > 8 {
        return Err(invalid("Tripo takes one prompt or up to four images"));
    }
    let mut prompt = None;
    let mut images = Vec::new();
    images.try_reserve_exact(4).map_err(|_| malformed("Tripo bounded allocation failed"))?;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && task == TEXT && prompt.is_none() => {
                if text.trim().is_empty() || !chars(text, l.prompt_chars) {
                    return Err(invalid("a Tripo prompt has 1 to 1024 characters"));
                }
                prompt = Some(text.as_str());
            }
            JobInput::Media { role, mime: m, bytes } => {
                let slot = match (task, role.as_str()) {
                    (IMAGE, "image") => "image",
                    (MULTIVIEW, view) => VIEWS.iter().copied().find(|v| *v == view).ok_or_else(|| invalid("a Tripo view is front, left, back or right"))?,
                    _ => return Err(invalid("this Tripo model doesn't take that input")),
                };
                if images.iter().any(|i: &Image<'_>| i.role == slot) {
                    return Err(invalid("each Tripo view takes one image"));
                }
                let mime = mime(m).ok_or_else(|| invalid("Tripo takes PNG, JPEG or WebP images"))?;
                if bytes.is_empty() || bytes.len() > l.input_bytes {
                    return Err(invalid("a Tripo image must be under 20 MB"));
                }
                let sniffed = nodecraft_media::sniff(bytes).map(|s| s.mime);
                if sniffed != Some(mime) {
                    return Err(invalid("a Tripo image's bytes don't match its type"));
                }
                images.push(Image { role: slot, mime, bytes });
            }
            _ => return Err(invalid("this Tripo model doesn't take that input")),
        }
    }
    let fits = match task {
        TEXT => prompt.is_some(),
        IMAGE => images.len() == 1,
        // Tripo needs the front and at least one more view.
        _ => images.iter().any(|i| i.role == "front") && images.len() >= 2,
    };
    if !fits {
        return Err(invalid(match task {
            TEXT => "a Tripo text model needs a prompt",
            IMAGE => "a Tripo image model needs one image",
            _ => "a Tripo multiview model needs the front view and at least one other",
        }));
    }
    Ok(Prepared { task, prompt, options: options(job, task, l)?, images })
}
impl Prepared<'_> {
    /// The task body, given each image's upload token (in `images` order).
    pub fn body(&self, tokens: &[String]) -> Result<Value, ProviderError> {
        if tokens.len() != self.images.len() {
            return Err(malformed("Tripo upload count disagrees with the job"));
        }
        let file = |image: &Image<'_>, token: &str| json!({"type": image.kind(), "file_token": token});
        let mut body = self.options.clone();
        body.insert("type".into(), json!(self.task));
        if let Some(prompt) = self.prompt {
            body.insert("prompt".into(), json!(prompt));
        }
        match self.task {
            IMAGE => {
                let (image, token) = self.images.first().zip(tokens.first()).ok_or_else(|| malformed("Tripo image missing"))?;
                body.insert("file".into(), file(image, token));
            }
            MULTIVIEW => {
                // Four slots in Tripo's order; a missing view is an object without a token.
                let files: Vec<Value> = VIEWS
                    .iter()
                    .map(|view| match self.images.iter().zip(tokens).find(|(image, _)| image.role == *view) {
                        Some((image, token)) => file(image, token),
                        None => json!({}),
                    })
                    .collect();
                body.insert("files".into(), Value::Array(files));
            }
            _ => {}
        }
        Ok(Value::Object(body))
    }
}
