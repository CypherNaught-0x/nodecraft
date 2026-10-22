//! A job checked and shaped into a Higgsfield request body. Nothing here touches the network.
use super::*;
use crate::JobInput;
use serde_json::{Map, Value, json};

pub(super) const ASPECTS: [&str; 7] = ["9:16", "16:9", "4:3", "3:4", "1:1", "2:3", "3:2"];
pub(super) const RESOLUTIONS: [&str; 2] = ["720p", "1080p"];
/// Genjutsu output resolutions; its price per source second depends on it.
pub(super) const EDIT_RESOLUTIONS: [&str; 3] = ["480p", "720p", "1080p"];
/// Reference images a Genjutsu edit takes, in order; restyle takes at most five.
pub(super) const EDIT_IMAGES: usize = 8;
pub(super) const RESTYLE_IMAGES: usize = 5;
pub(super) const SEEDS: std::ops::RangeInclusive<u64> = 1..=1_000_000;

pub(super) struct Image<'a> {
    pub mime: &'static str,
    pub bytes: &'a [u8],
}
pub(super) struct Prepared<'a> {
    pub model: &'static str,
    /// Results the request asks for.
    pub count: u64,
    /// The Soul ID a SOUL 2 request names, which the adapter confirms is ready before paying.
    pub reference: Option<String>,
    /// Everything but staged inputs, which [`Prepared::body`] adds.
    fields: Map<String, Value>,
    /// A Genjutsu source video, staged first.
    pub video: Option<Image<'a>>,
    pub images: Vec<Image<'a>>,
}
pub(super) fn mime(m: &str) -> Option<&'static str> {
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
fn soul(job: &ProviderJob, fields: &mut Map<String, Value>) -> Result<(u64, Option<String>), ProviderError> {
    // Sent explicitly: Higgsfield's pages disagree on some defaults.
    let mut aspect = "1:1";
    let mut resolution = "720p";
    let mut enhance = false;
    let mut batch = 1;
    let mut reference = None;
    let mut strength = None;
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
            // No Soul ID is an empty field: the card's text box before one is picked.
            "custom_reference_id" if value.as_str().is_some_and(|s| s.trim().is_empty()) => {}
            "custom_reference_id" => {
                let id = value
                    .as_str()
                    .map(str::trim)
                    .filter(|id| uuid(id))
                    .ok_or_else(|| invalid("a Higgsfield Soul ID is the UUID Higgsfield issued; Refresh option lists fetches this account's"))?;
                reference = Some(id.to_ascii_lowercase());
            }
            "custom_reference_strength" => {
                let n =
                    value.as_f64().filter(|n| n.is_finite() && (0.0..=1.0).contains(n)).ok_or_else(|| invalid("a Higgsfield Soul ID strength is 0 to 1"))?;
                strength = Some((n * 100.0).round() / 100.0);
            }
            _ => return Err(invalid("unknown Higgsfield SOUL option")),
        }
    }
    // The strength means nothing without an identity, and is left out with none.
    if let Some(id) = &reference {
        fields.insert("custom_reference_id".into(), json!(id));
        fields.insert("custom_reference_strength".into(), json!(strength.unwrap_or(1.0)));
    }
    fields.insert("aspect_ratio".into(), json!(aspect));
    fields.insert("resolution".into(), json!(resolution));
    fields.insert("enhance_prompt".into(), json!(enhance));
    fields.insert("batch_size".into(), json!(batch));
    Ok((batch, reference))
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
/// A restyle preset or Soul ID as Higgsfield issues them: a UUID.
pub(super) fn uuid(s: &str) -> bool {
    s.len() == 36 && s.bytes().enumerate().all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { b == b'-' } else { b.is_ascii_hexdigit() })
}
fn genjutsu(model: &str, job: &ProviderJob, fields: &mut Map<String, Value>) -> Result<(), ProviderError> {
    // Sent explicitly: the price depends on it.
    let mut resolution = "720p";
    let choose = || invalid("choose a Higgsfield restyle preset; Refresh option lists fetches them");
    for (name, value) in &job.params {
        match name.as_str() {
            "resolution" => resolution = text(value, "resolution", &EDIT_RESOLUTIONS)?,
            "preset_id" if model == RESTYLE => {
                let id = value.as_str().filter(|id| uuid(id)).ok_or_else(choose)?;
                fields.insert("preset_id".into(), json!(id.to_ascii_lowercase()));
            }
            _ => return Err(invalid("unknown Higgsfield Genjutsu option")),
        }
    }
    if model == RESTYLE && !fields.contains_key("preset_id") {
        return Err(choose());
    }
    fields.insert("resolution".into(), json!(resolution));
    Ok(())
}
fn edits(model: &str) -> bool {
    model == MOTION_TRANSFER || model == OBJECT_SWAP || model == RESTYLE
}
/// The reference images an edit takes.
fn edit_images(model: &str) -> usize {
    if model == RESTYLE { RESTYLE_IMAGES } else { EDIT_IMAGES }
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared<'_>, ProviderError> {
    let model = match job.model.as_str() {
        SOUL => SOUL,
        KLING_I2V => KLING_I2V,
        MOTION_TRANSFER => MOTION_TRANSFER,
        OBJECT_SWAP => OBJECT_SWAP,
        RESTYLE => RESTYLE,
        _ => return Err(invalid("choose a Higgsfield model this version supports")),
    };
    let most = if edits(model) { edit_images(model) + 2 } else { 3 };
    if job.inputs.len() > most || job.params.len() > 8 {
        return Err(if edits(model) {
            invalid(format!("this Higgsfield Genjutsu edit takes a prompt, one source video and at most {} images", edit_images(model)))
        } else {
            invalid("Higgsfield takes a prompt and at most one image")
        });
    }
    let mut fields = Map::new();
    let mut video = None;
    let mut images = Vec::new();
    images.try_reserve_exact(if edits(model) { edit_images(model) } else { 1 }).map_err(|_| malformed("Higgsfield bounded allocation failed"))?;
    for input in &job.inputs {
        match input {
            // A Genjutsu prompt is optional; an empty one is left out.
            JobInput::Text { role, text } if role == "prompt" && edits(model) && text.trim().is_empty() => {}
            JobInput::Text { role, text } if (role == "prompt" || (role == "negative_prompt" && model == KLING_I2V)) && !fields.contains_key(role.as_str()) => {
                if (role == "prompt" && text.trim().is_empty()) || !chars(text, l.prompt_chars) {
                    let what = if role == "prompt" { "a Higgsfield prompt has 1" } else { "a Higgsfield negative prompt has 0" };
                    return Err(invalid(format!("{what} to {} characters", l.prompt_chars)));
                }
                fields.insert(role.clone(), json!(text));
            }
            JobInput::Media { role, mime: m, bytes } if role == "source" && edits(model) && video.is_none() => {
                // Higgsfield's uploads take MP4 only.
                if m != "video/mp4" || nodecraft_media::sniff(bytes).map(|s| s.mime) != Some("video/mp4") {
                    return Err(invalid("Higgsfield takes the source video as an MP4; convert it first"));
                }
                if bytes.len() > l.video_bytes {
                    return Err(invalid(format!("a Higgsfield source video must be under {} MiB", l.video_bytes.div_ceil(MIB))));
                }
                video = Some(Image { mime: "video/mp4", bytes });
            }
            JobInput::Media { role, mime: m, bytes }
                if role == "image" && ((model == KLING_I2V && images.is_empty()) || (edits(model) && images.len() < edit_images(model))) =>
            {
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
    if !fields.contains_key("prompt") && !edits(model) {
        return Err(invalid("a Higgsfield model needs a prompt"));
    }
    let mut reference = None;
    let count = if edits(model) {
        if video.is_none() {
            return Err(invalid("a Higgsfield Genjutsu edit needs a source video"));
        }
        // Restyle's references are optional; the other edits are made from them.
        if images.is_empty() && model != RESTYLE {
            return Err(invalid("a Higgsfield Genjutsu edit needs at least one image"));
        }
        genjutsu(model, job, &mut fields)?;
        1
    } else if model == SOUL {
        let (count, id) = soul(job, &mut fields)?;
        reference = id;
        count
    } else {
        if images.len() != 1 {
            return Err(invalid("Kling image to video needs one image"));
        }
        kling(job, &mut fields)?;
        1
    };
    Ok(Prepared { model, count, reference, fields, video, images })
}
impl Prepared<'_> {
    /// The request body, given the staged source video's public URL and each image's (in
    /// `images` order).
    pub fn body(&self, video: Option<&str>, urls: &[String]) -> Result<Value, ProviderError> {
        if urls.len() != self.images.len() || video.is_some() != self.video.is_some() {
            return Err(malformed("Higgsfield upload count disagrees with the job"));
        }
        let mut body = self.fields.clone();
        if let Some(video) = video {
            // Genjutsu: the images are ordered references. Restyle refuses nulls, so a restyle
            // without references sends no list at all.
            body.insert("video_url".into(), json!(video));
            if !urls.is_empty() {
                body.insert("image_urls".into(), json!(urls));
            }
        } else if let Some(url) = urls.first() {
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
