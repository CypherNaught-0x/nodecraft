use super::*;
use crate::{HttpResponse, Output, Usage};
use base64::Engine;
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;
// This borrowed lexical pass precedes serde's potentially allocating escape decoder. It bounds
// unknown fields too; Cow by itself would be too late. Nesting is capped BEFORE recursion.
struct Lex<'a> {
    text: &'a str,
    at: usize,
    // Aggregate field count across the complete JSON document, not per record.
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
            return Err(malformed("invalid Seedream response JSON"));
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
            let b = self.byte().ok_or_else(|| malformed("truncated Seedream JSON string"))?;
            if b == b'"' {
                let end = self.at;
                self.at += 1;
                self.strings = self.strings.checked_add(decoded).ok_or_else(|| malformed("Seedream string extent overflow"))?;
                if self.strings > 64 * MIB + self.limits.metadata_bytes {
                    return Err(malformed("Seedream JSON strings exceed their aggregate limit"));
                }
                if !(blob && self.blob) {
                    self.metadata = self.metadata.checked_add(decoded).ok_or_else(|| malformed("Seedream metadata overflow"))?;
                    if self.metadata > self.limits.metadata_bytes {
                        return Err(malformed("Seedream metadata exceeds its limit"));
                    }
                }
                return self.text.get(start..end).ok_or_else(|| malformed("invalid JSON string boundary"));
            }
            let add = match b {
                0..=31 => return Err(malformed("control character in Seedream JSON")),
                b'\\' => {
                    if blob {
                        return Err(malformed("escaped Seedream base64 is unsupported"));
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
            decoded = decoded.checked_add(add).ok_or_else(|| malformed("Seedream string extent overflow"))?;
            if decoded > cap {
                return Err(malformed("Seedream JSON string exceeds its limit before unescaping"));
            }
        }
    }
    fn value(&mut self, depth: usize, blob: bool, data: bool) -> Result<(), ProviderError> {
        if depth > 16 {
            return Err(malformed("Seedream JSON is too deeply nested"));
        }
        self.values += 1;
        if self.values > 256 {
            return Err(malformed("too many Seedream JSON values"));
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
                        return Err(malformed("too many Seedream JSON fields"));
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
                        _ => return Err(malformed("invalid Seedream JSON object")),
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
                        return Err(malformed("too many Seedream response entries"));
                    }
                    self.value(depth + 1, false, false)?;
                    self.ws();
                    match self.byte() {
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        Some(b',') => self.at += 1,
                        _ => return Err(malformed("invalid Seedream JSON array")),
                    }
                }
            }
            Some(_) => {
                let start = self.at;
                while self.byte().is_some_and(|b| !matches!(b, b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}')) {
                    self.at += 1;
                    if self.at - start > 64 {
                        return Err(malformed("Seedream JSON scalar exceeds its limit"));
                    }
                }
                if start == self.at {
                    return Err(malformed("missing Seedream JSON value"));
                }
            }
            None => return Err(malformed("missing Seedream JSON value")),
        }
        Ok(())
    }
}
fn preflight<'a>(bytes: &'a [u8], n: usize, blob: bool, l: &'a Limits) -> Result<&'a str, ProviderError> {
    if bytes.len() > l.response_bytes {
        return Err(malformed("Seedream response exceeds its byte limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| malformed("Seedream response is not UTF8 JSON"))?;
    let mut lex = Lex { text, at: 0, fields: 0, values: 0, strings: 0, metadata: 0, n, blob, limits: l };
    lex.value(1, false, false)?;
    lex.ws();
    if lex.at != text.len() {
        return Err(malformed("trailing Seedream JSON"));
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
        return Err(malformed("invalid Seedream base64 length"));
    }
    let mut length = 0usize;
    for (i, c) in s.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let a = c.first().copied().and_then(sextet);
        let b = c.get(1).copied().and_then(sextet);
        let (Some(_), Some(b)) = (a, b) else {
            return Err(malformed("invalid Seedream base64 alphabet"));
        };
        let last = i == s.len() / 4 - 1;
        let size = match (c.get(2).copied(), c.get(3).copied()) {
            (Some(b'='), Some(b'=')) if last && b & 15 == 0 => 1,
            (Some(c), Some(b'=')) if last && sextet(c).is_some_and(|c| c & 3 == 0) => 2,
            (Some(c), Some(d)) if sextet(c).is_some() && sextet(d).is_some() => 3,
            _ => return Err(malformed("invalid Seedream base64 padding or unused bits")),
        };
        length = length.checked_add(size).ok_or_else(|| malformed("decoded Seedream extent overflow"))?;
    }
    Ok(length)
}

// deserialize_map deliberately has no sequence visitor: required/optional records cannot
// accept serde's positional struct representation. Unknown values stay borrowed RawValue.
struct Key<'a>(std::borrow::Cow<'a, str>);
impl<'de> Deserialize<'de> for Key<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct K;
        impl<'de> Visitor<'de> for K {
            type Value = Key<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded field name")
            }
            fn visit_borrowed_str<E: serde::de::Error>(self, s: &'de str) -> Result<Self::Value, E> {
                Ok(Key(std::borrow::Cow::Borrowed(s)))
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                if s.len() > 256 {
                    return Err(E::custom("field name limit"));
                }
                Ok(Key(std::borrow::Cow::Owned(s.to_owned())))
            }
        }
        d.deserialize_str(K)
    }
}
struct Object<'a>(Vec<(Key<'a>, &'a RawValue)>);
impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                out.try_reserve_exact(64).map_err(serde::de::Error::custom)?;
                while let Some(k) = a.next_key::<Key<'de>>()? {
                    if out.len() >= 64 {
                        return Err(serde::de::Error::custom("object field limit"));
                    }
                    out.push((k, a.next_value::<&'de RawValue>()?));
                }
                Ok(Object(out))
            }
        }
        d.deserialize_map(V)
    }
}
impl<'a> Object<'a> {
    fn required(&self, key: &str) -> Result<&'a RawValue, ProviderError> {
        let mut found = self.0.iter().filter(|(k, _)| k.0 == key);
        let v = found.next().map(|(_, v)| *v).ok_or_else(|| malformed("missing Seedream required field"))?;
        if found.next().is_some() {
            return Err(malformed("duplicate Seedream required field"));
        }
        Ok(v)
    }
    fn optional(&self, key: &str) -> Result<Option<&'a RawValue>, ProviderError> {
        if self.0.iter().any(|(k, _)| k.0 == key) { self.required(key).map(Some) } else { Ok(None) }
    }
}
fn object(raw: &str) -> Result<Object<'_>, ProviderError> {
    serde_json::from_str(raw).map_err(|_| malformed("Seedream record must be an object"))
}
fn text(raw: &RawValue) -> Result<&str, ProviderError> {
    serde_json::from_str(raw.get()).map_err(|_| malformed("Seedream field must be an unescaped string"))
}
fn usage(raw: Option<&RawValue>) -> Option<Usage> {
    let o = object(raw?.get()).ok()?;
    let num = |key| -> Option<Option<u64>> { o.optional(key).ok()?.map(|r| serde_json::from_str::<u64>(r.get()).ok()).map_or(Some(None), |n| n.map(Some)) };
    let generated = num("generated_images")?;
    if generated.is_some_and(|n| n != 1) {
        return None;
    }
    let output = num("output_tokens")?;
    let total = num("total_tokens")?;
    if output.zip(total).is_some_and(|(a, b)| a != b) {
        return None;
    }
    Some(Usage { output_tokens: output, ..Default::default() })
}
fn status(reply: &HttpResponse) -> Result<(), ProviderError> {
    if reply.is_success() {
        return Ok(());
    }
    let kind = match reply.status {
        300..=399 => ErrorKind::Malformed,
        400 | 413 | 415 | 422 => ErrorKind::UnsupportedInput,
        401 | 403 => ErrorKind::Auth,
        402 => ErrorKind::Budget,
        408 | 504 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        503 => ErrorKind::Busy,
        _ => ErrorKind::Server,
    };
    let mut e = ProviderError::new(kind, "Seedream request failed; review account or input settings; billing may have occurred");
    e.retry_after_ms =
        reply.header("retry-after").filter(|s| s.len() <= 16).and_then(|s| s.parse::<u64>().ok()).and_then(|s| s.checked_mul(1000)).filter(|n| *n <= 3600000);
    Err(e)
}
pub(super) fn parse(reply: HttpResponse, l: Limits) -> Result<Completed, ProviderError> {
    if reply.body.len() > l.response_bytes {
        return Err(malformed("Seedream response exceeds byte limit"));
    }
    status(&reply)?;
    let json = preflight(&reply.body, 1, true, &l)?;
    let root = object(json)?;
    if root.optional("error")?.is_some() {
        return Err(ProviderError::new(ErrorKind::JobFailed, "Seedream returned an unsuccessful generation"));
    }
    if text(root.required("model")?)? != MODEL {
        return Err(malformed("Seedream response model does not match"));
    }
    let list: Vec<&RawValue> = serde_json::from_str(root.required("data")?.get()).map_err(|_| malformed("Seedream data must be an array"))?;
    if list.len() != 1 {
        return Err(malformed("Seedream must return exactly one image"));
    }
    let item = object(list.first().ok_or_else(|| malformed("missing Seedream image"))?.get())?;
    if item.optional("error")?.is_some() || item.optional("url")?.is_some() {
        return Err(malformed("Seedream image error or unexpected URL"));
    }
    // `size` is advisory metadata. The bounded JPEG bytes below determine actual geometry;
    // omitted, malformed or stale dimensions must not discard an otherwise valid paid image.
    if let Some(f) = item.optional("output_format")?
        && text(f)? != "jpeg"
    {
        return Err(malformed("Seedream 4.5 output must be JPEG"));
    }
    let blob = text(item.required("b64_json")?)?;
    let n = decoded_len(blob)?;
    if n > l.output_bytes {
        return Err(malformed("Seedream image exceeds decoded byte limit"));
    }
    let usage = root.optional("usage").ok().and_then(usage).unwrap_or_default();
    let mut bytes = Vec::new();
    reserve(&mut bytes, n)?;
    observe(2);
    bytes.resize(n, 0);
    let actual = base64::engine::general_purpose::STANDARD.decode_slice(blob, &mut bytes).map_err(|_| malformed("invalid Seedream base64"))?;
    if actual != n {
        return Err(malformed("Seedream decoded extent mismatch"));
    }
    jpeg_extent(&bytes)?;
    let meta = nodecraft_media::probe_image(&bytes, &l.image(false)).map_err(|_| malformed("Seedream output exceeds codec bounds"))?;
    if meta.mime != "image/jpeg" {
        return Err(malformed("Seedream output MIME disagrees"));
    }
    geometry(meta.width, meta.height, l, false).map_err(|_| malformed("Seedream output geometry exceeds limits"))?;
    // Count attempts, including decoder errors, so tests prove geometry refuses before entry.
    observe(3);
    let _ = nodecraft_media::decode_image(&bytes, &l.image(false)).map_err(|_| malformed("Seedream output is corrupt"))?;
    Ok(Completed { outputs: vec![Output::Bytes { mime: "image/jpeg".into(), bytes: Arc::from(bytes) }], usage })
}

// The codec may ignore bytes after EOI. Walk marker extents without copying or interpreting
// pixel data so a valid prefix cannot hide a trailing second payload. The real codec still
// validates every scan; this is only whole-output framing admission.
fn jpeg_extent(bytes: &[u8]) -> Result<(), ProviderError> {
    let bad = || malformed("Seedream JPEG framing is truncated or has trailing data");
    if bytes.get(..2) != Some(&b"\xff\xd8"[..]) {
        return Err(bad());
    }
    let mut at = 2usize;
    loop {
        if bytes.get(at) != Some(&255) {
            return Err(bad());
        }
        while bytes.get(at) == Some(&255) {
            at += 1;
        }
        let marker = *bytes.get(at).ok_or_else(bad)?;
        at += 1;
        match marker {
            0xd9 => return if at == bytes.len() { Ok(()) } else { Err(bad()) },
            0 | 0xd8 | 0xd0..=0xd7 => return Err(bad()),
            1 => continue,
            _ => {}
        }
        let raw = bytes.get(at..at.saturating_add(2)).ok_or_else(bad)?;
        let n = usize::from(u16::from_be_bytes(raw.try_into().map_err(|_| bad())?));
        if n < 2 {
            return Err(bad());
        }
        at = at.checked_add(n).filter(|n| *n <= bytes.len()).ok_or_else(bad)?;
        if marker == 0xda {
            loop {
                while bytes.get(at).is_some_and(|b| *b != 255) {
                    at += 1;
                }
                let start = at;
                while bytes.get(at) == Some(&255) {
                    at += 1;
                }
                let code = *bytes.get(at).ok_or_else(bad)?;
                if code == 0 || code == 1 || (0xd0..=0xd7).contains(&code) {
                    at += 1;
                    continue;
                }
                at = start;
                break;
            }
        }
    }
}
