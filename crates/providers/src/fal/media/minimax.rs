//! Model-scoped complete container admission, with no playback or whole-audio allocation.
use super::*;
pub(in crate::fal) struct Proof {
    pub mime: &'static str,
    pub duration_ms: u64,
    pub width: Option<u64>,
    pub height: Option<u64>,
}
fn mp3(b: &[u8]) -> Result<u64, ProviderError> {
    let mut at = 0usize;
    for _ in 0..4 {
        if b.get(at..at.saturating_add(3)) != Some(b"ID3") {
            break;
        }
        let header = b.get(at..at.checked_add(10).ok_or_else(bad)?).ok_or_else(bad)?;
        let version = header.get(3).copied().ok_or_else(bad)?;
        if !(2..=4).contains(&version) {
            return Err(bad());
        }
        let length = header
            .get(6..10)
            .ok_or_else(bad)?
            .iter()
            .try_fold(0usize, |n, byte| if byte & 128 != 0 { None } else { n.checked_mul(128)?.checked_add(usize::from(*byte)) })
            .filter(|n| *n <= 1024 * 1024)
            .ok_or_else(bad)?;
        let footer = if header.get(5).copied().unwrap_or(0) & 16 != 0 {
            if version != 4 {
                return Err(bad());
            }
            10
        } else {
            0
        };
        at = at.checked_add(10).and_then(|n| n.checked_add(length)).and_then(|n| n.checked_add(footer)).filter(|n| *n <= b.len()).ok_or_else(bad)?;
    }
    let mut end = b.len();
    if end.checked_sub(128).and_then(|n| b.get(n..n + 3)) == Some(b"TAG") {
        end -= 128;
    }
    // An optional APE footer is bounded metadata; its byte count includes that footer.
    if let Some(footer) = end.checked_sub(32).and_then(|n| b.get(n..end)).filter(|s| s.starts_with(b"APETAGEX")) {
        let size = usize::try_from(u32le(footer, 12)?).map_err(|_| bad())?;
        if !(32..=1024 * 1024).contains(&size) {
            return Err(bad());
        }
        end = end.checked_sub(size).ok_or_else(bad)?;
    }
    let mut frames = 0u64;
    const RATES: [u16; 16] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0];
    while at < end {
        let h = b.get(at..at.checked_add(4).ok_or_else(bad)?).ok_or_else(bad)?;
        let b1 = h.get(1).copied().ok_or_else(bad)?;
        let b2 = h.get(2).copied().ok_or_else(bad)?;
        let b3 = h.get(3).copied().ok_or_else(bad)?;
        // MP3 layer III, MPEG1, fixed requested 32kHz mono. Every byte must belong to a full frame or admitted tag.
        if h.first() != Some(&255) || b1 & 254 != 250 || b2 & 12 != 8 || b3 >> 6 != 3 || b3 & 3 == 2 {
            return Err(bad());
        }
        let kbps = u64::from(*RATES.get(usize::from(b2 >> 4)).filter(|n| **n > 0).ok_or_else(bad)?);
        let length = usize::try_from(144000 * kbps / 32000 + u64::from((b2 >> 1) & 1)).map_err(|_| bad())?;
        at = at.checked_add(length).filter(|n| *n <= end).ok_or_else(bad)?;
        frames = frames.checked_add(1).filter(|n| *n <= 100_000).ok_or_else(bad)?;
    }
    if at != end || frames == 0 {
        return Err(bad());
    }
    frames.checked_mul(36).ok_or_else(bad)
}
fn music(b: &[u8]) -> Result<u64, ProviderError> {
    wav(b)?;
    let mut at = 12usize;
    let mut data = None;
    let mut correct = false;
    while at < b.len() {
        let size = usize::try_from(u32le(b, at + 4)?).map_err(|_| bad())?;
        let start = at.checked_add(8).ok_or_else(bad)?;
        match b.get(at..at + 4) {
            Some(b"fmt ") => correct = u16le(b, start)? == 1 && u16le(b, start + 2)? == 2 && u32le(b, start + 4)? == 44100 && u16le(b, start + 14)? == 16,
            Some(b"data") => data = Some(u64::try_from(size).map_err(|_| bad())?),
            _ => {}
        }
        at = start.checked_add(size).and_then(|n| n.checked_add(size % 2)).ok_or_else(bad)?;
    }
    if !correct {
        return Err(bad());
    }
    data.ok_or_else(bad)?.checked_mul(1000).map(|n| n.div_ceil(176400)).ok_or_else(bad)
}
pub(in crate::fal) fn validate(b: &[u8], r: Record, l: Limits) -> Result<Proof, ProviderError> {
    if b.is_empty() || b.len() > l.output_bytes {
        return Err(bad());
    }
    let mut proof = Proof { mime: "", duration_ms: 0, width: None, height: None };
    match r.model {
        Model::MiniMaxText | Model::MiniMaxFrames => {
            mp4(b)?;
            let meta = nodecraft_media::probe_movie(b).map_err(|_| bad())?;
            proof.duration_ms = meta.duration_ms.ok_or_else(bad)?;
            let width = meta.width.filter(|n| *n > 0 && *n <= l.output_width).ok_or_else(bad)?;
            let height = meta.height.filter(|n| *n > 0 && *n <= l.output_height).ok_or_else(bad)?;
            if u64::from(width).checked_mul(u64::from(height)).is_none_or(|n| n > l.total_pixels) {
                return Err(bad());
            }
            proof.width = Some(u64::from(width));
            proof.height = Some(u64::from(height));
            proof.mime = "video/mp4";
        }
        Model::MiniMaxSpeech => {
            proof.duration_ms = mp3(b)?;
            proof.mime = "audio/mpeg";
        }
        Model::MiniMaxMusic => {
            proof.duration_ms = music(b)?;
            proof.mime = "audio/wav";
        }
        _ => return Err(bad()),
    }
    if matches!(r.model, Model::MiniMaxSpeech | Model::MiniMaxMusic) {
        // History uses this same bounded public probe. Do not let a forged Xing/VBRI sample
        // count replace the complete frame/PCM duration admitted above.
        let info = nodecraft_media::probe_sound(b);
        let (rate, channels, tolerance) = if r.model == Model::MiniMaxSpeech { (32000, 1, 36) } else { (44100, 2, 1) };
        if info.sample_rate != Some(rate)
            || info.channels != Some(channels)
            || info.duration_ms.is_none_or(|ms| ms == 0 || ms.abs_diff(proof.duration_ms) > tolerance)
        {
            return Err(bad());
        }
    }
    let tolerance = if r.model.video() { 1000 } else { 1 };
    if proof.duration_ms == 0 || proof.duration_ms > u64::from(r.duration_ms).saturating_add(tolerance) {
        return Err(bad());
    }
    Ok(proof)
}
