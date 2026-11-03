use super::*;
use base64::Engine;
use serde::Serialize;

#[derive(Serialize)]
struct Body<'a> {
    image: &'a str,
    scale_factor: u32,
    flavor: &'static str,
    sharpen: u32,
    smart_grain: u32,
    ultra_detail: u32,
    filter_nsfw: bool,
}
struct Count {
    bytes: usize,
    cap: usize,
}
impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.checked_add(bytes.len()).filter(|n| *n <= self.cap).ok_or_else(|| std::io::Error::other("request limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn integer(job: &ProviderJob, key: &str, default: u32, min: u32, max: u32) -> Result<u32, ProviderError> {
    let n = job
        .params
        .get(key)
        .map(|value| value.as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| invalid("Magnific controls require integers")))
        .transpose()?
        .unwrap_or(default);
    if !(min..=max).contains(&n) {
        return Err(invalid("Magnific control outside supported range"));
    }
    Ok(n)
}
pub(super) fn prepare(job: &ProviderJob, limits: Limits) -> Result<(Contract, Vec<u8>), ProviderError> {
    // This service documents server-side credentials; browser projects cannot retain large
    // outputs either. Refuse before invoking even a caller-supplied transport.
    #[cfg(target_arch = "wasm32")]
    if job.model == PRECISION_V2 {
        return Err(invalid("Magnific Precision needs a desktop project so its large image results can be saved"));
    }
    if job.model != PRECISION_V2 || job.inputs.len() != 1 || job.params.len() > 4 || job.idempotency_key.len() > 256 {
        return Err(invalid("Choose Magnific Precision V2 with one image and supported controls"));
    }
    if job.params.keys().any(|key| !matches!(key.as_str(), "scale_factor" | "sharpen" | "smart_grain" | "ultra_detail")) {
        return Err(invalid("Magnific flavor and screening are fixed; prompts, seeds and output overrides are unsupported"));
    }
    let Some(crate::JobInput::Media { role, mime, bytes }) = job.inputs.first() else {
        return Err(invalid("Magnific needs one named raster image"));
    };
    if role != "image" || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") || bytes.len() > limits.input_bytes {
        return Err(invalid("Magnific needs a PNG, JPEG or WebP image of at most 1 MiB"));
    }
    let factor = match job.params.get("scale_factor").and_then(|value| value.as_str()) {
        Some("2") => 2,
        Some("4") => 4,
        Some(_) => return Err(invalid("Magnific scale must be 2 or 4")),
        None => integer(job, "scale_factor", 2, 2, 4)?,
    };
    if !matches!(factor, 2 | 4) {
        return Err(invalid("Magnific supports 2× or 4× scaling in this client"));
    }
    let mut body = Body {
        image: "",
        scale_factor: factor,
        flavor: "photo",
        sharpen: integer(job, "sharpen", 7, 0, 100)?,
        smart_grain: integer(job, "smart_grain", 7, 0, 100)?,
        ultra_detail: integer(job, "ultra_detail", 30, 0, 100)?,
        filter_nsfw: true,
    };
    let encoded =
        bytes.len().checked_add(2).and_then(|n| n.checked_div(3)).and_then(|n| n.checked_mul(4)).ok_or_else(|| invalid("Magnific encoded image overflow"))?;
    let mut counter = Count { bytes: 0, cap: limits.request_bytes };
    serde_json::to_writer(&mut counter, &body).map_err(|_| invalid("Magnific request exceeds limit"))?;
    let length = counter.bytes.checked_add(encoded).filter(|n| *n <= limits.request_bytes).ok_or_else(|| invalid("Magnific encoded request exceeds limit"))?;
    let meta = nodecraft_media::probe_image(bytes, &limits.image(true)).map_err(|_| invalid("Magnific input exceeds raster or 1024×1024 px limits"))?;
    if meta.mime != mime {
        return Err(invalid("Magnific input MIME mismatch"));
    }
    media::complete(bytes, mime).map_err(|_| invalid("Magnific needs a complete, static PNG, JPEG or WebP image"))?;
    if nodecraft_media::image_requires_orientation(bytes, &limits.image(true)).map_err(|_| invalid("Magnific input orientation refused"))? {
        return Err(invalid("Magnific needs an upright image: apply its EXIF rotation or reflection and save it first"));
    }
    let width = meta.width.checked_mul(factor).ok_or_else(|| invalid("Magnific output geometry overflow"))?;
    let height = meta.height.checked_mul(factor).ok_or_else(|| invalid("Magnific output geometry overflow"))?;
    let contract = Contract { width, height, limits };
    contract.validate()?;
    drop(nodecraft_media::decode_image(bytes, &limits.image(true)).map_err(|_| invalid("Magnific input codec refused"))?);
    let mut image = String::new();
    image.try_reserve_exact(encoded).map_err(|_| invalid("Magnific image encoding allocation refused"))?;
    base64::engine::general_purpose::STANDARD.encode_string(bytes, &mut image);
    body.image = &image;
    let mut out = Vec::new();
    out.try_reserve_exact(length).map_err(|_| invalid("Magnific request allocation refused"))?;
    serde_json::to_writer(&mut out, &body).map_err(|_| invalid("Magnific request encoding failed"))?;
    if out.len() != length {
        return Err(invalid("Magnific request extent changed"));
    }
    Ok((contract, out))
}
