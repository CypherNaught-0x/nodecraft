//! Bounded MP4 timing/geometry walk shared by explicit video client policies.
use super::*;
pub(in crate::fal) struct Info {
    pub width: u32,
    pub height: u32,
    pub min_ms: u64,
    pub max_ms: u64,
    pub media_clocks: usize,
}
pub(in crate::fal) fn probe(b: &[u8], l: Limits, max_seconds: u64) -> Result<Info, ProviderError> {
    #[derive(Default)]
    struct Meta {
        boxes: usize,
        movie: bool,
        width: u32,
        height: u32,
        min_ms: u64,
        max_ms: u64,
        media_clocks: usize,
    }
    fn number(b: &[u8], at: usize) -> Result<u32, ProviderError> {
        b.get(at..at.checked_add(4).ok_or_else(bad)?).and_then(|s| s.try_into().ok()).map(u32::from_be_bytes).ok_or_else(bad)
    }
    fn walk(b: &[u8], depth: usize, m: &mut Meta, l: Limits, max_seconds: u64) -> Result<(), ProviderError> {
        if depth > 16 {
            return Err(bad());
        }
        let mut at = 0usize;
        while at < b.len() {
            m.boxes = m.boxes.saturating_add(1);
            if m.boxes > 4096 {
                return Err(bad());
            }
            let n = number(b, at)?;
            let kind = b.get(at.checked_add(4).ok_or_else(bad)?..at.checked_add(8).ok_or_else(bad)?).ok_or_else(bad)?;
            let (header, n) = if n == 1 {
                let size = b
                    .get(at.checked_add(8).ok_or_else(bad)?..at.checked_add(16).ok_or_else(bad)?)
                    .and_then(|s| s.try_into().ok())
                    .map(u64::from_be_bytes)
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(bad)?;
                (16, size)
            } else if n == 0 {
                (8, b.len() - at)
            } else {
                (8, usize::try_from(n).map_err(|_| bad())?)
            };
            if n < header {
                return Err(bad());
            }
            let end = at.checked_add(n).filter(|n| *n <= b.len()).ok_or_else(bad)?;
            let payload = b.get(at.checked_add(header).ok_or_else(bad)?..end).ok_or_else(bad)?;
            match kind {
                b"moov" => {
                    if depth != 0 || n > l.metadata_bytes {
                        return Err(bad());
                    }
                    walk(payload, depth + 1, m, l, max_seconds)?;
                }
                b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" => walk(payload, depth + 1, m, l, max_seconds)?,
                b"mvhd" | b"mdhd" => {
                    let version = payload.first().copied().ok_or_else(bad)?;
                    let (scale, duration) = match version {
                        0 => (number(payload, 12)?, u64::from(number(payload, 16)?)),
                        1 => {
                            let duration = payload.get(24..32).and_then(|s| s.try_into().ok()).map(u64::from_be_bytes).ok_or_else(bad)?;
                            (number(payload, 20)?, duration)
                        }
                        _ => return Err(bad()),
                    };
                    // Client bounds apply to every declared movie/media clock.
                    if scale == 0 || duration == 0 || duration > u64::from(scale).saturating_mul(max_seconds) {
                        return Err(bad());
                    }
                    let ms = u64::try_from((u128::from(duration) * 1000).div_ceil(u128::from(scale))).map_err(|_| bad())?;
                    m.min_ms = if m.min_ms == 0 { ms } else { m.min_ms.min(ms) };
                    m.max_ms = m.max_ms.max(ms);
                    if kind == b"mdhd" {
                        m.media_clocks = m.media_clocks.saturating_add(1);
                    }
                    if kind == b"mvhd" {
                        if depth != 1 || m.movie {
                            return Err(bad());
                        }
                        m.movie = true;
                    }
                }
                b"tkhd" => {
                    let offset = match payload.first() {
                        Some(0) => 76,
                        Some(1) => 88,
                        _ => return Err(bad()),
                    };
                    let width = number(payload, offset)?;
                    let height = number(payload, offset + 4)?;
                    if width != 0 || height != 0 {
                        // Display geometry is16.16fixedpoint; ceiling preserves a conservative bound.
                        let width = u32::try_from((u64::from(width) + 65535) >> 16).map_err(|_| bad())?;
                        let height = u32::try_from((u64::from(height) + 65535) >> 16).map_err(|_| bad())?;
                        if depth != 2
                            || width == 0
                            || height == 0
                            || width > l.output_width
                            || height > l.output_height
                            || u64::from(width).saturating_mul(u64::from(height)) > l.total_pixels
                        {
                            return Err(bad());
                        }
                        if m.width != 0 {
                            return Err(bad());
                        }
                        m.width = width;
                        m.height = height;
                    }
                }
                _ => {}
            }
            at = end;
        }
        Ok(())
    }
    if b.len() > l.output_bytes || b.len() > l.total_output_bytes {
        return Err(bad());
    }
    let mut m = Meta::default();
    walk(b, 0, &mut m, l, max_seconds)?;
    if !m.movie || m.width == 0 || m.height == 0 {
        return Err(bad());
    }
    Ok(Info { width: m.width, height: m.height, min_ms: m.min_ms, max_ms: m.max_ms, media_clocks: m.media_clocks })
}
