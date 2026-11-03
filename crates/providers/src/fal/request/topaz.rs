//! One bounded Precision Standard V2 output stays within one started 24 MP price block.
use super::*;

#[derive(Serialize)]
struct Body<'a> {
    image_url: &'a str,
    model: &'static str,
    upscale_factor: u64,
    output_format: &'static str,
    crop_to_fill: bool,
    enable_safety_checker: bool,
}

pub(super) fn prepare(j: &ProviderJob, l: Limits) -> Result<(Record, Vec<u8>), ProviderError> {
    if j.inputs.len() != 1 || j.params.len() > 1 || j.idempotency_key.len() > 256 {
        return Err(invalid("Topaz needs one image and an optional upscale factor"));
    }
    let Some(crate::JobInput::Media { role, mime, bytes }) = j.inputs.first() else {
        return Err(invalid("Topaz accepts one named raster image; text is not supported"));
    };
    if role != "image" || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") || bytes.len() > l.input_bytes {
        return Err(invalid("Topaz needs a PNG, JPEG or WebP image of at most 1 MiB"));
    }
    if j.params.keys().any(|key| key != "upscale_factor") {
        return Err(invalid("Topaz model, PNG output, uncropped geometry and safety are fixed"));
    }
    if j.model.len().saturating_add(j.idempotency_key.len()).saturating_add(role.len()).saturating_add(mime.len()) > l.text_bytes {
        return Err(invalid("Topaz request metadata exceeds limit"));
    }
    let factor = integer(j, "upscale_factor", 2, 1, 4)?;
    let mut body = Body { image_url: "", model: "Standard V2", upscale_factor: factor, output_format: "png", crop_to_fill: false, enable_safety_checker: true };
    // Bound the complete JSON + data URI before codecs or owned encodings.
    let mut counter = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Topaz request exceeds limit"))?;
    let encoded_len = encoded(mime, bytes)?;
    let length = counter.n.checked_add(encoded_len).filter(|n| *n <= l.request_bytes).ok_or_else(|| invalid("Topaz encoded image exceeds limit"))?;
    let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Topaz input exceeds raster or 1024×1024 px limits"))?;
    if meta.mime != mime {
        return Err(invalid("Topaz input MIME mismatch"));
    }
    // The service gets original bytes. Never guess whether it applies EXIF transforms.
    if nodecraft_media::image_requires_orientation(bytes, &l.image(true)).map_err(|_| invalid("Topaz input orientation refused"))? {
        return Err(invalid("Topaz needs an upright image: apply its EXIF rotation or reflection and save it first"));
    }
    let factor = u32::try_from(factor).map_err(|_| invalid("Topaz upscale factor refused"))?;
    let width = meta.width.checked_mul(factor).ok_or_else(|| invalid("Topaz output width overflow"))?;
    let height = meta.height.checked_mul(factor).ok_or_else(|| invalid("Topaz output height overflow"))?;
    let pixels = u64::from(width).checked_mul(u64::from(height)).ok_or_else(|| invalid("Topaz output geometry overflow"))?;
    if width > l.output_width || height > l.output_height || pixels > l.total_pixels {
        return Err(invalid("Topaz output exceeds geometry limits before submit"));
    }
    observe(0);
    drop(nodecraft_media::decode_image(bytes, &l.image(true)).map_err(|_| invalid("Topaz input codec refused"))?);
    let image_url = uri(mime, bytes, encoded_len)?;
    body.image_url = &image_url;
    let mut out = Vec::new();
    reserve(&mut out, length)?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Topaz request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Topaz request extent changed"));
    }
    Ok((Record { model: Model::Topaz, requested: 1, width, height, duration_ms: 0 }, out))
}
