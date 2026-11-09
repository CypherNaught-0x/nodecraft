//! Original single-layer, STORE-only ZIP32 profile. No extraction or path resolution.
use crate::MediaError;

pub(super) const OVERHEAD: usize = 142;
const NAME: &[u8] = b"model.usda";
fn invalid() -> MediaError {
    MediaError::Decode("USDZ requires one unencrypted, aligned STORE USDA layer with consistent ZIP32 records".into())
}
fn part(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], MediaError> {
    bytes.get(at..at.checked_add(len).ok_or_else(invalid)?).ok_or_else(invalid)
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16, MediaError> {
    Ok(u16::from_le_bytes(part(bytes, at, 2)?.try_into().map_err(|_| invalid())?))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, MediaError> {
    Ok(u32::from_le_bytes(part(bytes, at, 4)?.try_into().map_err(|_| invalid())?))
}
fn size(bytes: &[u8], at: usize) -> Result<usize, MediaError> {
    usize::try_from(u32_at(bytes, at)?).map_err(|_| invalid())
}
fn name(bytes: &[u8]) -> bool {
    (6..=64).contains(&bytes.len())
        && bytes.ends_with(b".usda")
        && bytes.first().is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && bytes.get(..bytes.len().saturating_sub(5)).is_some_and(|s| s.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')))
}
fn extra(bytes: &[u8]) -> Result<(), MediaError> {
    if bytes.is_empty() {
        return Ok(());
    }
    if bytes.len() > 128 || u16_at(bytes, 0)? != 0x1986 || usize::from(u16_at(bytes, 2)?).checked_add(4) != Some(bytes.len()) {
        return Err(invalid());
    }
    if part(bytes, 4, bytes.len().saturating_sub(4))?.iter().any(|b| *b != 0) {
        return Err(invalid());
    }
    Ok(())
}
/// Small signature check, without CRC or full-file parsing on the sniffing path.
pub(super) fn recognizes(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"PK\x03\x04") {
        return false;
    }
    let Ok(n) = u16_at(bytes, 26) else { return false };
    let Ok(filename) = part(bytes, 30, usize::from(n)) else { return false };
    // Unsupported native USD layers still get a named USDZ profile error on import.
    name(filename)
        || (filename.len() <= 64
            && (filename.ends_with(b".usdc") || filename.ends_with(b".usd"))
            && filename.iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')))
}
pub(super) fn layer(bytes: &[u8]) -> Result<&[u8], MediaError> {
    let end = bytes.len().checked_sub(22).ok_or_else(invalid)?;
    let e = part(bytes, end, 22)?;
    if part(e, 0, 4)? != b"PK\x05\x06" || u16_at(e, 4)? != 0 || u16_at(e, 6)? != 0 || u16_at(e, 8)? != 1 || u16_at(e, 10)? != 1 || u16_at(e, 20)? != 0 {
        return Err(invalid());
    }
    let cd_at = size(e, 16)?;
    let cd_len = size(e, 12)?;
    if cd_at.checked_add(cd_len) != Some(end) {
        return Err(invalid());
    }
    let c = part(bytes, cd_at, cd_len)?;
    let l = part(bytes, 0, 30)?;
    if part(l, 0, 4)? != b"PK\x03\x04" || part(c, 0, 4)? != b"PK\x01\x02" {
        return Err(invalid());
    }
    let version = u16_at(l, 4)?;
    if !matches!(version, 10 | 20) || version != u16_at(c, 6)? || u16_at(l, 6)? != 0 || u16_at(c, 8)? != 0 || u16_at(l, 8)? != 0 || u16_at(c, 10)? != 0 {
        return Err(invalid());
    }
    let made_by = u16_at(c, 4)? >> 8;
    let attrs = u32_at(c, 38)?;
    let file_type = (attrs >> 16) & 0o170000;
    if !matches!(made_by, 0 | 3) || !matches!(file_type, 0 | 0o100000) || attrs & 0x10 != 0 || u16_at(c, 34)? != 0 || u32_at(c, 42)? != 0 || u16_at(c, 32)? != 0
    {
        return Err(invalid());
    }
    for (left, right, len) in [(10, 12, 4), (14, 16, 12), (26, 28, 2)] {
        if part(l, left, len)? != part(c, right, len)? {
            return Err(invalid());
        }
    }
    let n = usize::from(u16_at(l, 26)?);
    let x = usize::from(u16_at(l, 28)?);
    let cx = usize::from(u16_at(c, 30)?);
    let local_name = part(bytes, 30, n)?;
    if part(c, 46, n)? != local_name || 46usize.checked_add(n).and_then(|v| v.checked_add(cx)) != Some(c.len()) {
        return Err(invalid());
    }
    if local_name.ends_with(b".usdc") || local_name.ends_with(b".usd") {
        return Err(MediaError::Decode("USDZ binary USDC and generic USD layers are unsupported; export one static USDA triangle mesh".into()));
    }
    if !name(local_name) {
        return Err(invalid());
    }
    extra(part(bytes, 30usize.checked_add(n).ok_or_else(invalid)?, x)?)?;
    extra(part(c, 46usize.checked_add(n).ok_or_else(invalid)?, cx)?)?;
    let at = 30usize.checked_add(n).and_then(|v| v.checked_add(x)).ok_or_else(invalid)?;
    let len = size(l, 18)?;
    if !at.is_multiple_of(64) || len != size(l, 22)? || at.checked_add(len) != Some(cd_at) {
        return Err(invalid());
    }
    let body = part(bytes, at, len)?;
    if crc32fast::hash(body) != u32_at(l, 14)? {
        return Err(MediaError::Decode("USDZ layer CRC does not match its bytes".into()));
    }
    Ok(body)
}
fn short(out: &mut Vec<u8>, n: u16) {
    out.extend_from_slice(&n.to_le_bytes());
}
fn long(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}
/// `write` is our bounded formatter: it must append exactly the preflighted body length.
pub(super) fn encode(len: usize, cap: usize, write: impl FnOnce(&mut Vec<u8>) -> Result<(), MediaError>) -> Result<Vec<u8>, MediaError> {
    let total = len
        .checked_add(OVERHEAD)
        .filter(|n| *n <= cap)
        .ok_or_else(|| MediaError::LimitExceeded("USDZ complete package exceeds its output byte limit".into()))?;
    let length = u32::try_from(len).map_err(|_| invalid())?;
    let cd_at = length.checked_add(64).ok_or_else(invalid)?;
    let mut out = Vec::new();
    out.try_reserve_exact(total).map_err(|_| MediaError::LimitExceeded("USDZ output allocation refused".into()))?;
    out.extend_from_slice(b"PK\x03\x04");
    for value in [10, 0, 0, 0, 33] {
        short(&mut out, value);
    }
    long(&mut out, 0);
    long(&mut out, length);
    long(&mut out, length);
    short(&mut out, 10);
    short(&mut out, 24);
    out.extend_from_slice(NAME);
    short(&mut out, 0x1986);
    short(&mut out, 20);
    out.extend_from_slice(&[0; 20]);
    write(&mut out)?;
    if out.len() != len.checked_add(64).ok_or_else(invalid)? {
        return Err(invalid());
    }
    let crc = crc32fast::hash(out.get(64..).ok_or_else(invalid)?);
    out.get_mut(14..18).ok_or_else(invalid)?.copy_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(b"PK\x01\x02");
    for value in [0, 10, 0, 0, 0, 33] {
        short(&mut out, value);
    }
    long(&mut out, crc);
    long(&mut out, length);
    long(&mut out, length);
    for value in [10, 0, 0, 0, 0] {
        short(&mut out, value);
    }
    long(&mut out, 0);
    long(&mut out, 0);
    out.extend_from_slice(NAME);
    out.extend_from_slice(b"PK\x05\x06");
    for value in [0, 0, 1, 1] {
        short(&mut out, value);
    }
    long(&mut out, 56);
    long(&mut out, cd_at);
    short(&mut out, 0);
    if out.len() != total {
        return Err(invalid());
    }
    Ok(out)
}
