//! A bounded reader for the AWS event-stream framing (`application/vnd.amazon.eventstream`) that
//! Bedrock's `ConverseStream` replies in (#1150).
//!
//! Each message is: a prelude of the total length and the headers' length (4-byte big-endian
//! each), the prelude's CRC32, the headers, the payload, and the CRC32 of everything before it.
//! Each header is a 1-byte name length, the UTF-8 name, a 1-byte value type and the value. Both
//! checksums are the GZIP CRC32 (as AWS documents it for this encoding:
//! <https://docs.aws.amazon.com/lexv2/latest/dg/event-stream-encoding.html>).
//!
//! The reply is hostile input: every length is checked against what arrived and against a cap
//! before anything is sized by it, both checksums are verified before a message is read, and a
//! bad message stops the stream with a named error, never a panic. A stream that broke mid-message
//! is told apart from one that is corrupt, since the first is a dropped connection.

use std::collections::HashSet;

use crate::{ErrorKind, ProviderError};

/// Bytes of a message that aren't headers or payload: the prelude, its CRC and the message CRC.
const OVERHEAD: usize = 16;
/// The prelude and its CRC.
const PRELUDE: usize = 12;
/// Longest message accepted. Converse events are small (a text delta is a few hundred bytes);
/// the largest, a redacted reasoning block or a guardrail trace, is far below this.
pub(crate) const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// Longest headers section accepted (the AWS SDKs' own limit is 128 KiB).
pub(crate) const MAX_HEADER_BYTES: usize = 128 * 1024;

/// A header value, by its wire type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HeaderValue {
    Bool(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Bytes(Vec<u8>),
    String(String),
    /// Milliseconds since the Unix epoch.
    Timestamp(i64),
    Uuid([u8; 16]),
}

/// One whole, checked message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Message {
    pub headers: Vec<(String, HeaderValue)>,
    pub payload: Vec<u8>,
}

impl Message {
    /// A string header's value.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).and_then(|(_, v)| match v {
            HeaderValue::String(s) => Some(s.as_str()),
            _ => None,
        })
    }
}

/// Why a stream can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameError {
    /// The stream ended inside a message.
    Truncated,
    /// A message says it is longer than [`MAX_MESSAGE_BYTES`], or its headers longer than
    /// [`MAX_HEADER_BYTES`].
    TooLarge,
    /// The prelude's lengths contradict each other.
    BadLength,
    /// The prelude's checksum doesn't match.
    PreludeChecksum,
    /// The message's checksum doesn't match.
    MessageChecksum,
    /// A header is cut off, names an unknown type, isn't UTF-8, or repeats.
    BadHeader,
}

impl FrameError {
    fn what(self) -> &'static str {
        match self {
            Self::Truncated => "the reply stream ended inside an event",
            Self::TooLarge => "a reply stream event is larger than NodeCraft accepts",
            Self::BadLength => "a reply stream event has inconsistent lengths",
            Self::PreludeChecksum => "a reply stream event failed its prelude checksum",
            Self::MessageChecksum => "a reply stream event failed its checksum",
            Self::BadHeader => "a reply stream event has a malformed header",
        }
    }
}

impl From<FrameError> for ProviderError {
    fn from(e: FrameError) -> Self {
        // A stream cut short is a dropped connection; anything else is a reply that can't be read.
        let kind = if e == FrameError::Truncated { ErrorKind::Network } else { ErrorKind::Malformed };
        ProviderError::new(kind, e.what())
    }
}

/// Reads messages out of a byte stream that arrives in pieces of any size.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    /// Bytes of a message not yet whole. Never more than one message's worth plus one piece.
    pending: Vec<u8>,
    /// The first error: once a stream is unreadable, it stays so.
    failed: Option<FrameError>,
}

impl Decoder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Takes the next piece of the stream and returns the messages it completes, in order.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Result<Vec<Message>, FrameError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        let mut at = 0usize;
        loop {
            let rest = self.pending.get(at..).unwrap_or_default();
            match next(rest) {
                Ok(Some((message, used))) => {
                    out.push(message);
                    at = at.saturating_add(used);
                }
                Ok(None) => break,
                Err(e) => {
                    self.failed = Some(e);
                    self.pending.clear();
                    return Err(e);
                }
            }
        }
        self.pending.drain(..at.min(self.pending.len()));
        Ok(out)
    }

    /// The stream ended: anything left over is a message cut short.
    pub(crate) fn finish(&self) -> Result<(), FrameError> {
        if let Some(e) = self.failed {
            return Err(e);
        }
        if self.pending.is_empty() { Ok(()) } else { Err(FrameError::Truncated) }
    }
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    let b = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([*b.first()?, *b.get(1)?, *b.get(2)?, *b.get(3)?]))
}

/// The first whole message in `bytes` and the bytes it took, or `None` until it is whole. The
/// prelude is checked as soon as it arrives, so a hostile length is refused before more is read.
fn next(bytes: &[u8]) -> Result<Option<(Message, usize)>, FrameError> {
    if bytes.len() < PRELUDE {
        return Ok(None);
    }
    let (Some(total), Some(headers_len), Some(prelude_crc)) = (be32(bytes, 0), be32(bytes, 4), be32(bytes, 8)) else { return Ok(None) };
    if crc32(bytes.get(..8).unwrap_or_default()) != prelude_crc {
        return Err(FrameError::PreludeChecksum);
    }
    let total = usize::try_from(total).map_err(|_| FrameError::TooLarge)?;
    let headers_len = usize::try_from(headers_len).map_err(|_| FrameError::TooLarge)?;
    if total > MAX_MESSAGE_BYTES || headers_len > MAX_HEADER_BYTES {
        return Err(FrameError::TooLarge);
    }
    if total < OVERHEAD || headers_len > total - OVERHEAD {
        return Err(FrameError::BadLength);
    }
    let Some(message) = bytes.get(..total) else { return Ok(None) };
    let body_end = total - 4;
    let (Some(signed), Some(message_crc)) = (message.get(..body_end), be32(message, body_end)) else { return Err(FrameError::BadLength) };
    if crc32(signed) != message_crc {
        return Err(FrameError::MessageChecksum);
    }
    let headers_end = PRELUDE + headers_len;
    let headers = headers(message.get(PRELUDE..headers_end).ok_or(FrameError::BadLength)?)?;
    let payload = message.get(headers_end..body_end).ok_or(FrameError::BadLength)?.to_vec();
    Ok(Some((Message { headers, payload }, total)))
}

/// Reads `n` bytes at `*at`, moving past them.
fn take<'a>(bytes: &'a [u8], at: &mut usize, n: usize) -> Result<&'a [u8], FrameError> {
    let end = at.checked_add(n).ok_or(FrameError::BadHeader)?;
    let out = bytes.get(*at..end).ok_or(FrameError::BadHeader)?;
    *at = end;
    Ok(out)
}

fn array<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], FrameError> {
    take(bytes, at, N)?.try_into().map_err(|_| FrameError::BadHeader)
}

fn headers(bytes: &[u8]) -> Result<Vec<(String, HeaderValue)>, FrameError> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let [name_len] = array::<1>(bytes, &mut at)?;
        if name_len == 0 {
            return Err(FrameError::BadHeader);
        }
        let name = std::str::from_utf8(take(bytes, &mut at, usize::from(name_len))?).map_err(|_| FrameError::BadHeader)?.to_owned();
        let [kind] = array::<1>(bytes, &mut at)?;
        let value = match kind {
            0 => HeaderValue::Bool(true),
            1 => HeaderValue::Bool(false),
            2 => HeaderValue::Byte(i8::from_be_bytes(array(bytes, &mut at)?)),
            3 => HeaderValue::Short(i16::from_be_bytes(array(bytes, &mut at)?)),
            4 => HeaderValue::Int(i32::from_be_bytes(array(bytes, &mut at)?)),
            5 => HeaderValue::Long(i64::from_be_bytes(array(bytes, &mut at)?)),
            6 | 7 => {
                let len = usize::from(u16::from_be_bytes(array(bytes, &mut at)?));
                let raw = take(bytes, &mut at, len)?;
                if kind == 6 {
                    HeaderValue::Bytes(raw.to_vec())
                } else {
                    HeaderValue::String(std::str::from_utf8(raw).map_err(|_| FrameError::BadHeader)?.to_owned())
                }
            }
            8 => HeaderValue::Timestamp(i64::from_be_bytes(array(bytes, &mut at)?)),
            9 => HeaderValue::Uuid(array(bytes, &mut at)?),
            _ => return Err(FrameError::BadHeader),
        };
        // "Any given header can appear only once."
        if !seen.insert(name.clone()) {
            return Err(FrameError::BadHeader);
        }
        out.push((name, value));
    }
    Ok(out)
}

/// The GZIP (IEEE 802.3, reflected) CRC32 table, built at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
};

/// The GZIP CRC32 of `bytes`.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in bytes {
        let index = ((c ^ u32::from(*b)) & 0xFF) as usize;
        c = CRC_TABLE.get(index).copied().unwrap_or_default() ^ (c >> 8);
    }
    !c
}

/// Builds one message, for tests and fixtures: string headers only, both checksums right.
#[cfg(test)]
pub(crate) fn encode(headers: &[(&str, &str)], payload: &[u8]) -> Vec<u8> {
    let mut h = Vec::new();
    for (name, value) in headers {
        h.push(name.len() as u8);
        h.extend_from_slice(name.as_bytes());
        h.push(7);
        h.extend_from_slice(&(value.len() as u16).to_be_bytes());
        h.extend_from_slice(value.as_bytes());
    }
    let total = (OVERHEAD + h.len() + payload.len()) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&total.to_be_bytes());
    out.extend_from_slice(&(h.len() as u32).to_be_bytes());
    let prelude_crc = crc32(&out);
    out.extend_from_slice(&prelude_crc.to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(payload);
    let crc = crc32(&out);
    out.extend_from_slice(&crc.to_be_bytes());
    out
}

#[cfg(test)]
mod tests;
