use super::*;
use crate::{HttpMethod, HttpRequest, JobInput};
use base64::Engine;
use std::io::Write;
struct Image<'a> {
    mime: &'a str,
    bytes: &'a [u8],
}
enum Input<'a> {
    Text(&'a str),
    Image(Image<'a>),
}
pub(super) struct Prepared<'a> {
    model: &'a str,
    size: &'a str,
    aspect: &'a str,
    inputs: Vec<Input<'a>>,
    length: usize,
}
fn choice<'a>(job: &'a ProviderJob, key: &str, default: &'a str, choices: &[&str]) -> Result<&'a str, ProviderError> {
    let s = match job.params.get(key) {
        None => default,
        Some(v) => v.as_str().ok_or_else(|| invalid("Gemini option must be text"))?,
    };
    if !choices.contains(&s) {
        return Err(invalid("Gemini option is unsupported for this model"));
    }
    Ok(s)
}
pub(super) fn prepare<'a>(job: &'a ProviderJob, l: &Limits) -> Result<Prepared<'a>, ProviderError> {
    if !matches!(job.model.as_str(), NANO | LITE | PRO) {
        return Err(invalid("select a supported Gemini image catalog model"));
    }
    if job.inputs.len() > 15 || job.params.len() > 2 {
        return Err(invalid("too many Gemini inputs or options"));
    }
    let mut metadata = job.model.len();
    for (k, v) in &job.params {
        if !matches!(k.as_str(), "image_size" | "aspect_ratio") {
            return Err(invalid("unknown Gemini image option"));
        }
        let s = v.as_str().ok_or_else(|| invalid("Gemini option must be text"))?;
        add(&mut metadata, k.len(), l.metadata_bytes)?;
        add(&mut metadata, s.len(), l.metadata_bytes)?;
    }
    let size = choice(job, "image_size", "1K", if job.model == LITE { &["1K"] } else { &["1K", "2K", "4K"] })?;
    let aspect = choice(job, "aspect_ratio", "1:1", ASPECTS)?;
    let mut images = 0usize;
    let mut prompts = 0usize;
    let mut total = 0usize;
    // First pass is borrowed only; even the last hostile input is admitted before descriptors/copies.
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } if role == "prompt" => {
                prompts += 1;
                if text.is_empty() || text.len() > l.prompt_bytes || text.chars().take(l.prompt_chars.saturating_add(1)).count() > l.prompt_chars {
                    return Err(invalid("Gemini prompt exceeds its character or byte limit"));
                }
                add(&mut metadata, role.len(), l.metadata_bytes)?;
                add(&mut metadata, text.len(), l.metadata_bytes)?;
            }
            JobInput::Media { role, mime, bytes } if matches!(role.as_str(), "image" | "reference") => {
                images += 1;
                if bytes.len() > l.input_bytes || !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
                    return Err(invalid("Gemini reference format or bytes exceed limits"));
                }
                add(&mut total, bytes.len(), l.total_input_bytes)?;
                add(&mut metadata, role.len(), l.metadata_bytes)?;
                add(&mut metadata, mime.len(), l.metadata_bytes)?;
            }
            _ => return Err(invalid("Gemini needs one prompt and ordered image references; masks and conversations are unsupported")),
        }
    }
    if prompts != 1 || images > 14 {
        return Err(invalid("Gemini needs one prompt and at most fourteen references"));
    }
    let mut inputs = Vec::new();
    reserve(&mut inputs, job.inputs.len())?;
    for input in &job.inputs {
        match input {
            JobInput::Text { text, .. } => inputs.push(Input::Text(text)),
            JobInput::Media { mime, bytes, .. } => inputs.push(Input::Image(Image { mime, bytes })),
        }
    }
    let mut p = Prepared { model: &job.model, size, aspect, inputs, length: 0 };
    let mut count = Count { length: 0, cap: l.request_bytes };
    p.write(&mut count).map_err(|_| invalid("Gemini request body exceeds its byte limit"))?;
    p.length = count.length;
    let mut total_pixels = 0u64;
    for i in &p.inputs {
        if let Input::Image(i) = i {
            let m = nodecraft_media::probe_image(i.bytes, &l.image(true)).map_err(|_| invalid("Gemini reference cannot be read within codec limits"))?;
            if m.mime != i.mime {
                return Err(invalid("Gemini reference MIME disagrees with its bytes"));
            }
            total_pixels = total_pixels
                .checked_add(pixels(&m, l)?)
                .filter(|n| *n <= l.total_pixels)
                .ok_or_else(|| invalid("Gemini references exceed aggregate pixels"))?;
        }
    }
    for i in &p.inputs {
        if let Input::Image(i) = i {
            observe(false);
            let _ = nodecraft_media::decode_image(i.bytes, &l.image(true)).map_err(|_| invalid("Gemini reference is corrupt or exceeds codec limits"))?;
        }
    }
    Ok(p)
}
struct Count {
    length: usize,
    cap: usize,
}
impl Write for Count {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.length = self.length.checked_add(b.len()).filter(|n| *n <= self.cap).ok_or_else(|| std::io::Error::other("request extent"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn json(w: &mut dyn Write, s: &str) -> std::io::Result<()> {
    serde_json::to_writer(w, s).map_err(std::io::Error::other)
}
impl Prepared<'_> {
    fn write(&self, w: &mut dyn Write) -> std::io::Result<()> {
        w.write_all(b"{\"model\":")?;
        json(w, self.model)?;
        w.write_all(b",\"store\":false,\"background\":false,\"stream\":false,\"generation_config\":{\"thinking_summaries\":\"none\"},\"response_format\":{\"type\":\"image\",\"delivery\":\"inline\",\"mime_type\":\"image/jpeg\",\"image_size\":")?;
        json(w, self.size)?;
        w.write_all(b",\"aspect_ratio\":")?;
        json(w, self.aspect)?;
        w.write_all(b"},\"input\":[")?;
        for (n, input) in self.inputs.iter().enumerate() {
            if n > 0 {
                w.write_all(b",")?;
            }
            match input {
                Input::Text(text) => {
                    w.write_all(b"{\"type\":\"text\",\"text\":")?;
                    json(w, text)?;
                    w.write_all(b"}")?;
                }
                Input::Image(i) => {
                    w.write_all(b"{\"type\":\"image\",\"mime_type\":")?;
                    json(w, i.mime)?;
                    w.write_all(b",\"data\":\"")?;
                    // encode_string would retain a whole payload. A fixed 3072-byte quantum produces at most
                    // 4096 ASCII bytes with no JSON escaping, identically in measurement and encoding.
                    for chunk in i.bytes.chunks(3072) {
                        let mut encoded = [0u8; 4096];
                        let n =
                            base64::engine::general_purpose::STANDARD.encode_slice(chunk, &mut encoded).map_err(|_| std::io::Error::other("base64 extent"))?;
                        w.write_all(encoded.get(..n).ok_or_else(|| std::io::Error::other("base64 extent"))?)?;
                    }
                    w.write_all(b"\"}")?;
                }
            }
        }
        w.write_all(b"]}")
    }
    pub(super) fn encode(&self, key: &Secret, l: &Limits) -> Result<HttpRequest, ProviderError> {
        if self.length > l.request_bytes {
            return Err(invalid("Gemini request exceeds body limit"));
        }
        let mut body = Vec::new();
        reserve(&mut body, self.length)?;
        observe(true);
        self.write(&mut body).map_err(|_| invalid("Gemini request encoding failed"))?;
        if body.len() != self.length {
            return Err(invalid("Gemini measured request extent changed"));
        }
        Ok(HttpRequest { method: HttpMethod::Post, body, ..HttpRequest::get("https://generativelanguage.googleapis.com/v1beta/interactions") }
            .header("Content-Type", "application/json")
            .header("User-Agent", crate::USER_AGENT)
            .secret_header("x-goog-api-key", key.expose())
            .timeout_ms(120000)
            .max_response_bytes(l.response_bytes))
    }
}
