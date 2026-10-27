//! Recraft's one-result standard contracts: Fal controls only, with safe explicit presets.
use super::*;

#[derive(Serialize)]
struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}
#[derive(Serialize)]
struct Body<'a> {
    prompt: &'a str,
    image_size: &'a str,
    colors: Vec<Rgb>,
    #[serde(skip_serializing_if = "Option::is_none")]
    background_color: Option<Rgb>,
    enable_safety_checker: bool,
}
fn rgb(value: &str) -> Result<Rgb, ProviderError> {
    let hex = value
        .strip_prefix('#')
        .filter(|s| s.len() == 6 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("Recraft colors must be #RRGGBB without alpha"))?;
    // ASCII admission above establishes both byte and UTF-8 boundaries.
    let channel = |at: usize| hex.get(at..at + 2).and_then(|s| u8::from_str_radix(s, 16).ok()).ok_or_else(|| invalid("Recraft invalid RGB color"));
    Ok(Rgb { r: channel(0)?, g: channel(2)?, b: channel(4)? })
}
fn setting<'a>(j: &'a ProviderJob, name: &str, max: usize) -> Result<Option<&'a str>, ProviderError> {
    j.params.get(name).map(|v| v.as_str().filter(|s| s.len() <= max).ok_or_else(|| invalid("Recraft color controls require bounded text"))).transpose()
}
pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() != 1 || j.params.len() > 3 || j.idempotency_key.len() > 256 {
        return Err(invalid("Recraft requires one prompt and only its documented color/size controls"));
    }
    let [crate::JobInput::Text { role, text }] = j.inputs.as_slice() else {
        return Err(invalid("Recraft accepts text prompts only; rasterize SVG before other image models"));
    };
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len()).saturating_add(role.len()).saturating_add(text.len());
    for (name, value) in &j.params {
        if !matches!(name.as_str(), "image_size" | "colors" | "background_color") {
            return Err(invalid("Recraft unsupported control; one output and safety are fixed, Fal has no seed control"));
        }
        strings = strings.saturating_add(name.len()).saturating_add(value.as_str().map_or(0, str::len));
    }
    if strings > l.text_bytes || role != "prompt" {
        return Err(invalid("Recraft named prompt or aggregate text limit refused"));
    }
    let prompt = text.trim();
    if !(1..=10_000).contains(&prompt.chars().count()) {
        return Err(invalid("Recraft prompt must contain 1 to 10000 characters"));
    }
    let image_size = choice(j, "image_size", "square_hd", &["square_hd", "square", "portrait_4_3", "portrait_16_9", "landscape_4_3", "landscape_16_9"])?;
    let (width, height) = match image_size {
        "square" => (512u64, 512u64),
        "portrait_4_3" => (768, 1024),
        "portrait_16_9" => (576, 1024),
        "landscape_4_3" => (1024, 768),
        "landscape_16_9" => (1024, 576),
        _ => (1024, 1024),
    };
    if width > u64::from(l.output_width) || height > u64::from(l.output_height) || width.checked_mul(height).is_none_or(|pixels| pixels > l.total_pixels) {
        return Err(invalid("Recraft requested geometry exceeds host limits before payment"));
    }
    let mut colors = Vec::new();
    reserve(&mut colors, 6)?;
    if let Some(text) = setting(j, "colors", 47)?.filter(|s| !s.is_empty()) {
        for color in text.split(',') {
            if colors.len() == 6 {
                return Err(invalid("Recraft supports up to six preferred colors in this host"));
            }
            colors.push(rgb(color)?);
        }
    }
    let background_color = setting(j, "background_color", 7)?.filter(|s| !s.is_empty()).map(rgb).transpose()?;
    let body = Body { prompt, image_size, colors, background_color, enable_safety_checker: true };
    // Complete escaped representation admission precedes the request's owned allocation.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Recraft escaped request exceeds limit"))?;
    let mut bytes = Vec::new();
    reserve(&mut bytes, counter.n)?;
    serde_json::to_writer(&mut bytes, &body).map_err(|_| invalid("Recraft request encoding failed"))?;
    if bytes.len() != counter.n {
        return Err(invalid("Recraft request extent changed"));
    }
    Ok((Record { model, requested: 1, width: 0, height: 0 }, bytes))
}
