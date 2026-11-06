//! The reply: `{data: [{b64_json, media_type?}], usage?: {cost?}}`.
use super::{Limits, invalid, malformed};
use crate::{Completed, ErrorKind, HttpResponse, Output, ProviderError, Usage};
use base64::Engine;
use nodecraft_types::{Currency, Money};
use serde::Deserialize;
use std::sync::Arc;

/// Most images one reply may carry (the catalog asks for one).
const MAX_IMAGES: usize = 4;
/// Types NodeCraft places as image results.
const PLACED: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    data: Vec<Item>,
    #[serde(default)]
    usage: Option<Spend>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
}

#[derive(Deserialize)]
struct Spend {
    #[serde(default)]
    cost: Option<f64>,
}

/// USD as micros, rounded up so a reported charge is never understated; `None` for NaN, negative
/// or absurd amounts.
pub(super) fn usd(cost: f64) -> Option<Money> {
    if !(cost.is_finite() && (0.0..1e9).contains(&cost)) {
        return None;
    }
    let micros = (cost * 1e6).ceil();
    // In range: at most 1e15, well inside u64.
    (0.0..1e16).contains(&micros).then(|| Money::new(Currency::USD, micros as u64))
}

fn decode(text: &str, limits: Limits) -> Result<Vec<u8>, ProviderError> {
    // A base64 text decodes to at most 3/4 of its length; refuse before allocating.
    if text.len() / 4 * 3 > limits.output_bytes.saturating_add(3) {
        return Err(malformed("OpenRouter returned an image larger than NodeCraft accepts"));
    }
    base64::engine::general_purpose::STANDARD.decode(text.as_bytes()).map_err(|_| malformed("OpenRouter returned an image that isn't valid base64"))
}

pub(super) fn parse(reply: &HttpResponse, limits: Limits) -> Result<Completed, ProviderError> {
    if reply.status == 502 {
        return Err(ProviderError::new(ErrorKind::Server, "OpenRouter couldn't finish the image (not billed); try again or pick another model"));
    }
    if reply.status != 200 {
        return Err(ProviderError::from_status(reply.status, &reply.body));
    }
    let parsed: Reply = serde_json::from_slice(&reply.body).map_err(|_| malformed("OpenRouter's image reply isn't the JSON its API describes"))?;
    if parsed.data.is_empty() {
        return Err(malformed("OpenRouter's image reply has no image"));
    }
    if parsed.data.len() > MAX_IMAGES {
        return Err(malformed("OpenRouter's image reply has more images than were asked for"));
    }
    let mut outputs = Vec::new();
    for item in parsed.data {
        let text = item.b64_json.ok_or_else(|| malformed("OpenRouter's image reply has an entry without image data"))?;
        if item.media_type.as_deref() == Some("image/svg+xml") {
            return Err(invalid("this model returned an SVG, which NodeCraft can't place as an image yet; pick a model that makes PNG, JPEG or WebP"));
        }
        let bytes = decode(&text, limits)?;
        drop(text);
        if bytes.len() > limits.output_bytes {
            return Err(malformed("OpenRouter returned an image larger than NodeCraft accepts"));
        }
        let meta = nodecraft_media::probe_image(&bytes, &limits.image(false)).map_err(|_| malformed("OpenRouter returned an image NodeCraft can't read"))?;
        if !PLACED.contains(&meta.mime) {
            return Err(invalid("OpenRouter returned an image type NodeCraft can't place"));
        }
        let pixels = u64::from(meta.width).saturating_mul(u64::from(meta.height));
        if meta.width == 0 || meta.height == 0 || pixels > limits.pixels {
            return Err(malformed("OpenRouter returned an image outside the supported size"));
        }
        outputs.push(Output::Bytes { mime: meta.mime.into(), bytes: Arc::from(bytes) });
    }
    let usage = Usage { cost: parsed.usage.and_then(|spend| spend.cost).and_then(usd), ..Usage::default() };
    Ok(Completed { outputs, usage })
}
