//! OpenRouter's speech and video APIs, checked against OpenRouter's text-to-speech and video
//! generation guides on 2026-11-06. Both read the `openrouter` key; neither documents an
//! idempotency key, so an uncertain submit is never retried automatically.
//!
//! - [`OpenRouterSpeech`] (`openrouter-speech`): `POST /api/v1/audio/speech`, one synchronous
//!   request answered with the MP3 itself.
//! - [`OpenRouterVideo`] (`openrouter-video`): `POST /api/v1/videos` queues a job, `GET
//!   /api/v1/videos/{id}` reports it, and `GET /api/v1/videos/{id}/content` downloads the MP4. The
//!   charge is the completed job's `usage.cost`. OpenRouter documents no cancel endpoint.
//!
//! Which models exist and which settings each takes come from OpenRouter's own lists (the
//! engine's live catalog); these adapters check every value's shape before anything is sent.
mod speech;
#[cfg(test)]
mod tests;
mod video;

pub use speech::OpenRouterSpeech;
pub use video::OpenRouterVideo;

use crate::{ErrorKind, JobInput, ProviderError, ProviderJob};

/// The speech adapter's id, which catalog models name as their provider.
pub const SPEECH_ID: &str = "openrouter-speech";
/// The video adapter's id.
pub const VIDEO_ID: &str = "openrouter-video";
const API: &str = "https://openrouter.ai/api/v1";
const MIB: usize = 1024 * 1024;

fn invalid(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, message)
}

fn malformed(message: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Malformed, message)
}

/// A model id as OpenRouter writes them: no spaces, quotes or control characters.
fn model_ok(model: &str) -> bool {
    !model.is_empty() && model.len() <= 256 && model.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._:-~".contains(&b))
}

/// A short listed value (a voice, resolution or aspect ratio), as the listing's alphabet allows.
fn short_value<'a>(name: &str, value: &'a serde_json::Value, max: usize) -> Result<&'a str, ProviderError> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= max && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"._:-x".contains(&b)))
        .ok_or_else(|| invalid(format!("the setting `{name}` must be one of the values the model lists")))
}

/// A whole number setting within `max`, as JSON number or decimal text.
fn whole(name: &str, value: &serde_json::Value, max: u64) -> Result<u64, ProviderError> {
    let number = match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    };
    number.filter(|n| *n <= max).ok_or_else(|| invalid(format!("the setting `{name}` is a whole number from 0 to {max}")))
}

/// The job's one prompt, bounded in characters.
fn prompt(job: &ProviderJob, max_chars: usize) -> Result<&str, ProviderError> {
    let mut found = None;
    for input in &job.inputs {
        if let JobInput::Text { role, text } = input
            && role == "prompt"
        {
            if found.is_some() {
                return Err(invalid("OpenRouter takes one prompt"));
            }
            found = Some(text.as_str());
        }
    }
    let text = found.filter(|text| !text.trim().is_empty()).ok_or_else(|| invalid("OpenRouter needs a prompt"))?;
    if text.len() > max_chars.saturating_mul(4) || text.chars().take(max_chars.saturating_add(1)).count() > max_chars {
        return Err(invalid(format!("the prompt must be at most {max_chars} characters")));
    }
    Ok(text)
}

/// The transport error for a request that may have been accepted.
fn sent(error: ProviderError, what: &str) -> ProviderError {
    ProviderError {
        kind: error.kind,
        message: format!("{what} failed; OpenRouter may still have accepted and billed it: {}", error.message),
        retry_after_ms: None,
    }
}

/// USD as micros, rounded up so a reported charge is never understated; `None` for NaN, negative
/// or absurd amounts.
fn usd(cost: f64) -> Option<nodecraft_types::Money> {
    if !(cost.is_finite() && (0.0..1e9).contains(&cost)) {
        return None;
    }
    let micros = (cost * 1e6).ceil();
    (0.0..1e16).contains(&micros).then(|| nodecraft_types::Money::new(nodecraft_types::Currency::USD, micros as u64))
}
