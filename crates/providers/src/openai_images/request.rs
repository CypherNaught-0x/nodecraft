use super::*;
use crate::{HttpMethod, HttpRequest, JobInput};
use serde::Serialize;
use std::io::Write;

struct File<'a> {
    mime: &'a str,
    bytes: &'a [u8],
    mask: bool,
}
pub(super) struct Prepared<'a> {
    model: &'a str,
    prompt: &'a str,
    size: &'a str,
    quality: &'a str,
    pub(super) n: usize,
    pub(super) format: &'a str,
    pub(super) background: &'a str,
    compression: Option<u64>,
    files: Vec<File<'a>>,
    boundary: Option<String>,
    length: usize,
}
fn choice<'a>(job: &'a ProviderJob, key: &str, default: &'a str, choices: &[&str]) -> Result<&'a str, ProviderError> {
    let value = match job.params.get(key) {
        None => default,
        Some(v) => v.as_str().ok_or_else(|| invalid("image option has the wrong type"))?,
    };
    if !choices.contains(&value) {
        return Err(invalid("image option is not supported by this model"));
    }
    Ok(value)
}
pub(super) fn prepare<'a>(job: &'a ProviderJob, l: &Limits) -> Result<Prepared<'a>, ProviderError> {
    if !matches!(job.model.as_str(), SUNBURST | FLARE) {
        return Err(invalid("unsupported image model; choose a dated GPT Image 2.5 catalog entry"));
    }
    if job.inputs.len() > 18 || job.params.len() > 16 {
        return Err(invalid("too many image inputs or options"));
    }
    let mut metadata = job.model.len();
    for (key, value) in &job.params {
        if !matches!(key.as_str(), "size" | "quality" | "background" | "n" | "output_format" | "output_compression") {
            return Err(invalid("unknown image option"));
        }
        let value_len = match value {
            serde_json::Value::String(s) => s.len(),
            serde_json::Value::Number(_) => 32,
            _ => return Err(invalid("image option has the wrong type")),
        };
        metadata = metadata.checked_add(key.len()).and_then(|v| v.checked_add(value_len)).ok_or_else(|| invalid("image metadata overflow"))?;
    }
    let size = choice(job, "size", "1024x1024", &["1024x1024", "1536x1024", "1024x1536", "auto"])?;
    let quality = choice(job, "quality", "auto", &["low", "medium", "high", "xhigh", "max", "auto"])?;
    let background = choice(job, "background", "auto", &["opaque", "transparent", "auto"])?;
    let format = choice(job, "output_format", "png", &["png", "jpeg", "webp"])?;
    if background == "transparent" && format == "jpeg" {
        return Err(invalid("transparent images require PNG or WebP output"));
    }
    let n = job.params.get("n").map(|v| v.as_u64().ok_or_else(|| invalid("image count must be an integer"))).transpose()?.unwrap_or(1);
    if !(1..=10).contains(&n) {
        return Err(invalid("image count must be between 1 and 10"));
    }
    let compression = job
        .params
        .get("output_compression")
        .map(|v| v.as_u64().filter(|n| *n <= 100).ok_or_else(|| invalid("image compression must be an integer from 0 to 100")))
        .transpose()?;
    if compression.is_some() && format == "png" {
        return Err(invalid("compression applies only to JPEG or WebP output"));
    }
    let mut prompt = None;
    let mut refs = Vec::new();
    let mut mask = None;
    let mut bytes = 0usize;
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && prompt.is_none() => {
                if text.is_empty() || text.len() > l.prompt_bytes || text.chars().take(l.prompt_chars.saturating_add(1)).count() > l.prompt_chars {
                    return Err(invalid("image prompt exceeds the character or byte limit"));
                }
                metadata = metadata.checked_add(role.len()).and_then(|v| v.checked_add(text.len())).ok_or_else(|| invalid("image metadata overflow"))?;
                prompt = Some(text.as_str());
            }
            JobInput::Media { role, mime, bytes: data } if matches!(role.as_str(), "image" | "reference" | "mask") => {
                if data.len() > l.input_bytes || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
                    return Err(invalid("image input format or byte limit is unsupported"));
                }
                bytes = bytes.checked_add(data.len()).ok_or_else(|| invalid("image input extent overflow"))?;
                metadata = metadata.checked_add(role.len()).and_then(|v| v.checked_add(mime.len())).ok_or_else(|| invalid("image metadata overflow"))?;
                if role == "mask" {
                    if mask.is_some() {
                        return Err(invalid("only one image mask is supported"));
                    }
                    mask = Some(File { mime, bytes: data, mask: true });
                } else {
                    refs.push(File { mime, bytes: data, mask: false });
                }
            }
            _ => return Err(invalid("image inputs require one prompt, ordered references and at most one mask")),
        }
    }
    if refs.len() > 16 || bytes > l.total_input_bytes || metadata > l.metadata_bytes {
        return Err(invalid("image request exceeds aggregate input limits"));
    }
    let prompt = prompt.ok_or_else(|| invalid("image request needs one prompt"))?;
    if mask.is_some() && refs.is_empty() {
        return Err(invalid("an image mask requires a first reference image"));
    }
    if let Some(mask) = mask {
        refs.push(mask);
    }
    let mut prepared = Prepared {
        model: &job.model,
        prompt,
        size,
        quality,
        background,
        format,
        n: usize::try_from(n).map_err(|_| invalid("image count overflow"))?,
        compression,
        files: refs,
        boundary: None,
        length: 0,
    };
    // Complete metadata/extent and wire-size admission precedes any pixel or payload copy.
    if !prepared.files.is_empty() {
        prepared.boundary = Some(prepared.choose_boundary()?);
    }
    prepared.length = prepared.measure(l)?;
    let mut total_pixels = 0u64;
    let mut first = None;
    for file in &prepared.files {
        let meta = nodecraft_media::probe_image(file.bytes, &l.image(true)).map_err(|_| invalid("image input could not be read within codec limits"))?;
        if meta.mime != file.mime {
            return Err(invalid("image MIME does not match its bytes"));
        }
        total_pixels = total_pixels.checked_add(pixels(&meta, l)?).ok_or_else(|| invalid("image pixel extent overflow"))?;
        if total_pixels > l.total_pixels {
            return Err(invalid("image request exceeds the aggregate pixel limit"));
        }
        if file.mask {
            if meta.mime != "image/png" || !meta.has_alpha || first != Some(("image/png", meta.width, meta.height)) {
                return Err(invalid("mask and first reference must be matching PNG images; the mask needs alpha"));
            }
        } else if first.is_none() {
            first = Some((meta.mime, meta.width, meta.height));
        }
    }
    // Probe all inputs before full decoding any one of them; scratch is released each iteration.
    for file in &prepared.files {
        let _ = nodecraft_media::decode_image(file.bytes, &l.image(true)).map_err(|_| invalid("image input is corrupt or exceeds codec limits"))?;
    }
    Ok(prepared)
}
#[derive(Serialize)]
struct Generation<'a> {
    model: &'a str,
    prompt: &'a str,
    size: &'a str,
    quality: &'a str,
    background: &'a str,
    n: usize,
    output_format: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_compression: Option<u64>,
}
struct Sink {
    len: usize,
    cap: usize,
    bytes: Option<Vec<u8>>,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self.len.checked_add(bytes.len()).filter(|n| *n <= self.cap).ok_or_else(|| std::io::Error::other("image request exceeds its wire limit"))?;
        if let Some(out) = &mut self.bytes {
            out.extend_from_slice(bytes);
        }
        self.len = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Prepared<'_> {
    fn generation(&self) -> Generation<'_> {
        Generation {
            model: self.model,
            prompt: self.prompt,
            size: self.size,
            quality: self.quality,
            background: self.background,
            n: self.n,
            output_format: self.format,
            output_compression: self.compression,
        }
    }
    fn choose_boundary(&self) -> Result<String, ProviderError> {
        let mut digest = blake3::Hasher::new();
        digest.update(self.prompt.as_bytes());
        for f in &self.files {
            digest.update(f.bytes);
        }
        let hash = digest.finalize().to_hex();
        self.boundary_for_seed(hash.as_str())
    }
    fn boundary_for_seed(&self, hash: &str) -> Result<String, ProviderError> {
        for n in 0..8 {
            let b = format!("----NodeCraft{hash}-{n}");
            if !self.prompt.as_bytes().windows(b.len()).any(|w| w == b.as_bytes())
                && !self.files.iter().any(|f| f.bytes.windows(b.len()).any(|w| w == b.as_bytes()))
            {
                return Ok(b);
            }
        }
        Err(invalid("could not choose a bounded image upload boundary"))
    }
    fn multipart(&self, out: &mut Sink, boundary: &str) -> std::io::Result<()> {
        // Only prompt is variable-length; field names and every filename/MIME are fixed admitted data.
        for (name, value) in [
            ("model", self.model),
            ("prompt", self.prompt),
            ("size", self.size),
            ("quality", self.quality),
            ("background", self.background),
            ("output_format", self.format),
        ] {
            write!(out, "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")?;
        }
        write!(out, "--{boundary}\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n{}\r\n", self.n)?;
        if let Some(n) = self.compression {
            write!(out, "--{boundary}\r\nContent-Disposition: form-data; name=\"output_compression\"\r\n\r\n{n}\r\n")?;
        }
        for (i, f) in self.files.iter().enumerate() {
            let (name, ext) = (
                if f.mask { "mask" } else { "image[]" },
                match f.mime {
                    "image/jpeg" => "jpeg",
                    "image/webp" => "webp",
                    _ => "png",
                },
            );
            write!(out, "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"image{i:03}.{ext}\"\r\nContent-Type: {}\r\n\r\n", f.mime)?;
            out.write_all(f.bytes)?;
            out.write_all(b"\r\n")?;
        }
        write!(out, "--{boundary}--\r\n")
    }
    fn measure(&self, l: &Limits) -> Result<usize, ProviderError> {
        let mut sink = Sink { len: 0, cap: l.request_bytes, bytes: None };
        if self.files.is_empty() {
            serde_json::to_writer(&mut sink, &self.generation()).map_err(|_| invalid("image request exceeds its wire limit"))?;
        } else {
            self.multipart(&mut sink, self.boundary.as_deref().ok_or_else(|| invalid("image boundary is missing"))?)
                .map_err(|_| invalid("image request exceeds its wire limit"))?;
        }
        Ok(sink.len)
    }
    pub(super) fn encode(&self, key: &Secret, l: &Limits) -> Result<HttpRequest, ProviderError> {
        let length = self.length;
        let mut bytes = Vec::new();
        reserve(&mut bytes, length)?;
        observe(0);
        let mut sink = Sink { len: 0, cap: length, bytes: Some(bytes) };
        let (path, ct) = if self.files.is_empty() {
            serde_json::to_writer(&mut sink, &self.generation()).map_err(|_| invalid("image request encoding failed"))?;
            ("/images/generations", "application/json".to_owned())
        } else {
            let b = self.boundary.as_deref().ok_or_else(|| invalid("image boundary is missing"))?;
            self.multipart(&mut sink, b).map_err(|_| invalid("image request encoding failed"))?;
            ("/images/edits", format!("multipart/form-data; boundary={b}"))
        };
        let body = sink.bytes.ok_or_else(|| invalid("image encoder has no output buffer"))?;
        Ok(HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get(format!("{ORIGIN}{path}")) }
            .header("Content-Type", ct)
            .secret_header("Authorization", format!("Bearer {}", key.expose()))
            .timeout_ms(180_000)
            .max_response_bytes(l.response_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_boundary_scan_skips_collisions_and_stops_after_eight_candidates() {
        let make = |prompt| Prepared {
            model: SUNBURST,
            prompt,
            size: "1024x1024",
            quality: "low",
            n: 1,
            format: "png",
            background: "auto",
            compression: None,
            files: vec![],
            boundary: None,
            length: 0,
        };
        assert_eq!(make("----NodeCraftfixture-0").boundary_for_seed("fixture").unwrap(), "----NodeCraftfixture-1");
        let all = (0..8).map(|n| format!("----NodeCraftfixture-{n}")).collect::<Vec<_>>().join(" ");
        assert!(make(&all).boundary_for_seed("fixture").is_err());
    }
    #[test]
    fn exact_wire_budget_is_inclusive_and_one_less_refuses_before_encode() {
        let j = ProviderJob {
            model: SUNBURST.into(),
            params: serde_json::Map::new(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: "escaped\n\"雪".into() }],
            idempotency_key: String::new(),
        };
        let p = prepare(&j, &Limits::default()).unwrap();
        let mut l = Limits { request_bytes: p.length, ..Limits::default() };
        let p = prepare(&j, &l).unwrap();
        assert_eq!(p.encode(&Secret::new("fixture").unwrap(), &l).unwrap().body.len(), l.request_bytes);
        l.request_bytes -= 1;
        super::super::COUNTERS.with(|c| c.set((0, 0, 0, 0)));
        assert!(prepare(&j, &l).is_err());
        assert_eq!(super::super::COUNTERS.with(|c| c.get()).0, 0);
    }
}
