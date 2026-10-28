//! Rodin 2.5's four fixed-price schemas. Paid tiers and add-ons are not caller-controlled.
use super::*;

#[derive(Serialize)]
struct Body<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<&'a str>,
    tier: &'static str,
    quality_mesh_option: &'static str,
    material: &'static str,
    geometry_file_format: &'static str,
    enable_safety_checker: bool,
    enable_creative_mode: bool,
    hd_texture: bool,
    texture_delight: bool,
    is_micro: bool,
    #[serde(rename = "TAPose")]
    ta_pose: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_urls: Option<Vec<&'a str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    use_original_alpha: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_render: Option<bool>,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    let images_mode = matches!(model, Model::RodinImages | Model::RodinFastImages);
    let fast = matches!(model, Model::RodinFastText | Model::RodinFastImages);
    if j.inputs.len() > if images_mode { 6 } else { 1 } || j.params.len() > if images_mode { 2 } else { 1 } || j.idempotency_key.len() > 256 {
        return Err(invalid("Rodin input count or identity exceeds limit"));
    }
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut images = Vec::new();
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                strings = strings.saturating_add(role.len()).saturating_add(text.len());
                if role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Rodin takes at most one named prompt"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if !images_mode || role != "images" || bytes.len() > l.input_bytes {
                    return Err(invalid("Rodin image role or input size refused"));
                }
                images.push((mime.as_str(), bytes.as_ref()));
            }
        }
    }
    for (name, value) in &j.params {
        strings = strings.saturating_add(name.len()).saturating_add(value.as_str().map_or(0, str::len));
        if name != "seed" && !(images_mode && name == "use_original_alpha") {
            return Err(invalid("Rodin unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("Rodin text metadata exceeds limit"));
    }
    let prompt = prompt.map(str::trim);
    if !images_mode && prompt.is_none_or(str::is_empty) {
        return Err(invalid("Rodin text mode requires a prompt"));
    }
    if prompt.is_some_and(|text| text.chars().count() > 1024) {
        return Err(invalid("Rodin prompt exceeds 1024 characters"));
    }
    if images_mode && !(1..=5).contains(&images.len()) {
        return Err(invalid("Rodin image mode requires one to five ordered images"));
    }
    let seed = j.params.get("seed").map(|v| v.as_u64().filter(|n| *n <= 65535).ok_or_else(|| invalid("Rodin seed must be 0 to 65535"))).transpose()?;
    let use_original_alpha = if images_mode {
        Some(
            j.params
                .get("use_original_alpha")
                .map(|v| v.as_bool().ok_or_else(|| invalid("Rodin alpha preservation must be boolean")))
                .transpose()?
                .unwrap_or(false),
        )
    } else {
        None
    };
    let mut body = Body {
        prompt,
        tier: if fast { "Gen-2.5-Extreme-Low" } else { "Gen-2.5-High" },
        quality_mesh_option: "Auto",
        material: if fast { "Shaded" } else { "PBR" },
        geometry_file_format: "glb",
        enable_safety_checker: true,
        enable_creative_mode: false,
        hd_texture: false,
        texture_delight: false,
        is_micro: false,
        ta_pose: false,
        seed,
        image_urls: images_mode.then(|| vec![""; images.len()]),
        use_original_alpha,
        preview_render: images_mode.then_some(false),
    };
    // Price and the entire escaped JSON/base64 extent are fixed before any image codec
    // or owned data URI is created. The placeholder strings already include their quotes.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Rodin escaped request exceeds limit"))?;
    let lengths: Vec<usize> = images.iter().map(|(mime, bytes)| encoded(mime, bytes)).collect::<Result<_, _>>()?;
    let length = lengths
        .iter()
        .try_fold(counter.n, |n, size| n.checked_add(*size))
        .filter(|n| *n <= l.request_bytes)
        .ok_or_else(|| invalid("Rodin encoded request exceeds limit"))?;
    for (mime, bytes) in &images {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Rodin reference exceeds image limits"))?;
        if meta.mime != *mime || !matches!(*mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(invalid("Rodin reference MIME mismatch"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Rodin reference codec refused"))?);
    }
    let uris: Vec<String> = images.iter().zip(lengths).map(|((mime, bytes), size)| uri(mime, bytes, size)).collect::<Result<_, _>>()?;
    body.image_urls = images_mode.then(|| uris.iter().map(String::as_str).collect());
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Rodin request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Rodin encoded request extent changed"));
    }
    Ok((Record { model, requested: 1, width: 0, height: 0, duration_ms: 0 }, out))
}
