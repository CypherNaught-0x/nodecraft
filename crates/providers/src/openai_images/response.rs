use super::*;
use crate::{HttpResponse, Output, Usage};
use base64::Engine;
use serde::Deserialize;
use std::borrow::Cow;

// This borrowed lexical pass precedes serde's potentially allocating escape decoder. It bounds
// unknown fields too; Cow by itself would be too late. Nesting is capped BEFORE recursion.
struct Lex<'a> {
    text: &'a str,
    at: usize,
    fields: usize,
    values: usize,
    strings: usize,
    metadata: usize,
    n: usize,
    blob: bool,
    limits: &'a Limits,
}
impl Lex<'_> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn ws(&mut self) {
        while self.byte().is_some_and(|b| matches!(b, b' ' | b'\r' | b'\n' | b'\t')) {
            self.at += 1;
        }
    }
    fn require(&mut self, b: u8) -> Result<(), ProviderError> {
        self.ws();
        if self.byte() != Some(b) {
            return Err(malformed("invalid image response JSON"));
        }
        self.at += 1;
        Ok(())
    }
    fn hex(&mut self) -> Result<u32, ProviderError> {
        let mut n = 0u32;
        for _ in 0..4 {
            let b = self.byte().ok_or_else(|| malformed("truncated JSON escape"))?;
            let digit = match b {
                b'0'..=b'9' => u32::from(b - b'0'),
                b'a'..=b'f' => u32::from(b - b'a') + 10,
                b'A'..=b'F' => u32::from(b - b'A') + 10,
                _ => return Err(malformed("invalid JSON escape")),
            };
            n = n * 16 + digit;
            self.at += 1;
        }
        Ok(n)
    }
    fn string(&mut self, key: bool, blob: bool) -> Result<&str, ProviderError> {
        self.require(b'"')?;
        let start = self.at;
        let mut decoded = 0usize;
        let cap = if key {
            256
        } else if blob && self.blob {
            encoded_limit(self.limits.output_bytes)?
        } else {
            self.limits.metadata_bytes
        };
        loop {
            let b = self.byte().ok_or_else(|| malformed("truncated image JSON string"))?;
            if b == b'"' {
                let end = self.at;
                self.at += 1;
                self.strings = self.strings.checked_add(decoded).ok_or_else(|| malformed("image string extent overflow"))?;
                if self.strings > 64 * MIB + self.limits.metadata_bytes {
                    return Err(malformed("image JSON strings exceed their aggregate limit"));
                }
                if !(blob && self.blob) {
                    self.metadata = self.metadata.checked_add(decoded).ok_or_else(|| malformed("image metadata overflow"))?;
                    if self.metadata > self.limits.metadata_bytes {
                        return Err(malformed("image metadata exceeds its limit"));
                    }
                }
                return self.text.get(start..end).ok_or_else(|| malformed("invalid JSON string boundary"));
            }
            let add = match b {
                0..=31 => return Err(malformed("control character in image JSON")),
                b'\\' => {
                    if key {
                        return Err(malformed("escaped image response field names are unsupported"));
                    }
                    self.at += 1;
                    let e = self.byte().ok_or_else(|| malformed("truncated JSON escape"))?;
                    self.at += 1;
                    match e {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => 1,
                        b'u' => {
                            let mut scalar = self.hex()?;
                            if (0xD800..=0xDBFF).contains(&scalar) {
                                if self.byte() != Some(b'\\') {
                                    return Err(malformed("unpaired JSON surrogate"));
                                }
                                self.at += 1;
                                if self.byte() != Some(b'u') {
                                    return Err(malformed("unpaired JSON surrogate"));
                                }
                                self.at += 1;
                                let low = self.hex()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err(malformed("unpaired JSON surrogate"));
                                }
                                scalar = 0x10000 + (scalar - 0xD800) * 1024 + (low - 0xDC00);
                            }
                            char::from_u32(scalar).ok_or_else(|| malformed("invalid JSON scalar"))?.len_utf8()
                        }
                        _ => return Err(malformed("invalid JSON escape")),
                    }
                }
                _ => {
                    let c = self.text.get(self.at..).and_then(|s| s.chars().next()).ok_or_else(|| malformed("invalid JSON UTF8"))?;
                    self.at += c.len_utf8();
                    c.len_utf8()
                }
            };
            decoded = decoded.checked_add(add).ok_or_else(|| malformed("image string extent overflow"))?;
            if decoded > cap {
                return Err(malformed("image JSON string exceeds its limit before unescaping"));
            }
        }
    }
    fn value(&mut self, depth: usize, blob: bool, data: bool) -> Result<(), ProviderError> {
        if depth > 16 {
            return Err(malformed("image JSON is too deeply nested"));
        }
        self.values += 1;
        if self.values > 256 {
            return Err(malformed("too many image JSON values"));
        }
        self.ws();
        match self.byte() {
            Some(b'"') => {
                self.string(false, blob)?;
            }
            Some(b'{') => {
                self.at += 1;
                self.ws();
                if self.byte() == Some(b'}') {
                    self.at += 1;
                    return Ok(());
                }
                loop {
                    self.fields += 1;
                    if self.fields > 64 {
                        return Err(malformed("too many image JSON fields"));
                    }
                    let key = self.string(true, false)?;
                    let (is_blob, is_data) = (key == "b64_json", key == "data");
                    self.require(b':')?;
                    self.value(depth + 1, is_blob, is_data)?;
                    self.ws();
                    match self.byte() {
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(malformed("invalid image JSON object")),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                self.ws();
                if self.byte() == Some(b']') {
                    self.at += 1;
                    return Ok(());
                }
                let mut count = 0;
                loop {
                    count += 1;
                    if count > if data { self.n } else { 64 } {
                        return Err(malformed("too many image response entries"));
                    }
                    self.value(depth + 1, false, false)?;
                    self.ws();
                    match self.byte() {
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(malformed("invalid image JSON array")),
                    }
                }
            }
            Some(_) => {
                let start = self.at;
                while self.byte().is_some_and(|b| !matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}')) {
                    self.at += 1;
                    if self.at - start > 64 {
                        return Err(malformed("image JSON scalar exceeds its limit"));
                    }
                }
                if start == self.at {
                    return Err(malformed("missing image JSON value"));
                }
            }
            None => return Err(malformed("missing image JSON value")),
        }
        Ok(())
    }
}
fn preflight<'a>(bytes: &'a [u8], n: usize, blob: bool, l: &'a Limits) -> Result<&'a str, ProviderError> {
    if bytes.len() > l.response_bytes {
        return Err(malformed("image response exceeds its byte limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| malformed("image response is not UTF8 JSON"))?;
    let mut lex = Lex { text, at: 0, fields: 0, values: 0, strings: 0, metadata: 0, n, blob, limits: l };
    lex.value(1, false, false)?;
    lex.ws();
    if lex.at != text.len() {
        return Err(malformed("trailing image JSON"));
    }
    Ok(text)
}
fn encoded_limit(n: usize) -> Result<usize, ProviderError> {
    n.checked_add(2).and_then(|v| v.checked_div(3)).and_then(|v| v.checked_mul(4)).ok_or_else(|| malformed("base64 extent overflow"))
}
fn sextet(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}
fn decoded_len(s: &str) -> Result<usize, ProviderError> {
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return Err(malformed("invalid image base64 length"));
    }
    let mut length = 0usize;
    for (i, c) in s.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let a = c.first().copied().and_then(sextet);
        let b = c.get(1).copied().and_then(sextet);
        let (Some(_), Some(b)) = (a, b) else {
            return Err(malformed("invalid image base64 alphabet"));
        };
        let last = i == s.len() / 4 - 1;
        let size = match (c.get(2).copied(), c.get(3).copied()) {
            (Some(b'='), Some(b'=')) if last && b & 15 == 0 => 1,
            (Some(c), Some(b'=')) if last && sextet(c).is_some_and(|c| c & 3 == 0) => 2,
            (Some(c), Some(d)) if sextet(c).is_some() && sextet(d).is_some() => 3,
            _ => return Err(malformed("invalid image base64 padding or unused bits")),
        };
        length = length.checked_add(size).ok_or_else(|| malformed("decoded image extent overflow"))?;
    }
    Ok(length)
}
#[derive(Deserialize)]
struct Datum<'a> {
    #[serde(borrow)]
    b64_json: Cow<'a, str>,
}
#[derive(Deserialize, Default)]
struct Tokens {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    input_tokens_details: Option<InputTokens>,
    output_tokens_details: Option<InputTokens>,
}
#[derive(Deserialize)]
struct InputTokens {
    image_tokens: u64,
    text_tokens: u64,
}
#[derive(Deserialize)]
struct Reply<'a> {
    #[serde(borrow)]
    data: Vec<Datum<'a>>,
    usage: Option<Tokens>,
}
#[derive(Deserialize)]
struct ApiError<'a> {
    #[serde(borrow)]
    error: ErrorBody<'a>,
}
#[derive(Deserialize)]
struct ErrorBody<'a> {
    #[serde(borrow)]
    code: Option<Cow<'a, str>>,
    #[serde(borrow, rename = "type")]
    kind: Option<Cow<'a, str>>,
}
fn api_error(reply: &HttpResponse, l: &Limits) -> ProviderError {
    let parsed = preflight(&reply.body, 10, false, l).ok().and_then(|s| serde_json::from_str::<ApiError<'_>>(s).ok());
    let code = parsed.as_ref().and_then(|v| v.error.code.as_deref());
    let ty = parsed.as_ref().and_then(|v| v.error.kind.as_deref());
    let (kind, message) = if code == Some("moderation_blocked") {
        (ErrorKind::BlockedContent, "image request did not meet the provider's safety requirements")
    } else if ty == Some("image_generation_user_error") {
        (ErrorKind::UnsupportedInput, "revise the image prompt or reference images before trying again")
    } else if matches!(
        code,
        Some(
            "insufficient_quota"
                | "credit_balance_exhausted"
                | "organization_spend_limit_exceeded"
                | "project_spend_limit_exceeded"
                | "organization_usage_limit_exceeded"
        )
    ) || ty == Some("insufficient_quota")
    {
        (ErrorKind::Budget, "OpenAI credits or spending limits prevent this request")
    } else {
        match reply.status {
            300..=399 => (ErrorKind::Malformed, "image redirect refused; the original request may have been processed"),
            400 | 413 | 415 | 422 => (ErrorKind::UnsupportedInput, "OpenAI rejected the image request parameters or inputs"),
            401 | 403 => (ErrorKind::Auth, "check OpenAI image model access and the API key"),
            402 => (ErrorKind::Budget, "OpenAI image billing is unavailable"),
            404 | 410 => (ErrorKind::NotFound, "the requested OpenAI image model or endpoint is unavailable"),
            408 | 504 => (ErrorKind::Timeout, "OpenAI image request timed out; it may still have been billed"),
            429 => (ErrorKind::RateLimited, "OpenAI image requests are rate limited"),
            503 => (ErrorKind::Busy, "OpenAI image service is temporarily overloaded"),
            _ => (ErrorKind::Server, "OpenAI image service could not complete the request"),
        }
    };
    let mut e = ProviderError::new(kind, message);
    e.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    e
}
fn usage(tokens: Option<Tokens>) -> Result<Usage, ProviderError> {
    let t = tokens.unwrap_or_default();
    for (details, total) in [(&t.input_tokens_details, t.input_tokens), (&t.output_tokens_details, t.output_tokens)] {
        if let Some(d) = details {
            let sum = d.image_tokens.checked_add(d.text_tokens).ok_or_else(|| malformed("image token count overflow"))?;
            if total.is_some_and(|n| n != sum) {
                return Err(malformed("inconsistent image token counts"));
            }
        }
    }
    if let (Some(input), Some(output), Some(total)) = (t.input_tokens, t.output_tokens, t.total_tokens)
        && input.checked_add(output) != Some(total)
    {
        return Err(malformed("inconsistent image token totals"));
    }
    Ok(Usage { input_tokens: t.input_tokens, output_tokens: t.output_tokens, cost: None })
}
pub(super) fn parse(reply: HttpResponse, n: usize, format: &str, background: &str, l: &Limits) -> Result<Completed, ProviderError> {
    if !reply.is_success() {
        return Err(api_error(&reply, l));
    }
    let text = preflight(&reply.body, n, true, l)?;
    observe(3);
    let parsed: Reply<'_> = serde_json::from_str(text).map_err(|_| malformed("invalid image response fields"))?;
    if parsed.data.is_empty() || parsed.data.len() > n {
        return Err(malformed("invalid image response count"));
    }
    let usage = usage(parsed.usage)?;
    let mut sizes = Vec::new();
    reserve(&mut sizes, parsed.data.len())?;
    let mut total = 0usize;
    for d in &parsed.data {
        let len = decoded_len(&d.b64_json)?;
        if len > l.output_bytes {
            return Err(malformed("decoded image exceeds its byte limit"));
        }
        total = total.checked_add(len).ok_or_else(|| malformed("decoded image extent overflow"))?;
        if total > l.total_output_bytes {
            return Err(malformed("images exceed their aggregate output limit"));
        }
        sizes.push(len);
    }
    let mut outputs = Vec::new();
    reserve(&mut outputs, sizes.len())?;
    let mut total_pixels = 0u64;
    for (d, len) in parsed.data.iter().zip(sizes) {
        let mut bytes = Vec::new();
        reserve(&mut bytes, len)?;
        observe(1);
        bytes.resize(len, 0);
        let actual = base64::engine::general_purpose::STANDARD
            .decode_slice(d.b64_json.as_bytes(), &mut bytes)
            .map_err(|_| malformed("image base64 could not be decoded"))?;
        if actual != len {
            return Err(malformed("image base64 extent changed"));
        }
        let meta = nodecraft_media::probe_image(&bytes, &l.image(false)).map_err(|_| malformed("image output could not be read within codec limits"))?;
        let expected = match format {
            "jpeg" => "image/jpeg",
            "webp" => "image/webp",
            _ => "image/png",
        };
        if meta.mime != expected || (background == "transparent" && !meta.has_alpha) {
            return Err(malformed("image output format or alpha does not match the request"));
        }
        total_pixels = total_pixels
            .checked_add(pixels(&meta, l).map_err(|_| malformed("image output dimensions exceed limits"))?)
            .ok_or_else(|| malformed("image pixel extent overflow"))?;
        if total_pixels > l.total_pixels {
            return Err(malformed("image outputs exceed their aggregate pixel limit"));
        }
        let _ = nodecraft_media::decode_image(&bytes, &l.image(false)).map_err(|_| malformed("image output is corrupt or exceeds codec limits"))?;
        // Vec -> Arc can copy this bounded payload; it is included in the resource worksheet.
        outputs.push(Output::Bytes { mime: meta.mime.to_owned(), bytes: Arc::from(bytes) });
    }
    observe(2);
    Ok(Completed { outputs, usage })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lexical_depth_limit_counts_the_root_before_recursive_descent() {
        let limits = Limits::default();
        let exact = format!("{}0{}", "[".repeat(15), "]".repeat(15));
        assert!(preflight(exact.as_bytes(), 10, true, &limits).is_ok());
        let too_deep = format!("{}0{}", "[".repeat(16), "]".repeat(16));
        assert!(preflight(too_deep.as_bytes(), 10, true, &limits).is_err());
    }
}
