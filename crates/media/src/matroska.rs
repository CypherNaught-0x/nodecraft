//! The length and frame size of a Matroska or WebM file, read from its headers (#608): the
//! segment's `Info` gives the length, the first video track's `Video` element the frame size.
//! Clusters (the media itself) are skipped by size, never read.
use crate::{MediaError, movie::MovieInfo};
use std::cell::Cell;

/// Elements visited before the file is given up on, so a hostile file of tiny elements can't spin.
const MAX_ELEMENTS: usize = 4096;

const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_TYPE: u32 = 0x83;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const CLUSTER: u32 = 0x1F43_B675;

fn damaged() -> MediaError {
    MediaError::Decode("the video's headers are damaged".into())
}

/// A variable-length integer at `at`: its value (with the length marker kept for an element id,
/// stripped for a size) and its length. A size of all ones is unknown, returned as `None`.
fn vint(b: &[u8], at: usize, keep_marker: bool) -> Result<(Option<u64>, usize), MediaError> {
    let first = *b.get(at).ok_or_else(damaged)?;
    let len = usize::try_from(first.leading_zeros()).map_err(|_| damaged())? + 1;
    if len > 8 {
        return Err(damaged());
    }
    let bytes = b.get(at..at.checked_add(len).ok_or_else(damaged)?).ok_or_else(damaged)?;
    let mut value = 0u64;
    for byte in bytes {
        value = (value << 8) | u64::from(*byte);
    }
    let marker = 1u64 << (7 * len);
    if keep_marker {
        return Ok((Some(value), len));
    }
    let value = value & (marker - 1);
    Ok(((value != marker - 1).then_some(value), len))
}

/// One element: its id and its payload (to the end of `b` when its size is unknown).
type Element<'a> = (u32, &'a [u8]);

/// Visits the elements directly inside `b` until `visit` says to stop.
fn walk<'a>(b: &'a [u8], seen: &Cell<usize>, mut visit: impl FnMut(Element<'a>) -> Result<bool, MediaError>) -> Result<(), MediaError> {
    let mut at = 0usize;
    while at < b.len() {
        seen.set(seen.get().saturating_add(1));
        if seen.get() > MAX_ELEMENTS {
            return Err(MediaError::LimitExceeded("the video has too many header elements".into()));
        }
        let (id, id_len) = vint(b, at, true)?;
        let id = u32::try_from(id.ok_or_else(damaged)?).map_err(|_| damaged())?;
        let (size, size_len) = vint(b, at.checked_add(id_len).ok_or_else(damaged)?, false)?;
        let body = at.checked_add(id_len).and_then(|at| at.checked_add(size_len)).ok_or_else(damaged)?;
        // An unknown size (a live recording's segment or cluster) runs to the end of its parent.
        let end = match size {
            Some(size) => body.checked_add(usize::try_from(size).map_err(|_| damaged())?).ok_or_else(damaged)?,
            None => b.len(),
        };
        // A cut-off last element gives what is there: a recording's headers come first.
        let payload = b.get(body..end.min(b.len())).ok_or_else(damaged)?;
        if visit((id, payload))? {
            break;
        }
        at = end;
    }
    Ok(())
}

fn uint(b: &[u8]) -> Option<u64> {
    (b.len() <= 8).then(|| b.iter().fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
}

fn float(b: &[u8]) -> Option<f64> {
    let value = match b.len() {
        4 => f64::from(f32::from_be_bytes(b.try_into().ok()?)),
        8 => f64::from_be_bytes(b.try_into().ok()?),
        _ => return None,
    };
    (value.is_finite() && value > 0.0).then_some(value)
}

/// The length in milliseconds of `duration` ticks of `scale` nanoseconds, rounded up.
fn duration_ms(duration: f64, scale: u64) -> Option<u64> {
    // u64 nanoseconds per tick are exact in f64 up to 2^53; a real scale is about a million.
    #[allow(clippy::cast_precision_loss)]
    let ms = (duration * scale as f64 / 1_000_000.0).ceil();
    // Past ten thousand years the header is lying; and the cast must only see a finite number.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    if ms.is_finite() && ms > 0.0 && ms < 3.2e14 { Some(ms as u64) } else { None }
}

/// The frame size of the first video track among `tracks`.
fn video_size(tracks: &[u8], seen: &Cell<usize>) -> Result<Option<(u32, u32)>, MediaError> {
    let mut size = None;
    walk(tracks, seen, |(id, entry)| {
        if id != TRACK_ENTRY {
            return Ok(false);
        }
        let (mut is_video, mut w, mut h) = (false, None, None);
        walk(entry, seen, |(id, payload)| {
            match id {
                TRACK_TYPE => is_video = uint(payload) == Some(1),
                VIDEO => walk(payload, seen, |(id, payload)| {
                    let value = uint(payload).and_then(|v| u32::try_from(v).ok()).filter(|v| *v > 0);
                    match id {
                        PIXEL_WIDTH => w = value,
                        PIXEL_HEIGHT => h = value,
                        _ => {}
                    }
                    Ok(false)
                })?,
                _ => {}
            }
            Ok(false)
        })?;
        if let (true, Some(w), Some(h)) = (is_video, w, h) {
            size = Some((w, h));
        }
        Ok(size.is_some())
    })?;
    Ok(size)
}

/// Reads a Matroska or WebM file's length and frame size from its headers.
pub(crate) fn probe_matroska(bytes: &[u8]) -> Result<MovieInfo, MediaError> {
    let seen = Cell::new(0);
    let mut segment = None;
    walk(bytes, &seen, |(id, payload)| {
        segment = (id == SEGMENT).then_some(payload);
        Ok(segment.is_some())
    })?;
    let segment = segment.ok_or(MediaError::Unrecognized)?;
    let mut info = MovieInfo::default();
    let (mut found_info, mut found_tracks) = (false, false);
    walk(segment, &seen, |(id, payload)| {
        match id {
            INFO if !found_info => {
                found_info = true;
                let (mut scale, mut duration) = (1_000_000u64, None);
                walk(payload, &seen, |(id, payload)| {
                    match id {
                        TIMESTAMP_SCALE => scale = uint(payload).filter(|s| *s > 0).unwrap_or(scale),
                        DURATION => duration = float(payload),
                        _ => {}
                    }
                    Ok(false)
                })?;
                info.duration_ms = duration.and_then(|duration| duration_ms(duration, scale));
            }
            TRACKS if !found_tracks => {
                found_tracks = true;
                if let Some((w, h)) = video_size(payload, &seen)? {
                    (info.width, info.height) = (Some(w), Some(h));
                }
            }
            _ => {}
        }
        // The media follows the headers: once it starts, or both are read, stop.
        Ok(id == CLUSTER || (found_info && found_tracks))
    })?;
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An element with a one- to four-byte id and an eight-byte size.
    fn el(id: u32, payload: &[u8]) -> Vec<u8> {
        let id = id.to_be_bytes();
        let start = id.iter().position(|b| *b != 0).unwrap();
        let mut out = id[start..].to_vec();
        out.push(0x01);
        out.extend_from_slice(&u64::try_from(payload.len()).unwrap().to_be_bytes()[1..]);
        out.extend_from_slice(payload);
        out
    }
    fn file(segment: &[Vec<u8>]) -> Vec<u8> {
        [el(0x1A45_DFA3, &el(0x4282, b"webm")), el(SEGMENT, &segment.concat())].concat()
    }
    fn info(scale: Option<u32>, duration: &[u8]) -> Vec<u8> {
        let scale = scale.map(|s| el(TIMESTAMP_SCALE, &s.to_be_bytes())).unwrap_or_default();
        el(INFO, &[scale, el(DURATION, duration)].concat())
    }
    fn track(kind: u8, w: u16, h: u16) -> Vec<u8> {
        let video = el(VIDEO, &[el(PIXEL_WIDTH, &w.to_be_bytes()), el(PIXEL_HEIGHT, &h.to_be_bytes())].concat());
        el(TRACK_ENTRY, &[el(TRACK_TYPE, &[kind]), video].concat())
    }

    #[test]
    fn length_and_frame_size_come_from_the_headers() {
        let webm = file(&[info(None, &4_500.0f64.to_be_bytes()), el(TRACKS, &[track(2, 0, 0), track(1, 1280, 720)].concat()), el(CLUSTER, b"media")]);
        assert_eq!(probe_matroska(&webm).unwrap(), MovieInfo { duration_ms: Some(4_500), width: Some(1280), height: Some(720) });
        // A float duration in ticks of 100 µs, rounded up.
        let scaled = file(&[info(Some(100_000), &12.34f32.to_be_bytes())]);
        assert_eq!(probe_matroska(&scaled).unwrap().duration_ms, Some(2));
        // No length, or a lying one, is unknown.
        for duration in [f64::NAN.to_be_bytes(), f64::INFINITY.to_be_bytes(), (-1.0f64).to_be_bytes(), 1e300f64.to_be_bytes()] {
            assert_eq!(probe_matroska(&file(&[info(None, &duration)])).unwrap().duration_ms, None);
        }
        // An unknown-size segment, as a live recording writes, runs to the end of the file.
        let live = [el(0x1A45_DFA3, b""), vec![0x18, 0x53, 0x80, 0x67, 0xFF], info(None, &1_000.0f64.to_be_bytes())].concat();
        assert_eq!(probe_matroska(&live).unwrap().duration_ms, Some(1_000));
    }

    #[test]
    fn damaged_headers_are_errors_or_unknowns_never_panics() {
        let good = file(&[info(None, &4_500.0f64.to_be_bytes()), el(TRACKS, &track(1, 640, 480))]);
        for cut in 0..good.len() {
            let _ = probe_matroska(&good[..cut]);
        }
        for byte in 0..good.len() {
            let mut bad = good.clone();
            bad[byte] ^= 0xFF;
            let _ = probe_matroska(&bad);
        }
        assert!(probe_matroska(&[0u8; 16]).is_err(), "a zero byte is no length marker");
        // Thousands of empty elements stop at the cap.
        let many = [el(0x1A45_DFA3, b""), el(SEGMENT, &[0xEC, 0x80].repeat(10_000))].concat();
        assert!(matches!(probe_matroska(&many), Err(MediaError::LimitExceeded(_))));
    }
}
