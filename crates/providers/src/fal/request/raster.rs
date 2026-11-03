//! Bounded static PNG/WebP envelope shared by paid image-driven video models.
use super::*;
pub(super) fn check(mime: &str, b: &[u8]) -> Result<(), ProviderError> {
    let bad = || invalid("requires one static, bounded first-frame image");
    let (mut at, little) = match mime {
        "image/png" if b.get(..8) == Some(b"\x89PNG\r\n\x1a\n") => (8usize, false),
        "image/webp" if b.get(..4) == Some(b"RIFF") && b.get(8..12) == Some(b"WEBP") => (12, true),
        "image/jpeg" => return Ok(()),
        _ => return Err(bad()),
    };
    let mut chunks = 0usize;
    while at < b.len() {
        chunks = chunks.saturating_add(1);
        if chunks > 4096 {
            return Err(bad());
        }
        let (kind, n) = if little {
            let kind = b.get(at..at.checked_add(4).ok_or_else(bad)?).ok_or_else(bad)?;
            let n = b
                .get(at.checked_add(4).ok_or_else(bad)?..at.checked_add(8).ok_or_else(bad)?)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
                .ok_or_else(bad)?;
            (kind, n)
        } else {
            let n = b.get(at..at.checked_add(4).ok_or_else(bad)?).and_then(|s| s.try_into().ok()).map(u32::from_be_bytes).ok_or_else(bad)?;
            let kind = b.get(at.checked_add(4).ok_or_else(bad)?..at.checked_add(8).ok_or_else(bad)?).ok_or_else(bad)?;
            (kind, n)
        };
        if matches!(kind, b"acTL" | b"fcTL" | b"fdAT" | b"ANIM" | b"ANMF") {
            return Err(bad());
        }
        if kind == b"VP8X" && b.get(at.saturating_add(8)).is_none_or(|flag| flag & 2 != 0) {
            return Err(bad());
        }
        let n = usize::try_from(n).map_err(|_| bad())?;
        at = at
            .checked_add(if little { 8 } else { 12 })
            .and_then(|v| v.checked_add(n))
            .and_then(|v| v.checked_add(if little { n % 2 } else { 0 }))
            .filter(|v| *v <= b.len())
            .ok_or_else(bad)?;
    }
    Ok(())
}
