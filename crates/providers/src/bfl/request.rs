use super::*;
use crate::JobInput;
use serde_json::{Value, json};

pub(super) fn geometry(width: u64, height: u64) -> Result<(), ProviderError> {
    if !(64..=2048).contains(&width)
        || !(64..=2048).contains(&height)
        || width % 16 != 0
        || height % 16 != 0
        || width.checked_mul(height).is_none_or(|pixels| pixels > PIXELS)
    {
        return Err(invalid("Black Forest Labs dimensions must be multiples of 16 from 64 to 2048, at most 4 million pixels"));
    }
    Ok(())
}
pub(super) fn body(job: &ProviderJob) -> Result<(Value, u64, u64), ProviderError> {
    if job.model != MODEL || job.inputs.len() != 1 || job.params.len() > 3 || job.idempotency_key.len() > 256 {
        return Err(invalid("Black Forest Labs supports one FLUX.2 klein 4B text prompt and bounded parameters"));
    }
    let Some(JobInput::Text { role, text }) = job.inputs.first() else { return Err(invalid("Black Forest Labs requires a text prompt")) };
    if role != "prompt" || text.len() > 16 * 1024 || text.trim().is_empty() || text.contains('\0') {
        return Err(invalid("Black Forest Labs prompt must be nonempty text up to 16 KiB without NUL"));
    }
    if job.params.keys().any(|key| !matches!(key.as_str(), "width" | "height" | "seed")) {
        return Err(invalid("Black Forest Labs supports only width, height and seed parameters"));
    }
    let integer = |key, default| match job.params.get(key) {
        Some(value) => value.as_u64().ok_or_else(|| invalid("Black Forest Labs parameters must be unsigned integers")),
        None => Ok(default),
    };
    let width = integer("width", 1024)?;
    let height = integer("height", 1024)?;
    geometry(width, height)?;
    let mut body = json!({"prompt":text,"width":width,"height":height,"output_format":"png"});
    if job.params.contains_key("seed") {
        let seed = integer("seed", 0)?;
        if seed > u64::from(u32::MAX) {
            return Err(invalid("Black Forest Labs seed exceeds 32 bits"));
        }
        if let Some(object) = body.as_object_mut() {
            object.insert("seed".into(), Value::from(seed));
        }
    }
    Ok((body, width, height))
}
