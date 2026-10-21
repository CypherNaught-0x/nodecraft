//! A job checked and shaped into a Higgsfield request body. Nothing here touches the network.
use super::*;
use crate::JobInput;
use serde_json::{Map, Value, json};

pub(super) const ASPECTS: [&str; 7] = ["9:16", "16:9", "4:3", "3:4", "1:1", "2:3", "3:2"];
pub(super) const RESOLUTIONS: [&str; 2] = ["720p", "1080p"];
pub(super) const SEEDS: std::ops::RangeInclusive<u64> = 1..=1_000_000;

pub(super) struct Image<'a> {
    pub mime: &'static str,
    pub bytes: &'a [u8],
}
pub(super) struct Prepared<'a> {
    pub model: &'static str,
    /// Results the request asks for.
    pub count: u64,
    /// Everything but staged inputs, which [`Prepared::body`] adds.
    fields: Map<String, Value>,
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
fn text(value: &Value, what: &str, allowed: &[&'static str]) -> Result<&'static str, ProviderError> {
    let s = value.as_str().ok_or_else(|| invalid(format!("Higgsfield {what} must be text")))?;
    allowed.iter().copied().find(|a| *a == s).ok_or_else(|| invalid(format!("unknown Higgsfield {what}")))
}
/// A count given as a number or as the text of a choice.
fn count(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
fn soul(job: &ProviderJob, fields: &mut Map<String, Value>) -> Result<u64, ProviderError> {
    // Sent explicitly: Higgsfield's pages disagree on some defaults.
    let mut aspect = "1:1";
    let mut resolution = "720p";
    let mut enhance = false;
    let mut batch = 1;
    for (name, value) in &job.params {
        match name.as_str() {
            "aspect_ratio" => aspect = text(value, "aspect ratio", &ASPECTS)?,
            "resolution" => resolution = text(value, "resolution", &RESOLUTIONS)?,
            "enhance_prompt" => enhance = value.as_bool().ok_or_else(|| invalid("Higgsfield prompt enhancement is on or off"))?,
            "batch_size" => batch = count(value).filter(|n| matches!(n, 1 | 4)).ok_or_else(|| invalid("Higgsfield SOUL makes 1 or 4 images"))?,
            "seed" => {
                // An explicit null is refused by SOUL 2; no seed means a random one.
                let seed = value.as_u64().filter(|n| SEEDS.contains(n)).ok_or_else(|| invalid("a Higgsfield seed is 1 to 1000000"))?;
                fields.insert("seed".into(), json!(seed));
            }
            _ => return Err(invalid("unknown Higgsfield SOUL option")),
        }
    }
    fields.insert("aspect_ratio".into(), json!(aspect));
    fields.insert("resolution".into(), json!(resolution));
    fields.insert("enhance_prompt".into(), json!(enhance));
    fields.insert("batch_size".into(), json!(batch));
    Ok(batch)
}
fn kling(job: &ProviderJob, fields: &mut Map<String, Value>) -> Result<(), ProviderError> {
    let mut duration = 5;
    let mut cfg = 0.5;
    for (name, value) in &job.params {
        match name.as_str() {
            "duration" => duration = count(value).filter(|n| matches!(n, 5 | 10)).ok_or_else(|| invalid("a Kling clip is 5 or 10 seconds"))?,
            "cfg_scale" => {
                cfg = value.as_f64().filter(|n| n.is_finite() && (0.0..=1.0).contains(n)).ok_or_else(|| invalid("Kling prompt adherence is 0 to 1"))?;
            }
            _ => return Err(invalid("unknown Higgsfield Kling option")),
        }
    }
    fields.insert("duration".into(), json!(duration));
    // Hundredths, the step Higgsfield documents.
    fields.insert("cfg_scale".into(), json!((cfg * 100.0).round() / 100.0));
    fields.entry("negative_prompt").or_insert_with(|| json!(""));
    Ok(())
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared<'_>, ProviderError> {
    let model = match job.model.as_str() {
        SOUL => SOUL,
        KLING_I2V => KLING_I2V,
        _ => return Err(invalid("choose Higgsfield SOUL 2 or Kling 2.5 Turbo Pro image to video")),
    };
    if job.inputs.len() > 3 || job.params.len() > 8 {
        return Err(invalid("Higgsfield takes a prompt and at most one image"));
    }
    let mut fields = Map::new();
    let mut images = Vec::new();
    images.try_reserve_exact(1).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if (role == "prompt" || (role == "negative_prompt" && model == KLING_I2V)) && !fields.contains_key(role.as_str()) => {
                if (role == "prompt" && text.trim().is_empty()) || !chars(text, l.prompt_chars) {
                    let what = if role == "prompt" { "a Higgsfield prompt has 1" } else { "a Higgsfield negative prompt has 0" };
                    return Err(invalid(format!("{what} to {} characters", l.prompt_chars)));
                }
                fields.insert(role.clone(), json!(text));
            }
            JobInput::Media { role, mime: m, bytes } if role == "image" && model == KLING_I2V && images.is_empty() => {
                let mime = mime(m).ok_or_else(|| invalid("Higgsfield takes PNG, JPEG or WebP images"))?;
                if bytes.is_empty() || bytes.len() > l.input_bytes {
                    return Err(invalid(format!("a Higgsfield image must be under {} MiB", l.input_bytes.div_ceil(MIB))));
                }
                if nodecraft_media::sniff(bytes).map(|s| s.mime) != Some(mime) {
                    return Err(invalid("a Higgsfield image's bytes don't match its type"));
                }
                images.push(Image { mime, bytes });
            }
            _ => return Err(invalid("this Higgsfield model doesn't take that input")),
        }
    }
    if !fields.contains_key("prompt") {
        return Err(invalid("a Higgsfield model needs a prompt"));
    }
    let count = if model == SOUL {
        soul(job, &mut fields)?
    } else {
        if images.len() != 1 {
            return Err(invalid("Kling image to video needs one image"));
        }
        kling(job, &mut fields)?;
        1
    };
    Ok(Prepared { model, count, fields, images })
}
impl Prepared<'_> {
    /// The request body, given each image's staged public URL (in `images` order).
    pub fn body(&self, urls: &[String]) -> Result<Value, ProviderError> {
        if urls.len() != self.images.len() {
            return Err(malformed("Higgsfield upload count disagrees with the job"));
        }
        let mut body = self.fields.clone();
        if let Some(url) = urls.first() {
            body.insert("image_url".into(), json!(url));
        }
        Ok(Value::Object(body))
    }
}
/// What the job asks for, so a body staged for one job is never replayed for another under the
/// same key: the model, every parameter and every input's role, type and content.
pub(super) fn fingerprint(job: &ProviderJob) -> ContentHash {
    let inputs: Vec<Value> = job
        .inputs
        .iter()
        .map(|input| match input {
            JobInput::Text { role, text } => json!(["text", role, ContentHash::of(text.as_bytes()).to_hex()]),
            JobInput::Media { role, mime, bytes } => json!(["media", role, mime, ContentHash::of(bytes).to_hex()]),
        })
        .collect();
    let described = json!({"model": job.model, "params": job.params, "inputs": inputs});
    ContentHash::of(described.to_string().as_bytes())
}
/// The job's key as Higgsfield takes them (1 to 255 visible ASCII characters), or a stable hash
/// of it when it isn't one.
pub(super) fn idempotency_key(key: &str) -> String {
    if !key.is_empty() && key.len() <= 255 && key.bytes().all(|b| b.is_ascii_graphic()) { key.to_owned() } else { ContentHash::of(key.as_bytes()).to_hex() }
}
