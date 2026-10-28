//! Validate the complete RIFF layout and the uncompressed sample format before retaining audio.
use super::*;

fn u16le(bytes: &[u8], at: usize) -> Result<u16, ProviderError> {
    bytes.get(at..at.checked_add(2).ok_or_else(malformed)?).and_then(|value| <[u8; 2]>::try_from(value).ok()).map(u16::from_le_bytes).ok_or_else(malformed)
}
fn u32le(bytes: &[u8], at: usize) -> Result<u32, ProviderError> {
    bytes.get(at..at.checked_add(4).ok_or_else(malformed)?).and_then(|value| <[u8; 4]>::try_from(value).ok()).map(u32::from_le_bytes).ok_or_else(malformed)
}
struct Format {
    frame: usize,
    byte_rate: u32,
}
fn format(bytes: &[u8]) -> Result<Format, ProviderError> {
    let tag = u16le(bytes, 0)?;
    let channels = u16le(bytes, 2)?;
    let rate = u32le(bytes, 4)?;
    let byte_rate = u32le(bytes, 8)?;
    let alignment = u16le(bytes, 12)?;
    let bits = u16le(bytes, 14)?;
    if channels != 2 || rate != 44_100 || bytes.len() < 16 {
        return Err(malformed());
    }
    // WAVEFORMAT has 16 bytes; WAVEFORMATEX adds cbSize and that many extension bytes.
    if bytes.len() != 16 && (bytes.len() < 18 || usize::from(u16le(bytes, 16)?).checked_add(18) != Some(bytes.len())) {
        return Err(malformed());
    }
    let sample_tag = if tag == 0xfffe {
        if bytes.len() < 40 || u16le(bytes, 16)? < 22 {
            return Err(malformed());
        }
        let valid_bits = u16le(bytes, 18)?;
        let mask = u32le(bytes, 20)?;
        let subformat = u32le(bytes, 24)?;
        // Canonical PCM/IEEE-float KSDATAFORMAT_SUBTYPE GUID, no compressed/custom formats.
        if bytes.get(28..40) != Some(&[0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71])
            || !matches!(mask, 0 | 3)
            || valid_bits == 0
            || valid_bits > bits
            || subformat == 3 && valid_bits != bits
        {
            return Err(malformed());
        }
        u16::try_from(subformat).map_err(|_| malformed())?
    } else {
        tag
    };
    if !matches!((sample_tag, bits), (1, 8 | 16 | 24 | 32) | (3, 32 | 64)) {
        return Err(malformed());
    }
    let frame = channels.checked_mul(bits / 8).ok_or_else(malformed)?;
    if alignment != frame || rate.checked_mul(u32::from(frame)) != Some(byte_rate) {
        return Err(malformed());
    }
    Ok(Format { frame: usize::from(frame), byte_rate })
}
pub(super) fn validate(bytes: &[u8], duration: Option<u16>) -> Result<(), ProviderError> {
    if bytes.len() > AUDIO_BYTES
        || bytes.get(..4) != Some(b"RIFF")
        || bytes.get(8..12) != Some(b"WAVE")
        || usize::try_from(u32le(bytes, 4)?).ok().and_then(|size| size.checked_add(8)) != Some(bytes.len())
    {
        return Err(malformed());
    }
    let mut at = 12usize;
    let mut chunks = 0usize;
    let mut fmt = None;
    let mut data = None;
    let mut fact = None;
    while at < bytes.len() {
        chunks = chunks.checked_add(1).filter(|n| *n <= 4096).ok_or_else(malformed)?;
        let name = bytes.get(at..at.checked_add(4).ok_or_else(malformed)?).ok_or_else(malformed)?;
        let length = usize::try_from(u32le(bytes, at.checked_add(4).ok_or_else(malformed)?)?).map_err(|_| malformed())?;
        let start = at.checked_add(8).ok_or_else(malformed)?;
        let end = start.checked_add(length).filter(|end| *end <= bytes.len()).ok_or_else(malformed)?;
        match name {
            b"fmt " => {
                if fmt.is_some() {
                    return Err(malformed());
                }
                fmt = Some(format(bytes.get(start..end).ok_or_else(malformed)?)?);
            }
            b"data" => {
                if fmt.is_none() || length == 0 || data.replace(length).is_some() {
                    return Err(malformed());
                }
            }
            b"fact" if length < 4 => return Err(malformed()),
            b"fact" => {
                let count = u32le(bytes, start)?;
                if fact.replace(count).is_some() {
                    return Err(malformed());
                }
            }
            _ => {}
        }
        at = end.checked_add(length % 2).filter(|end| *end <= bytes.len()).ok_or_else(malformed)?;
    }
    let fmt = fmt.ok_or_else(malformed)?;
    let length = data.ok_or_else(malformed)?;
    if !length.is_multiple_of(fmt.frame) {
        return Err(malformed());
    }
    if fact.is_some_and(|frames| usize::try_from(frames).ok() != Some(length / fmt.frame)) {
        return Err(malformed());
    }
    let ms = u64::try_from(length).ok().and_then(|n| n.checked_mul(1000)).ok_or_else(malformed)?.div_ceil(u64::from(fmt.byte_rate));
    let limit = duration.map_or(385_000, |seconds| u64::from(seconds).saturating_mul(1000).saturating_add(5000));
    if !(1..=limit).contains(&ms) {
        return Err(malformed());
    }
    Ok(())
}
