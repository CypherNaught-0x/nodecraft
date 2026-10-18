use super::*;
use serde::Deserialize;
use serde_json::value::RawValue;
use std::borrow::Cow;

const FRAMING_BYTES: usize = 64 * 1024;
// Includes whole JSON output, leaving null available for nonterminal text predictions.
pub(super) fn regions(record: Record, l: Limits) -> Result<(usize, usize), ProviderError> {
    let output = match record.model {
        Model::Text => l.text_bytes.checked_add(2).map(|n| n.max(4)),
        Model::Image => usize::from(record.requested).checked_mul(4099).and_then(|n| n.checked_add(1)),
    }
    .ok_or_else(|| invalid("Replicate output echo extent overflow"))?;
    let input = l
        .response_bytes
        .checked_sub(l.metadata_bytes)
        .and_then(|n| n.checked_sub(FRAMING_BYTES))
        .and_then(|n| n.checked_sub(output))
        .ok_or_else(|| invalid("Replicate response budget cannot contain its admitted regions"))?;
    Ok((input, output))
}
#[derive(Clone, Copy, PartialEq)]
enum Region {
    Other,
    Input,
    Output,
}
// Counts encoded spelling (a conservative bound on decoded UTF8) before serde may allocate
// escape scratch. Unknown input/log fields are bounded too. Root-only output has a separate cap.
struct Lex<'a> {
    text: &'a str,
    at: usize,
    fields: usize,
    values: usize,
    metadata: usize,
    input_bytes: usize,
    output_bytes: usize,
    input_cap: usize,
    output_cap: usize,
    seen_input: bool,
    seen_output: bool,
    l: Limits,
    record: Record,
}
impl Lex<'_> {
    fn b(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }
    fn ws(&mut self) {
        while self.b().is_some_and(|b| b.is_ascii_whitespace()) {
            self.at += 1;
        }
    }
    fn need(&mut self, b: u8) -> Result<(), ProviderError> {
        self.ws();
        if self.b() != Some(b) {
            return Err(malformed("invalid Replicate JSON framing"));
        }
        self.at += 1;
        Ok(())
    }
    fn string(&mut self, key: bool, region: Region) -> Result<&str, ProviderError> {
        self.need(b'"')?;
        let start = self.at;
        let cap = if key {
            256
        } else if region == Region::Input {
            self.input_cap
        } else if region == Region::Output {
            if self.record.model == Model::Text { self.l.text_bytes } else { 4096 }
        } else {
            self.l.metadata_bytes
        };
        loop {
            let b = self.b().ok_or_else(|| malformed("truncated Replicate JSON string"))?;
            if b == b'"' {
                let end = self.at;
                self.at += 1;
                let n = end.saturating_sub(start);
                if region == Region::Other {
                    self.metadata = self
                        .metadata
                        .checked_add(n)
                        .filter(|n| *n <= self.l.metadata_bytes)
                        .ok_or_else(|| malformed("Replicate JSON metadata exceeds its limit before unescaping"))?;
                }
                return self.text.get(start..end).ok_or_else(|| malformed("invalid Replicate JSON string boundary"));
            }
            if b < 32 {
                return Err(malformed("unescaped JSON control byte"));
            }
            if b == b'\\' {
                if key {
                    return Err(malformed("escaped Replicate field names are unsupported"));
                }
                self.at += 1;
                if self.b().is_none() {
                    return Err(malformed("truncated JSON escape"));
                }
            }
            self.at += 1;
            if self.at.saturating_sub(start) > cap {
                return Err(malformed("Replicate JSON string exceeds its limit before unescaping"));
            }
        }
    }
    fn value(&mut self, depth: usize, region: Region) -> Result<(), ProviderError> {
        if depth > 16 {
            return Err(malformed("Replicate JSON depth exceeds 16"));
        }
        self.values += 1;
        if self.values > 256 {
            return Err(malformed("Replicate JSON value count exceeds 256"));
        }
        self.ws();
        match self.b() {
            Some(b'"') => {
                self.string(false, region)?;
            }
            Some(b'{') => {
                self.at += 1;
                self.ws();
                if self.b() == Some(b'}') {
                    self.at += 1;
                    return Ok(());
                }
                loop {
                    self.fields += 1;
                    if self.fields > 64 {
                        return Err(malformed("Replicate JSON field count exceeds 64"));
                    }
                    let (is_input, is_output) = {
                        let key = self.string(true, region)?;
                        (depth == 1 && key == "input", depth == 1 && key == "output")
                    };
                    self.need(b':')?;
                    self.ws();
                    let start = self.at;
                    let child = if is_input {
                        if self.seen_input {
                            return Err(malformed("duplicate Replicate input field"));
                        }
                        self.seen_input = true;
                        if !matches!(self.b(), Some(b'{' | b'n')) {
                            return Err(malformed("Replicate echoed input is not an object or null"));
                        }
                        Region::Input
                    } else if is_output {
                        if self.seen_output {
                            return Err(malformed("duplicate Replicate output field"));
                        }
                        self.seen_output = true;
                        Region::Output
                    } else {
                        region
                    };
                    self.value(depth + 1, child)?;
                    let bytes = self.at.checked_sub(start).ok_or_else(|| malformed("Replicate JSON region extent overflow"))?;
                    if is_input {
                        self.input_bytes = self
                            .input_bytes
                            .checked_add(bytes)
                            .filter(|n| *n <= self.input_cap)
                            .ok_or_else(|| malformed("Replicate echoed input exceeds its region"))?;
                    } else if is_output {
                        self.output_bytes = self
                            .output_bytes
                            .checked_add(bytes)
                            .filter(|n| *n <= self.output_cap)
                            .ok_or_else(|| malformed("Replicate output exceeds its region"))?;
                    }
                    self.ws();
                    match self.b() {
                        Some(b'}') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(malformed("invalid Replicate JSON object")),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                self.ws();
                if self.b() == Some(b']') {
                    self.at += 1;
                    return Ok(());
                }
                let mut n = 0;
                loop {
                    n += 1;
                    if n > if region == Region::Output { usize::from(self.record.requested) } else { 64 } {
                        return Err(malformed("Replicate output or array count exceeds its admitted bound"));
                    }
                    self.value(depth + 1, region)?;
                    self.ws();
                    match self.b() {
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(malformed("invalid Replicate JSON array")),
                    }
                }
            }
            Some(_) => {
                let start = self.at;
                while self.b().is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b',' | b']' | b'}')) {
                    self.at += 1;
                    if self.at - start > 64 {
                        return Err(malformed("Replicate JSON scalar exceeds its limit"));
                    }
                }
                if start == self.at {
                    return Err(malformed("missing Replicate JSON value"));
                }
            }
            None => return Err(malformed("missing Replicate JSON value")),
        }
        Ok(())
    }
}
fn preflight(bytes: &[u8], record: Record, l: Limits) -> Result<&str, ProviderError> {
    if bytes.len() > l.response_bytes {
        return Err(malformed("Replicate API response exceeds its byte limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| malformed("Replicate response is not UTF8 JSON"))?;
    let (input_cap, output_cap) = regions(record, l)?;
    let mut lex = Lex {
        text,
        at: 0,
        fields: 0,
        values: 0,
        metadata: 0,
        input_bytes: 0,
        output_bytes: 0,
        input_cap,
        output_cap,
        seen_input: false,
        seen_output: false,
        l,
        record,
    };
    lex.value(1, Region::Other)?;
    lex.ws();
    if lex.at != text.len() {
        return Err(malformed("trailing Replicate JSON"));
    }
    let framing = text
        .len()
        .checked_sub(lex.input_bytes)
        .and_then(|n| n.checked_sub(lex.output_bytes))
        .and_then(|n| n.checked_sub(lex.metadata))
        .ok_or_else(|| malformed("Replicate JSON region accounting overflow"))?;
    if framing > FRAMING_BYTES {
        return Err(malformed("Replicate JSON framing exceeds its limit"));
    }
    Ok(text)
}
#[derive(Deserialize)]
pub(super) struct Prediction<'a> {
    #[serde(borrow)]
    pub(super) id: Cow<'a, str>,
    #[serde(borrow)]
    model: Cow<'a, str>,
    #[serde(borrow)]
    version: Cow<'a, str>,
    #[serde(borrow)]
    pub(super) status: Cow<'a, str>,
    #[serde(default)]
    pub(super) data_removed: bool,
    #[serde(borrow, default)]
    pub(super) output: Option<&'a RawValue>,
}
impl Prediction<'_> {
    pub(super) fn identity(&self, id: Option<&str>, record: Record) -> Result<(), ProviderError> {
        let (m, v) = record.model.parts();
        if self.id.is_empty()
            || self.id.len() > 128
            || !self.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || id.is_some_and(|id| id != self.id)
            || self.model != m
            || self.version != v
        {
            return Err(malformed("Replicate prediction identity or pinned version does not match"));
        }
        if !matches!(self.status.as_ref(), "starting" | "processing" | "succeeded" | "failed" | "canceled" | "aborted") {
            return Err(malformed("unknown Replicate prediction status"));
        }
        Ok(())
    }
}
pub(super) fn status(reply: &crate::HttpResponse, l: Limits) -> Result<(), ProviderError> {
    if reply.body.len() > l.response_bytes.max(l.output_bytes) {
        return Err(malformed("Replicate HTTP response exceeds its limit"));
    }
    if reply.is_success() {
        return Ok(());
    }
    let (kind, message) = match reply.status {
        300..=399 => (ErrorKind::Malformed, "Replicate redirect refused; the original operation may have been processed"),
        400 | 413 | 415 | 422 => (ErrorKind::UnsupportedInput, "Replicate rejected the input or model parameters"),
        401 | 403 => (ErrorKind::Auth, "check Replicate access and the supplied API key"),
        402 => (ErrorKind::Budget, "Replicate credits or billing are unavailable"),
        404 | 410 => (ErrorKind::NotFound, "Replicate prediction or file is unavailable"),
        408 | 504 => (ErrorKind::Timeout, "Replicate request timed out; processing or billing may still have occurred"),
        429 => (ErrorKind::RateLimited, "Replicate request rate limit reached"),
        503 => (ErrorKind::Busy, "Replicate is temporarily overloaded"),
        _ => (ErrorKind::Server, "Replicate could not complete the request"),
    };
    let mut e = ProviderError::new(kind, message);
    e.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|s| s.checked_mul(1000))
        .filter(|n| *n <= 3_600_000);
    Err(e)
}
pub(super) fn parse(reply: &crate::HttpResponse, record: Record, l: Limits) -> Result<Prediction<'_>, ProviderError> {
    if reply.body.len() > l.response_bytes {
        return Err(malformed("Replicate API response exceeds its byte limit"));
    }
    status(reply, l)?;
    let text = preflight(&reply.body, record, l)?;
    observe(5);
    serde_json::from_str(text).map_err(|_| malformed("invalid Replicate prediction fields"))
}
#[derive(Deserialize)]
#[serde(transparent)]
struct Text<'a> {
    #[serde(borrow)]
    value: Cow<'a, str>,
}
pub(super) enum AdmittedOutput<'a> {
    Text(Cow<'a, str>),
    Images(Vec<Cow<'a, str>>),
}
pub(super) fn outputs<'a>(raw: Option<&'a RawValue>, record: Record, l: Limits) -> Result<AdmittedOutput<'a>, ProviderError> {
    let raw = raw.ok_or_else(|| ProviderError::new(ErrorKind::Expired, "Replicate success has no retained output; it may have expired"))?;
    match record.model {
        Model::Text => {
            let text: Text<'_> = serde_json::from_str(raw.get()).map_err(|_| malformed("Replicate text output is not a scalar string"))?;
            if text.value.len() > l.text_bytes {
                return Err(malformed("Replicate text output exceeds limit"));
            }
            Ok(AdmittedOutput::Text(text.value))
        }
        Model::Image => {
            let list: Vec<Text<'_>> = serde_json::from_str(raw.get()).map_err(|_| malformed("Replicate image output is not a URL array"))?;
            if list.is_empty() || list.len() > usize::from(record.requested) {
                return Err(malformed("Replicate returned an invalid image count"));
            }
            let mut urls = Vec::new();
            reserve(&mut urls, list.len())?;
            for t in list {
                delivery_url(&t.value)?;
                urls.push(t.value);
            }
            Ok(AdmittedOutput::Images(urls))
        }
    }
}
pub(super) fn delivery_url(value: &str) -> Result<(), ProviderError> {
    if value.len() > 4096 {
        return Err(malformed("Replicate output URL exceeds limit"));
    }
    let u = url::Url::parse(value).map_err(|_| malformed("Replicate output URL is invalid"))?;
    let host = u.host_str().unwrap_or_default();
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.port().is_some_and(|p| p != 443)
        || !(host == "replicate.delivery" || host.ends_with(".replicate.delivery"))
    {
        return Err(malformed("Replicate output origin is not an authorized delivery host"));
    }
    Ok(())
}
