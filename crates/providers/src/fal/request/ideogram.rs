//! Fixed Balanced/count contract keeps the catalog's per-call estimate valid.
use super::*;

#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    negative_prompt: Option<&'a str>,
    image_size: &'a str,
    style: &'a str,
    expand_prompt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    rendering_speed: &'static str,
    num_images: u8,
    sync_mode: bool,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() > 2 || j.params.len() > 4 || j.idempotency_key.len() > 256 {
        return Err(invalid("Ideogram input count or identity exceeds limit"));
    }
    let mut text_bytes = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut negative_prompt = None;
    for input in &j.inputs {
        let crate::JobInput::Text { role, text } = input else { return Err(invalid("Ideogram accepts text inputs only")) };
        text_bytes = text_bytes.saturating_add(role.len()).saturating_add(text.len());
        let target = match role.as_str() {
            "prompt" => &mut prompt,
            "negative_prompt" => &mut negative_prompt,
            _ => return Err(invalid("Ideogram requires named prompt inputs")),
        };
        if target.replace(text.as_str()).is_some() {
            return Err(invalid("Ideogram duplicate input role"));
        }
    }
    for (key, value) in &j.params {
        text_bytes = text_bytes.saturating_add(key.len()).saturating_add(value.as_str().map_or(0, str::len));
        if !matches!(key.as_str(), "seed" | "image_size" | "style" | "expand_prompt") {
            return Err(invalid("Ideogram unsupported parameter; rendering speed and image count are fixed"));
        }
    }
    if text_bytes > l.text_bytes {
        return Err(invalid("Ideogram text metadata exceeds limit"));
    }
    let prompt = prompt.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| invalid("Ideogram prompt required"))?;
    let image_size = choice(j, "image_size", "square_hd", &["square_hd", "square", "portrait_4_3", "portrait_16_9", "landscape_4_3", "landscape_16_9"])?;
    let style = choice(j, "style", "AUTO", &["AUTO", "GENERAL", "REALISTIC", "DESIGN"])?;
    let expand_prompt =
        j.params.get("expand_prompt").map(|v| v.as_bool().ok_or_else(|| invalid("Ideogram MagicPrompt must be boolean"))).transpose()?.unwrap_or(true);
    // A conservative signed-32-bit seed subset, shared by the catalog and direct adapter.
    let seed = j.params.contains_key("seed").then(|| integer(j, "seed", 0, 0, 2_147_483_647)).transpose()?;
    let body = Body { prompt, negative_prompt, image_size, style, expand_prompt, seed, rendering_speed: "BALANCED", num_images: 1, sync_mode: false };
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Ideogram escaped request exceeds limit"))?;
    let mut out = Vec::new();
    reserve(&mut out, counter.n)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Ideogram request encoding failed"))?;
    Ok((Record { model: Model::Ideogram, requested: 1, width: 0, height: 0, duration_ms: 0 }, out))
}
