//! Pika-only local MP4 timing/geometry bounds, beyond the shared structural envelope.
use super::*;
pub(in crate::fal) fn probe(b: &[u8], l: Limits) -> Result<(u32, u32), ProviderError> {
    #[derive(Default)]
    struct Meta {
        boxes: usize,
        movie: bool,
        width: u32,
        height: u32,
    }
    fn number(b: &[u8], at: usize) -> Result<u32, ProviderError> {
        b.get(at..at.checked_add(4).ok_or_else(bad)?).and_then(|s| s.try_into().ok()).map(u32::from_be_bytes).ok_or_else(bad)
    }
    fn walk(b: &[u8], depth: usize, m: &mut Meta, l: Limits) -> Result<(), ProviderError> {
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
                    walk(payload, depth + 1, m, l)?;
                }
                b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"dinf" => walk(payload, depth + 1, m, l)?,
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
                    // A conservative local 11-second envelope for the longest offered10s request.
                    if scale == 0 || duration == 0 || duration > u64::from(scale).saturating_mul(11) {
                        return Err(bad());
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
    walk(b, 0, &mut m, l)?;
    if !m.movie || m.width == 0 || m.height == 0 {
        return Err(bad());
    }
    Ok((m.width, m.height))
}
