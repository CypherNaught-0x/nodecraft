use super::*;
pub(super) struct File {
    pub url: String,
    pub mime: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u64>,
    pub height: Option<u64>,
}
fn delivery(s: &str, key: &Secret) -> Result<(), ProviderError> {
    if s.contains(key.expose()) {
        return Err(malformed("fal output URL refused or redacted"));
    }
    storage(s)
}
/// Whether `s` is a link to fal's file storage.
pub(super) fn storage(s: &str) -> Result<(), ProviderError> {
    if s.len() > 4096 || s.bytes().any(|b| b.is_ascii_control() || b == b'\\') {
        return Err(malformed("fal output URL refused or redacted"));
    }
    let u = url::Url::parse(s).map_err(|_| malformed("fal output URL invalid"))?;
    let host = u.host_str().unwrap_or_default();
    // Check raw authority as well: URL normalization removes an explicit default port.
    let authority = s.strip_prefix("https://").and_then(|s| s.split('/').next()).ok_or_else(|| malformed("fal output scheme refused"))?;
    if authority != host
        || u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
        || u.fragment().is_some()
        || !(host == "fal.media" || host.ends_with(".fal.media"))
        || !host
            .split('.')
            .all(|s| !s.is_empty() && s.len() <= 63 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !s.starts_with('-') && !s.ends_with('-'))
    {
        return Err(malformed("fal output origin refused"));
    }
    Ok(())
}
pub(super) fn outputs(b: &[u8], r: Record, l: Limits, key: &Secret) -> Result<Vec<File>, ProviderError> {
    let root = json::parse(b, l)?;
    if root.get("error").is_some_and(|v| v.get() != "null") {
        return Err(malformed("fal result contains an error instead of a complete output"));
    }
    let name = match r.model {
        Model::Image | Model::Styled | Model::QwenImage | Model::QwenEdit | Model::GrokImage | Model::GrokEdit | Model::Photon | Model::Ideogram => "images",
        Model::RodinText | Model::RodinImages | Model::RodinFastText | Model::RodinFastImages => "model_mesh",
        Model::Train => "diffusers_lora_file",
        Model::Wan3Text
        | Model::Wan3Frames
        | Model::GrokText
        | Model::GrokFrames
        | Model::Video
        | Model::Frames
        | Model::Speech
        | Model::RayText
        | Model::RayFrames
        | Model::LtxText
        | Model::LtxFrames => "video",
        Model::Audio => "audio",
        Model::HunyuanText | Model::HunyuanImage | Model::HunyuanSketch | Model::Mesh => "model_glb",
        Model::Upscale | Model::RemoveBackground | Model::Topaz => "image",
    };
    // Multiple model envelopes or unexpected top-level media cannot be filtered away.
    if matches!(r.model, Model::RayText | Model::RayFrames) && root.get("exr_file").is_some_and(|v| v.get() != "null") {
        return Err(malformed("Luma returned an unrequested EXR sidecar"));
    }
    for k in ["images", "video", "audio", "model_glb", "model_mesh", "image", "diffusers_lora_file"] {
        if k != name && root.get(k).is_some() {
            return Err(malformed("fal conflicting output envelopes"));
        }
    }
    let raw = root.get(name).ok_or_else(|| malformed("fal required output missing"))?;
    let mut records = Vec::new();
    reserve(&mut records, 4)?;
    if r.model.images() {
        records = json::files(raw)?;
        if records.len() > usize::from(r.requested) {
            return Err(malformed("fal image count exceeds requested count"));
        }
    } else {
        records.push(raw);
    }
    let mut out = Vec::new();
    reserve(&mut out, records.len())?;
    let mut declared = 0u64;
    let mut pixels = 0u64;
    for raw in records {
        let f = json::object(raw.get())?;
        let url = f.text("url")?;
        delivery(&url, key)?;
        if r.model == Model::Train {
            // Only the link is kept, so the weights' size and type are fal's; the link must be
            // one a descriptor may hold.
            if !nodecraft_media::style::plain_https(&url) {
                return Err(malformed("fal trained weights link carries a query or fragment"));
            }
            out.push(File { url: url.into_owned(), mime: None, size: None, width: None, height: None });
            continue;
        }
        // A clip's generic `application/octet-stream` declares nothing; its bytes are still
        // checked as MP4 after download.
        let mime = f.optional_text("content_type")?.filter(|m| !((r.model.video() || r.model.rodin()) && m == "application/octet-stream"));
        if mime.as_ref().is_some_and(|s| s.len() > 128) {
            return Err(malformed("fal MIME too long"));
        }
        // Names are not routing or media authority, but malformed typed metadata still refuses.
        let _ = f.optional_text("file_name")?;
        let size = f.optional_u64("file_size")?;
        if let Some(n) = size {
            declared = declared.checked_add(n).ok_or_else(|| malformed("fal declared size overflow"))?;
            if n == 0 || n > l.output_bytes as u64 || declared > l.total_output_bytes as u64 {
                return Err(malformed("fal declared outputs exceed quota"));
            }
        }
        let width = f.optional_u64("width")?;
        let height = f.optional_u64("height")?;
        if matches!(r.model, Model::Image | Model::Styled) && (width.is_none() || height.is_none()) {
            return Err(malformed("fal image dimensions required"));
        }
        if width.is_some_and(|n| n == 0 || n > u64::from(l.output_width)) || height.is_some_and(|n| n == 0 || n > u64::from(l.output_height)) {
            return Err(malformed("fal declared geometry exceeds quota"));
        }
        if let (Some(w), Some(h)) = (width, height) {
            pixels = w
                .checked_mul(h)
                .and_then(|n| pixels.checked_add(n))
                .filter(|n| *n <= l.total_pixels)
                .ok_or_else(|| malformed("fal declared aggregate geometry exceeds quota"))?;
        }
        let allowed: &[&str] = match r.model {
            Model::Image | Model::Styled | Model::Upscale | Model::RemoveBackground | Model::Topaz => &["image/png"],
            Model::QwenImage | Model::QwenEdit | Model::GrokImage | Model::GrokEdit | Model::Photon | Model::Ideogram => {
                &["image/png", "image/jpeg", "image/webp"]
            }
            Model::Wan3Text
            | Model::Wan3Frames
            | Model::GrokText
            | Model::GrokFrames
            | Model::Video
            | Model::Frames
            | Model::Speech
            | Model::RayText
            | Model::RayFrames
            | Model::LtxText
            | Model::LtxFrames => &["video/mp4"],
            Model::Audio => &["audio/wav"],
            Model::RodinText
            | Model::RodinImages
            | Model::RodinFastText
            | Model::RodinFastImages
            | Model::HunyuanText
            | Model::HunyuanImage
            | Model::HunyuanSketch
            | Model::Mesh => &["model/gltf-binary"],
            Model::Train => return Err(malformed("fal trained weights are never downloaded")),
        };
        if mime.as_deref().is_some_and(|s| !allowed.contains(&s)) {
            return Err(malformed("fal output category/MIME refused before download"));
        }
        out.push(File { url: url.into_owned(), mime: mime.map(|s| s.into_owned()), size, width, height });
    }
    Ok(out)
}
/// The units fal says it billed the request (#437), from the `X-Fal-Billable-Units` header of the
/// authenticated result fetch; only fal's API answers it, never the media CDN. The header is
/// optional evidence: absent, repeated, oversized or not a plain decimal, it is left out and the
/// result is kept. https://fal.ai/docs/documentation/model-apis/common-parameters#response-headers
pub(super) fn billable_units(r: &HttpResponse) -> Option<BillableUnits> {
    let mut values = r.headers.iter().filter(|(name, _)| name.eq_ignore_ascii_case("x-fal-billable-units")).map(|(_, value)| value);
    let (Some(value), None) = (values.next(), values.next()) else { return None };
    // HTTP allows spaces and tabs around a field value.
    let value = value.trim_matches([' ', '\t']);
    if value.len() > 32 {
        return None;
    }
    BillableUnits::parse(value).ok()
}
