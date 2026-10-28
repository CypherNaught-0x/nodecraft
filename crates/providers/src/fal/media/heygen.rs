//! Measured client media bounds, never a vendor duration or billing promise.
use super::*;
pub(in crate::fal) fn audio(b: &[u8]) -> Result<(), ProviderError> {
    wav(b)?;
    let mut at = 12usize;
    while at < b.len() {
        let n = usize::try_from(u32le(b, at.checked_add(4).ok_or_else(bad)?)?).map_err(|_| bad())?;
        let start = at.checked_add(8).ok_or_else(bad)?;
        if b.get(at..start.saturating_sub(4)) == Some(b"fmt ") && u16le(b, start)? != 1 {
            return Err(bad());
        }
        at = start.checked_add(n).and_then(|v| v.checked_add(n % 2)).filter(|v| *v <= b.len()).ok_or_else(bad)?;
    }
    if nodecraft_media::probe_sound(b).duration_ms.is_none_or(|ms| ms == 0 || ms > 10_000) {
        return Err(bad());
    }
    Ok(())
}
pub(in crate::fal) fn input_video(b: &[u8], l: Limits) -> Result<(), ProviderError> {
    mp4(b)?;
    let info = video::probe(
        b,
        Limits { output_bytes: l.input_bytes, total_output_bytes: l.input_bytes, output_width: 1280, output_height: 720, total_pixels: 1280 * 720, ..l },
        10,
    )?;
    // Container clocks differing by more than100ms are not a measured short source.
    // Small encoder rounding is tolerated; unknown clocks are never estimated.
    if info.media_clocks == 0 || info.max_ms.saturating_sub(info.min_ms) > 100 {
        return Err(bad());
    }
    Ok(())
}
pub(in crate::fal) fn output(b: &[u8], l: Limits) -> Result<(u32, u32), ProviderError> {
    let info = video::probe(b, l, 11)?;
    Ok((info.width, info.height))
}
