use super::*;
use crate::{HttpResponse, Output, Usage};
use base64::Engine;
use serde::Deserialize;
use std::borrow::Cow;

/// Derived structs also accept positional sequences. REST records are maps only; this adapter
/// selects deserialize_map before delegating to the borrowed derived field visitor. It owns no
/// JSON tree and never tries to recover a partially consumed outer deserializer.
struct Object<T>(T);
impl<T> std::ops::Deref for Object<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Map<T>(std::marker::PhantomData<fn() -> T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Map<T> {
            type Value = Object<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Object)
            }
        }
        deserializer.deserialize_map(Map(std::marker::PhantomData))
    }
}

#[derive(Deserialize)]
struct Content<'a> {
    #[serde(borrow, rename = "type")]
    kind: Cow<'a, str>,
    #[serde(borrow)]
    data: Option<Cow<'a, str>>,
    #[serde(borrow)]
    mime_type: Option<Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<Cow<'a, str>>,
    #[serde(borrow)]
    uri: Option<Cow<'a, str>>,
}
#[derive(Deserialize)]
struct Step<'a> {
    #[serde(borrow, rename = "type")]
    kind: Cow<'a, str>,
    #[serde(borrow, default)]
    content: Vec<Object<Content<'a>>>,
    #[serde(borrow, default)]
    summary: Vec<Object<Content<'a>>>,
}
#[derive(Deserialize)]
struct Reply<'a> {
    #[serde(borrow)]
    status: Cow<'a, str>,
    #[serde(borrow, default)]
    steps: Vec<Object<Step<'a>>>,
    #[serde(borrow)]
    usage: Option<&'a serde_json::value::RawValue>,
}
pub(super) fn encoded_limit(n: usize) -> Result<usize, ProviderError> {
    n.checked_add(2).and_then(|v| v.checked_div(3)).and_then(|v| v.checked_mul(4)).ok_or_else(|| malformed("Gemini base64 extent overflow"))
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
        return Err(malformed("invalid Gemini base64 length"));
    }
    let mut total = 0usize;
    for (i, c) in s.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let (a, b) = (c.first().copied().and_then(sextet), c.get(1).copied().and_then(sextet));
        let (Some(_), Some(b)) = (a, b) else {
            return Err(malformed("invalid Gemini base64 alphabet"));
        };
        let last = i == s.len() / 4 - 1;
        let n = match (c.get(2).copied(), c.get(3).copied()) {
            (Some(b'='), Some(b'=')) if last && b & 15 == 0 => 1,
            (Some(c), Some(b'=')) if last && sextet(c).is_some_and(|c| c & 3 == 0) => 2,
            (Some(c), Some(d)) if sextet(c).is_some() && sextet(d).is_some() => 3,
            _ => return Err(malformed("invalid Gemini base64 padding or unused bits")),
        };
        total = total.checked_add(n).ok_or_else(|| malformed("Gemini base64 extent overflow"))?;
    }
    Ok(total)
}
#[derive(Deserialize)]
struct ModalityTokens<'a> {
    #[serde(borrow)]
    modality: Cow<'a, str>,
    tokens: u64,
}
#[derive(Deserialize)]
struct Tokens<'a> {
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    total_cached_tokens: Option<u64>,
    total_thought_tokens: Option<u64>,
    total_tool_use_tokens: Option<u64>,
    #[serde(borrow)]
    input_tokens_by_modality: Option<Vec<Object<ModalityTokens<'a>>>>,
    #[serde(borrow)]
    output_tokens_by_modality: Option<Vec<Object<ModalityTokens<'a>>>>,
}
fn usage(raw: Option<&serde_json::value::RawValue>) -> Usage {
    let Some(v) = raw.and_then(|v| serde_json::from_str::<Object<Tokens<'_>>>(v.get()).ok()) else {
        return Usage::default();
    };
    for (list, total) in [(&v.input_tokens_by_modality, v.total_input_tokens), (&v.output_tokens_by_modality, v.total_output_tokens)] {
        if let Some(list) = list {
            if list.len() > 16 {
                return Usage::default();
            }
            let mut sum = 0u64;
            for (i, item) in list.iter().enumerate() {
                if !matches!(item.modality.as_ref(), "text" | "image" | "audio" | "video" | "document")
                    || list.iter().take(i).any(|p| p.modality == item.modality)
                {
                    return Usage::default();
                }
                let Some(n) = sum.checked_add(item.tokens) else {
                    return Usage::default();
                };
                sum = n;
            }
            if total.is_some_and(|n| sum > n) {
                return Usage::default();
            }
        }
    }
    if let (Some(a), Some(b)) = (v.total_input_tokens, v.total_output_tokens) {
        let Some(sum) = a.checked_add(b) else {
            return Usage::default();
        };
        if v.total_tokens.is_some_and(|n| sum > n) {
            return Usage::default();
        }
    }
    for count in [v.total_cached_tokens, v.total_thought_tokens, v.total_tool_use_tokens].into_iter().flatten() {
        if v.total_tokens.is_some_and(|n| count > n) {
            return Usage::default();
        }
    }
    Usage { input_tokens: v.total_input_tokens, output_tokens: v.total_output_tokens, cost: None, billable_units: None }
}
fn error(reply: &HttpResponse) -> ProviderError {
    let kind = match reply.status {
        300..=399 => ErrorKind::Malformed,
        400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Budget,
        404 | 410 => ErrorKind::NotFound,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        503 => ErrorKind::Busy,
        _ => ErrorKind::Server,
    };
    let mut e = ProviderError::new(kind, "Gemini image request failed; sent requests may have been billed");
    e.retry_after_ms = reply
        .header("retry-after")
        .filter(|s| s.len() <= 16)
        .and_then(|s| s.trim().parse::<u64>().ok())
        .and_then(|n| n.checked_mul(1000))
        .filter(|n| *n <= 3600000);
    e
}
pub(super) fn parse(reply: HttpResponse, l: &Limits) -> Result<Completed, ProviderError> {
    if reply.body.len() > l.response_bytes {
        return Err(malformed("Gemini response exceeds its byte limit"));
    }
    if !reply.is_success() {
        return Err(error(&reply));
    }
    let text = wire::admit(&reply.body, l)?;
    let parsed: Object<Reply<'_>> = serde_json::from_str(text).map_err(|_| malformed("invalid required Gemini response fields"))?;
    match parsed.status.as_ref() {
        "completed" => {}
        "cancelled" => return Err(ProviderError::new(ErrorKind::Cancelled, "Gemini cancelled the interaction; billing may be unresolved")),
        "failed" => return Err(ProviderError::new(ErrorKind::JobFailed, "Gemini image interaction failed")),
        "incomplete" => return Err(malformed("Gemini returned incomplete output; no partial result published")),
        _ => return Err(malformed("Gemini returned an unsupported nonterminal status")),
    }
    if parsed.steps.len() > l.steps {
        return Err(malformed("too many Gemini response steps"));
    }
    let mut count = 0usize;
    let mut images = 0usize;
    let mut total_bytes = 0usize;
    let mut texts = 0usize;
    let mut items = Vec::new();
    for step in &parsed.steps {
        add(&mut count, step.content.len(), l.contents)?;
        add(&mut count, step.summary.len(), l.contents)?;
        match step.kind.as_ref() {
            "thought" | "user_input" => {}
            "model_output" => {
                if !step.summary.is_empty() {
                    return Err(malformed("unexpected Gemini output summary"));
                }
                for c in &step.content {
                    match c.kind.as_ref() {
                        "image" => {
                            if c.uri.is_some() || c.text.is_some() || c.mime_type.as_deref() != Some("image/jpeg") {
                                return Err(malformed("Gemini output must be an inline JPEG image"));
                            }
                            let data = c.data.as_deref().ok_or_else(|| malformed("Gemini image data missing"))?;
                            let size = decoded_len(data)?;
                            if size > l.output_bytes {
                                return Err(malformed("Gemini output exceeds decoded byte limit"));
                            }
                            add(&mut total_bytes, size, l.total_output_bytes)?;
                            add(&mut images, 1, l.images)?;
                            reserve(&mut items, 1)?;
                            items.push((c, Some(size)));
                        }
                        "text" => {
                            if c.data.is_some() || c.uri.is_some() {
                                return Err(malformed("invalid Gemini text content"));
                            }
                            let text = c.text.as_deref().ok_or_else(|| malformed("Gemini text missing"))?;
                            add(&mut texts, text.len(), l.text_bytes)?;
                            reserve(&mut items, 1)?;
                            items.push((c, None));
                        }
                        _ => return Err(malformed("unknown Gemini output content type")),
                    }
                }
            }
            _ => return Err(malformed("unknown Gemini response step type")),
        }
    }
    if images == 0 {
        return Err(malformed("Gemini returned no final image; revise the request or check safety restrictions"));
    }
    let mut outputs = Vec::new();
    reserve(&mut outputs, items.len())?;
    let mut total_pixels = 0u64;
    for (c, size) in items {
        if let Some(size) = size {
            let mut bytes = Vec::new();
            reserve(&mut bytes, size)?;
            bytes.resize(size, 0);
            let n = base64::engine::general_purpose::STANDARD
                .decode_slice(c.data.as_deref().unwrap_or_default(), &mut bytes)
                .map_err(|_| malformed("Gemini image base64 decode failed"))?;
            if n != size {
                return Err(malformed("Gemini decoded extent changed"));
            }
            let m = nodecraft_media::probe_image(&bytes, &l.image(false)).map_err(|_| malformed("Gemini output cannot be read within codec limits"))?;
            if m.mime != "image/jpeg" {
                return Err(malformed("Gemini image MIME disagrees with its bytes"));
            }
            total_pixels =
                total_pixels.checked_add(pixels(&m, l)?).filter(|v| *v <= l.total_pixels).ok_or_else(|| malformed("Gemini outputs exceed aggregate pixels"))?;
            let _ = nodecraft_media::decode_image(&bytes, &l.image(false)).map_err(|_| malformed("Gemini output is corrupt or exceeds codec limits"))?;
            outputs.push(Output::Bytes { mime: "image/jpeg".into(), bytes: Arc::from(bytes) });
        } else {
            outputs.push(Output::Text(c.text.as_deref().unwrap_or_default().into()));
        }
    }
    Ok(Completed { outputs, usage: usage(parsed.usage) })
}
