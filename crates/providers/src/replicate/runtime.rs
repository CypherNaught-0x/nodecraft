//! Server lifetime and optional execution-time evidence, separate from money and polling time.
use super::{ProviderError, invalid};
use serde::Deserialize;
use serde_json::value::RawValue;

const MAX_MS: u64 = 24 * 60 * 60 * 1000;

pub(super) fn cancel_after(milliseconds: u64) -> Result<String, ProviderError> {
    if !(5_000..=MAX_MS).contains(&milliseconds) {
        return Err(invalid("Replicate server lifetime must be between 5 seconds and 24 hours; no prediction was sent"));
    }
    let seconds = milliseconds.checked_add(999).map(|n| n / 1000).ok_or_else(|| invalid("Replicate server lifetime overflow"))?;
    Ok(format!("{seconds}s"))
}

pub(super) fn execution_ms(raw: Option<&RawValue>) -> Option<u64> {
    #[derive(Deserialize)]
    struct Metrics {
        predict_time: Option<f64>,
    }
    // The response lexer already bounds this borrowed region before deserialization. Bad
    // optional telemetry must not discard an otherwise valid, already-paid output.
    let raw = raw?.get();
    if !raw.trim_start().starts_with('{') {
        return None;
    }
    let seconds = serde_json::from_str::<Metrics>(raw).ok()?.predict_time?;
    if !seconds.is_finite() || !(0.0..=86_400.0).contains(&seconds) {
        return None;
    }
    // Guarded above: at most 86,400,000, far below u64::MAX. Millisecond resolution is rounded.
    Some((seconds * 1000.0).round() as u64)
}
