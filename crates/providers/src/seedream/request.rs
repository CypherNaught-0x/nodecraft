use super::*;
use crate::{HttpMethod, HttpRequest, JobInput};
use base64::Engine;
use std::io::Write;
struct File<'a> {
    mime: &'a str,
    bytes: &'a [u8],
}
pub(super) struct Prepared<'a> {
    prompt: &'a str,
    size: &'a str,
    files: Vec<File<'a>>,
    length: usize,
}
fn add(a: usize, b: usize) -> Result<usize, ProviderError> {
    a.checked_add(b).ok_or_else(|| invalid("Seedream request extent overflow"))
}
fn encoded(n: usize) -> Result<usize, ProviderError> {
    n.checked_add(2).and_then(|n| n.checked_div(3)).and_then(|n| n.checked_mul(4)).ok_or_else(|| invalid("Seedream base64 extent overflow"))
}
struct Sink {
    n: usize,
    cap: usize,
    bytes: Option<Vec<u8>>,
}
impl Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let n = self.n.checked_add(b.len()).filter(|n| *n <= self.cap).ok_or_else(|| std::io::Error::other("bounded request full"))?;
        if let Some(v) = &mut self.bytes {
            v.extend_from_slice(b);
        }
        self.n = n;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Prepared<'_> {
    fn prefix(&self, out: &mut Sink) -> std::io::Result<()> {
        out.write_all(b"{\"model\":")?;
        serde_json::to_writer(&mut *out, MODEL)?;
        out.write_all(b",\"prompt\":")?;
        serde_json::to_writer(&mut *out, self.prompt)?;
        out.write_all(b",\"size\":")?;
        serde_json::to_writer(&mut *out, self.size)?;
        out.write_all(b",\"stream\":false,\"sequential_image_generation\":\"disabled\",\"response_format\":\"b64_json\",\"watermark\":true")
    }
    fn measure(&self, l: Limits) -> Result<usize, ProviderError> {
        let mut out = Sink { n: 0, cap: l.request_bytes, bytes: None };
        self.prefix(&mut out).map_err(|_| invalid("Seedream request exceeds wire limit"))?;
        let mut n = out.n;
        if !self.files.is_empty() {
            n = add(n, b",\"image\":[".len())?;
            for (i, f) in self.files.iter().enumerate() {
                n = add(n, usize::from(i > 0) + 2 + "data:".len() + f.mime.len() + ";base64,".len())?;
                n = add(n, encoded(f.bytes.len())?)?;
            }
            n = add(n, 1)?;
        }
        n = add(n, 1)?;
        if n > l.request_bytes {
            return Err(invalid("Seedream request exceeds wire limit"));
        }
        Ok(n)
    }
    pub(super) fn encode(&self, key: &Secret, l: Limits) -> Result<HttpRequest, ProviderError> {
        let mut bytes = Vec::new();
        reserve(&mut bytes, self.length)?;
        observe(0);
        let mut out = Sink { n: 0, cap: self.length, bytes: Some(bytes) };
        let write = (|| -> std::io::Result<()> {
            self.prefix(&mut out)?;
            if !self.files.is_empty() {
                out.write_all(b",\"image\":[")?;
                for (i, f) in self.files.iter().enumerate() {
                    if i > 0 {
                        out.write_all(b",")?;
                    }
                    write!(out, "\"data:{};base64,", f.mime)?;
                    let len = encoded(f.bytes.len()).map_err(std::io::Error::other)?;
                    let mut scratch = Vec::new();
                    scratch.try_reserve_exact(len).map_err(|_| std::io::Error::other("base64 allocation"))?;
                    scratch.resize(len, 0);
                    let n =
                        base64::engine::general_purpose::STANDARD.encode_slice(f.bytes, &mut scratch).map_err(|_| std::io::Error::other("base64 encoding"))?;
                    out.write_all(scratch.get(..n).ok_or_else(|| std::io::Error::other("base64 extent"))?)?;
                    out.write_all(b"\"")?;
                }
                out.write_all(b"]")?;
            }
            out.write_all(b"}")
        })();
        write.map_err(|_| invalid("Seedream request encoding failed"))?;
        if out.n != self.length {
            return Err(invalid("Seedream request length disagrees with admission"));
        }
        Ok(HttpRequest { method: HttpMethod::Post, body: out.bytes.ok_or_else(|| invalid("Seedream request body unavailable"))?, ..HttpRequest::get(API) }
            .header("Content-Type", "application/json")
            .secret_header("Authorization", format!("Bearer {}", key.expose()))
            .timeout_ms(180000)
            .max_response_bytes(l.response_bytes))
    }
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared<'_>, ProviderError> {
    if job.model != MODEL || job.inputs.len() > 15 || job.params.len() > 1 || job.idempotency_key.len() > 512 {
        return Err(invalid("Seedream model, identity or input count is unsupported"));
    }
    if job.params.keys().any(|k| k != "size") {
        return Err(invalid("unknown Seedream option"));
    }
    let size = match job.params.get("size") {
        None => "2K",
        Some(v) => v.as_str().filter(|s| matches!(*s, "2K" | "4K")).ok_or_else(|| invalid("Seedream size must be 2K or 4K"))?,
    };
    // A lowered output geometry limit cannot silently clip an upstream-selected paid shape.
    if l.axis < 16384 || l.pixels < 16777216 {
        return Err(invalid("lowered geometry cannot admit Seedream variable output shapes"));
    }
    let mut p = Prepared { prompt: "", size, files: Vec::new(), length: 0 };
    reserve(&mut p.files, 14)?;
    let mut seen = false;
    let mut total = 0usize;
    let mut meta = job.model.len() + job.idempotency_key.len();
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" && !seen => {
                if text.is_empty() || text.len() > l.prompt_bytes || text.chars().take(l.prompt_chars.saturating_add(1)).count() > l.prompt_chars {
                    return Err(invalid("Seedream prompt exceeds supported limits"));
                }
                p.prompt = text;
                seen = true;
                meta = add(meta, add(role.len(), text.len())?)?;
            }
            JobInput::Media { role, mime, bytes } if role == "image" => {
                if p.files.len() >= 14 || bytes.len() > l.input_bytes || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
                    return Err(invalid("Seedream reference format, count or bytes unsupported"));
                }
                total = add(total, bytes.len())?;
                meta = add(meta, add(role.len(), mime.len())?)?;
                p.files.push(File { mime, bytes });
            }
            _ => return Err(invalid("Seedream needs one prompt and ordered image references")),
        }
    }
    if !seen || total > l.total_input_bytes || meta > l.metadata_bytes {
        return Err(invalid("Seedream aggregate input limits exceeded"));
    }
    p.length = p.measure(l)?;
    let mut pixels = 0u64;
    for f in &p.files {
        let m = nodecraft_media::probe_image(f.bytes, &l.image(true)).map_err(|_| invalid("Seedream reference header exceeds codec limits"))?;
        if m.mime != f.mime {
            return Err(invalid("Seedream reference MIME mismatch"));
        }
        pixels = pixels
            .checked_add(geometry(m.width, m.height, l, true)?)
            .filter(|n| *n <= l.total_pixels)
            .ok_or_else(|| invalid("Seedream aggregate pixels exceeded"))?;
    }
    for f in &p.files {
        observe(1);
        let _ = nodecraft_media::decode_image(f.bytes, &l.image(true)).map_err(|_| invalid("Seedream reference is corrupt or exceeds codec limits"))?;
    }
    Ok(p)
}
