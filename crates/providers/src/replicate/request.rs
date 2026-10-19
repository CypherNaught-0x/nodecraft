use super::*;
use crate::JobInput;
use base64::Engine;
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
struct Params {
    width: u32,
    height: u32,
    num_outputs: u8,
    num_inference_steps: u16,
    guidance_scale: f64,
    prompt_strength: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i64>,
    scheduler: &'static str,
    refine: &'static str,
    apply_watermark: bool,
    disable_safety_checker: bool,
}
#[derive(Serialize)]
struct Input<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    negative_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mask: Option<&'a str>,
    #[serde(flatten)]
    params: Option<Params>,
}
#[derive(Serialize)]
struct Wire<'a> {
    version: &'static str,
    input: Input<'a>,
}
#[derive(Clone, Copy)]
struct Media<'a> {
    bytes: &'a [u8],
    mime: &'static str,
}
pub(super) struct Prepared<'a> {
    pub(super) record: Record,
    text: &'a str,
    negative: Option<&'a str>,
    params: Option<Params>,
    image: Option<Media<'a>>,
    mask: Option<Media<'a>>,
    length: usize,
}
fn integer(job: &ProviderJob, name: &str, default: u64, min: u64, max: u64) -> Result<u64, ProviderError> {
    let n = match job.params.get(name) {
        Some(v) => v.as_u64().ok_or_else(|| invalid("Replicate integer parameter must be an unsigned integer"))?,
        None => default,
    };
    if n < min || n > max {
        return Err(invalid("Replicate integer parameter is outside its supported range"));
    }
    Ok(n)
}
fn number(job: &ProviderJob, name: &str, default: f64, min: f64, max: f64) -> Result<f64, ProviderError> {
    let n = match job.params.get(name) {
        Some(v) => v.as_f64().ok_or_else(|| invalid("Replicate numeric parameter must be a finite number"))?,
        None => default,
    };
    if !n.is_finite() || n < min || n > max {
        return Err(invalid("Replicate numeric parameter is outside its supported range"));
    }
    Ok(n)
}
fn encoded(n: usize) -> Result<usize, ProviderError> {
    n.checked_add(2).and_then(|n| n.checked_div(3)).and_then(|n| n.checked_mul(4)).ok_or_else(|| invalid("Replicate data URI length overflow"))
}
fn uri_len(m: Media<'_>) -> Result<usize, ProviderError> {
    5usize
        .checked_add(m.mime.len())
        .and_then(|n| n.checked_add(8))
        .and_then(|n| n.checked_add(encoded(m.bytes.len()).ok()?))
        .ok_or_else(|| invalid("Replicate data URI length overflow"))
}
fn uri(m: Media<'_>) -> Result<String, ProviderError> {
    let n = uri_len(m)?;
    let mut s = String::new();
    s.try_reserve_exact(n).map_err(|_| invalid("Replicate input encoding allocation refused"))?;
    s.push_str("data:");
    s.push_str(m.mime);
    s.push_str(";base64,");
    observe(2);
    base64::engine::general_purpose::STANDARD.encode_string(m.bytes, &mut s);
    Ok(s)
}
struct Count {
    n: usize,
    cap: usize,
}
impl std::io::Write for Count {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.n = self.n.checked_add(b.len()).filter(|n| *n <= self.cap).ok_or_else(|| std::io::Error::other("request cap"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Bound the supported response serializer's escaped string content before allocating it.
fn echo_text(value: &str) -> Result<usize, ProviderError> {
    value.chars().try_fold(0usize, |n, c| {
        n.checked_add(match c {
            '\u{0}'..='\u{1f}' | '\u{7f}' => 6,
            '"' | '\\' | '/' => 2,
            c if c.is_ascii() => 1,
            c if u32::from(c) <= 0xffff => 6,
            _ => 12,
        })
        .ok_or_else(|| invalid("Replicate echoed text extent overflow"))
    })
}
fn serialized_length<T: Serialize>(value: &T) -> Result<usize, ProviderError> {
    let mut count = Count { n: 0, cap: usize::MAX };
    serde_json::to_writer(&mut count, value).map_err(|_| invalid("Replicate echo counting failed"))?;
    Ok(count.n)
}
fn numeric_extra<T: Serialize>(value: &T) -> Result<usize, ProviderError> {
    64usize.checked_sub(serialized_length(value)?).ok_or_else(|| invalid("Replicate numeric echo exceeds scalar allowance"))
}

impl Prepared<'_> {
    fn wire<'a>(&'a self, image: Option<&'a str>, mask: Option<&'a str>) -> Wire<'a> {
        Wire {
            version: self.record.model.wire(),
            input: Input {
                text: (self.record.model == Model::Text).then_some(self.text),
                prompt: (self.record.model == Model::Image).then_some(self.text),
                negative_prompt: self.negative,
                image,
                mask,
                params: self.params,
            },
        }
    }
    pub(super) fn echo_bound(&self) -> Result<usize, ProviderError> {
        let mut n = serialized_length(&self.wire(self.image.map(|_| ""), self.mask.map(|_| "")).input)?;
        for text in [Some(self.text), self.negative].into_iter().flatten() {
            let actual = serialized_length(&text)?.checked_sub(2).ok_or_else(|| invalid("invalid JSON text extent"))?;
            let extra = echo_text(text)?.checked_sub(actual).ok_or_else(|| invalid("invalid JSON echo extent"))?;
            n = n.checked_add(extra).ok_or_else(|| invalid("Replicate echo extent overflow"))?;
        }
        for m in [self.image, self.mask].into_iter().flatten() {
            // Existing placeholders include quotes. Reserve optional slash escaping on every
            // base64 byte, plus the MIME slash in the otherwise literal ASCII prefix.
            n = n
                .checked_add(uri_len(m)?)
                .and_then(|n| n.checked_add(encoded(m.bytes.len()).ok()?))
                .and_then(|n| n.checked_add(1))
                .ok_or_else(|| invalid("Replicate data URI echo overflow"))?;
        }
        if let Some(p) = self.params {
            for extra in [
                numeric_extra(&p.width)?,
                numeric_extra(&p.height)?,
                numeric_extra(&p.num_outputs)?,
                numeric_extra(&p.num_inference_steps)?,
                numeric_extra(&p.guidance_scale)?,
                numeric_extra(&p.prompt_strength)?,
            ] {
                n = n.checked_add(extra).ok_or_else(|| invalid("Replicate scalar echo overflow"))?;
            }
            if let Some(seed) = p.seed {
                n = n.checked_add(numeric_extra(&seed)?).ok_or_else(|| invalid("Replicate seed echo overflow"))?;
            }
            // Known absent SDXL defaults/finite normalization: inventory documented in plan/docs.
            n = n.checked_add(2048).ok_or_else(|| invalid("Replicate normalization extent overflow"))?;
        }
        Ok(n)
    }
    pub(super) fn encode(&self, l: Limits) -> Result<Vec<u8>, ProviderError> {
        if self.length > l.request_bytes {
            return Err(invalid("Replicate encoded request exceeds its limit"));
        }
        let image = self.image.map(uri).transpose()?;
        let mask = self.mask.map(uri).transpose()?;
        let mut body = Vec::new();
        reserve(&mut body, self.length)?;
        observe(3);
        serde_json::to_writer(&mut body, &self.wire(image.as_deref(), mask.as_deref())).map_err(|_| invalid("Replicate request encoding failed"))?;
        if body.len() != self.length {
            return Err(invalid("Replicate request encoding extent changed"));
        }
        Ok(body)
    }
}
pub(super) fn prepare(job: &ProviderJob, l: Limits) -> Result<Prepared<'_>, ProviderError> {
    let model = Model::from_wire(&job.model)?;
    if job.inputs.len() > 4 || job.params.len() > 32 || job.idempotency_key.len() > 256 {
        return Err(invalid("Replicate input metadata exceeds its limits"));
    }
    let mut text = None;
    let mut negative = None;
    let mut image = None;
    let mut mask = None;
    let mut metadata = job.model.len().checked_add(job.idempotency_key.len()).ok_or_else(|| invalid("Replicate metadata overflow"))?;
    let mut total_bytes = 0usize;
    for input in &job.inputs {
        let role = match input {
            JobInput::Text { role, .. } | JobInput::Media { role, .. } => role,
        };
        metadata = metadata.checked_add(role.len()).ok_or_else(|| invalid("Replicate metadata overflow"))?;
        match input {
            JobInput::Text { role, text: value } => {
                metadata = metadata.checked_add(value.len()).ok_or_else(|| invalid("Replicate metadata overflow"))?;
                if value.len() > 128 * 1024 {
                    return Err(invalid("Replicate input text exceeds 128 KiB"));
                }
                let target = match (model, role.as_str()) {
                    (Model::Text, "text" | "prompt") => &mut text,
                    (Model::Image, "prompt") => &mut text,
                    (Model::Image, "negative_prompt") => &mut negative,
                    _ => return Err(invalid("unsupported Replicate text input role")),
                };
                if target.replace(value.as_str()).is_some() {
                    return Err(invalid("duplicate Replicate text input role"));
                }
            }
            JobInput::Media { role, mime, bytes } => {
                if model != Model::Image {
                    return Err(invalid("text model does not accept media"));
                }
                metadata = metadata.checked_add(mime.len()).ok_or_else(|| invalid("Replicate metadata overflow"))?;
                if bytes.len() > l.input_bytes {
                    return Err(invalid("Replicate input exceeds local 256,000-byte image limit"));
                }
                total_bytes = total_bytes.checked_add(bytes.len()).ok_or_else(|| invalid("Replicate input extent overflow"))?;
                if total_bytes > l.total_input_bytes {
                    return Err(invalid("Replicate aggregate input capacity exceeded"));
                }
                let target = match role.as_str() {
                    "image" => &mut image,
                    "mask" => &mut mask,
                    _ => return Err(invalid("unsupported Replicate media role")),
                };
                if target.replace((mime.as_str(), bytes.as_ref())).is_some() {
                    return Err(invalid("duplicate Replicate media role"));
                }
            }
        }
        if metadata > l.metadata_bytes {
            return Err(invalid("Replicate metadata capacity exceeded"));
        }
    }
    let text = text.filter(|s| !s.trim().is_empty()).ok_or_else(|| invalid("Replicate requires a nonempty prompt or text input"))?;
    if mask.is_some() && image.is_none() {
        return Err(invalid("Replicate mask requires an image"));
    }
    let params = if model == Model::Image {
        for key in job.params.keys() {
            if !matches!(key.as_str(), "width" | "height" | "num_outputs" | "num_inference_steps" | "guidance_scale" | "prompt_strength" | "seed") {
                return Err(invalid("unsupported Replicate parameter"));
            }
        }
        let width = integer(job, "width", 1024, 512, 1024)?;
        let height = integer(job, "height", 1024, 512, 1024)?;
        if ![512, 768, 1024].contains(&width) || ![512, 768, 1024].contains(&height) {
            return Err(invalid("Replicate dimensions must be 512, 768 or 1024"));
        }
        let seed = job.params.get("seed").map(|v| v.as_i64().ok_or_else(|| invalid("Replicate seed must be a signed 64-bit integer"))).transpose()?;
        Some(Params {
            width: u32::try_from(width).map_err(|_| invalid("invalid width"))?,
            height: u32::try_from(height).map_err(|_| invalid("invalid height"))?,
            num_outputs: u8::try_from(integer(job, "num_outputs", 1, 1, 4)?).map_err(|_| invalid("invalid count"))?,
            num_inference_steps: u16::try_from(integer(job, "num_inference_steps", 50, 1, 100)?).map_err(|_| invalid("invalid steps"))?,
            guidance_scale: number(job, "guidance_scale", 7.5, 1.0, 50.0)?,
            prompt_strength: number(job, "prompt_strength", 0.8, 0.0, 1.0)?,
            seed,
            scheduler: "K_EULER",
            refine: "no_refiner",
            apply_watermark: true,
            disable_safety_checker: false,
        })
    } else {
        if !job.params.is_empty() {
            return Err(invalid("text model has no scalar parameters"));
        }
        None
    };
    // Probe every input and its aggregate geometry before allocating any decoded pixel buffer.
    let mut dims = None;
    let mut total_pixels = 0u64;
    let mut media = [None, None];
    for (i, source) in [image, mask].into_iter().enumerate() {
        if let Some((claimed, bytes)) = source {
            observe(0);
            let meta = nodecraft_media::probe_image(bytes, &l.image(true)).map_err(|_| invalid("Replicate input is not a bounded PNG/JPEG/WebP image"))?;
            if claimed != meta.mime || !matches!(meta.mime, "image/png" | "image/jpeg" | "image/webp") || (i == 1 && meta.mime != "image/png") {
                return Err(invalid("Replicate input MIME or mask format does not match its bytes"));
            }
            total_pixels = total_pixels.checked_add(extent(&meta, l)?).ok_or_else(|| invalid("Replicate pixel extent overflow"))?;
            if total_pixels > l.total_pixels {
                return Err(invalid("Replicate aggregate input geometry exceeds limits"));
            }
            if i == 0 {
                dims = Some((meta.width, meta.height));
            } else if dims != Some((meta.width, meta.height)) {
                return Err(invalid("Replicate mask dimensions must match its image"));
            }
            if let Some(slot) = media.get_mut(i) {
                *slot = Some(Media { bytes, mime: meta.mime });
            }
        }
    }
    let mut prepared = Prepared {
        record: Record { model, requested: params.map_or(1, |p| p.num_outputs) },
        text,
        negative,
        params,
        image: media.first().copied().flatten(),
        mask: media.get(1).copied().flatten(),
        length: 0,
    };
    // Empty string placeholders include exactly the same JSON keys/quotes; URI ASCII needs no
    // JSON escaping. Add its checked extent without base64 encoding or proportional allocation.
    let mut count = Count { n: 0, cap: l.request_bytes };
    serde_json::to_writer(&mut count, &prepared.wire(prepared.image.map(|_| ""), prepared.mask.map(|_| "")))
        .map_err(|_| invalid("Replicate encoded request exceeds its limit"))?;
    for m in media.iter().flatten() {
        count.n = count.n.checked_add(uri_len(*m)?).filter(|n| *n <= l.request_bytes).ok_or_else(|| invalid("Replicate encoded request exceeds its limit"))?;
    }
    prepared.length = count.n;
    let (input_cap, _) = response::regions(prepared.record, l)?;
    if prepared.echo_bound()? > input_cap {
        return Err(invalid("Replicate input cannot fit its bounded prediction response echo"));
    }
    for m in media.iter().flatten() {
        observe(1);
        let _ = nodecraft_media::decode_image(m.bytes, &l.image(true)).map_err(|_| invalid("Replicate input is corrupt or exceeds codec limits"))?;
    }
    Ok(prepared)
}
