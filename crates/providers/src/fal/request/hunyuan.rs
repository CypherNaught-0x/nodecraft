//! Hunyuan3D V3's three distinct schemas, with fixed default face count and priced mesh presets.
use super::*;

const PRESETS: &[&str] = &["textured", "pbr", "low-poly-triangles", "low-poly-quads", "low-poly-triangles-pbr", "low-poly-quads-pbr", "geometry"];

#[derive(Serialize)]
struct Body<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_image_url: Option<&'a str>,
    enable_safety_checker: bool,
    enable_pbr: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    generate_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    polygon_type: Option<&'static str>,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits, model: Model) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() > 2 || j.params.len() > 1 || j.idempotency_key.len() > 256 {
        return Err(invalid("Hunyuan3D input count or identity exceeds limit"));
    }
    let mut strings = j.model.len().saturating_add(j.idempotency_key.len());
    let mut prompt = None;
    let mut image = None;
    for input in &j.inputs {
        match input {
            crate::JobInput::Text { role, text } => {
                strings = strings.saturating_add(role.len()).saturating_add(text.len());
                if model == Model::HunyuanImage || role != "prompt" || prompt.replace(text.as_str()).is_some() {
                    return Err(invalid("Hunyuan3D requires exactly the named inputs for this mode"));
                }
            }
            crate::JobInput::Media { role, mime, bytes } => {
                strings = strings.saturating_add(role.len()).saturating_add(mime.len());
                if model == Model::HunyuanText || role != "image" || bytes.len() > l.input_bytes || image.replace((mime.as_str(), bytes.as_ref())).is_some() {
                    return Err(invalid("Hunyuan3D image role, count or size refused"));
                }
            }
        }
    }
    for (key, value) in &j.params {
        strings = strings.saturating_add(key.len()).saturating_add(value.as_str().map_or(0, str::len));
        if key != "mesh_preset" {
            return Err(invalid("Hunyuan3D unsupported parameter"));
        }
    }
    if strings > l.text_bytes {
        return Err(invalid("Hunyuan3D text metadata exceeds limit"));
    }
    let prompt = if model == Model::HunyuanImage {
        None
    } else {
        let text = prompt.map(str::trim).filter(|text| !text.is_empty()).ok_or_else(|| invalid("Hunyuan3D prompt required"))?;
        if text.chars().count() > 1024 {
            return Err(invalid("Hunyuan3D prompt exceeds 1024 characters"));
        }
        Some(text)
    };
    if model != Model::HunyuanText && image.is_none() {
        return Err(invalid("Hunyuan3D image required"));
    }
    let allowed = if model == Model::HunyuanSketch { &["textured", "pbr"][..] } else { PRESETS };
    let preset = choice(j, "mesh_preset", "textured", allowed)?;
    let (generate_type, polygon_type, pbr) = match preset {
        "textured" => ("Normal", None, false),
        "pbr" => ("Normal", None, true),
        "low-poly-triangles" => ("LowPoly", Some("triangle"), false),
        "low-poly-quads" => ("LowPoly", Some("quadrilateral"), false),
        "low-poly-triangles-pbr" => ("LowPoly", Some("triangle"), true),
        "low-poly-quads-pbr" => ("LowPoly", Some("quadrilateral"), true),
        "geometry" => ("Geometry", None, false),
        _ => return Err(invalid("Hunyuan3D mesh preset refused")),
    };
    let mut body = Body {
        prompt,
        input_image_url: image.map(|_| ""),
        enable_safety_checker: true,
        enable_pbr: pbr,
        generate_type: (model != Model::HunyuanSketch).then_some(generate_type),
        polygon_type,
    };
    // The provider's 500K default is deliberately omitted; custom counts carry another fee.
    // Admit escaped JSON plus the entire data URI before codecs or owned base64 allocations.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Hunyuan3D escaped request exceeds limit"))?;
    let image_len = image.map(|(mime, bytes)| encoded(mime, bytes)).transpose()?;
    let length =
        counter.n.checked_add(image_len.unwrap_or(0)).filter(|n| *n <= l.request_bytes).ok_or_else(|| invalid("Hunyuan3D encoded request exceeds limit"))?;
    if let Some((mime, bytes)) = image {
        let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Hunyuan3D image exceeds limits"))?;
        if meta.mime != mime || !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
            return Err(invalid("Hunyuan3D image MIME mismatch"));
        }
        if model == Model::HunyuanSketch && (meta.width < 128 || meta.height < 128) {
            return Err(invalid("Hunyuan3D sketches require at least 128 pixels on each axis"));
        }
        observe(0);
        drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Hunyuan3D image codec refused"))?);
    }
    let image_uri = image.zip(image_len).map(|((mime, bytes), size)| uri(mime, bytes, size)).transpose()?;
    body.input_image_url = image_uri.as_deref();
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Hunyuan3D request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Hunyuan3D request extent changed"));
    }
    Ok((Record { model, requested: 1, width: 0, height: 0, duration_ms: 0 }, out))
}
