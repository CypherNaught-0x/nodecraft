//! The length and frame size of an MP4 or QuickTime movie, read from its headers without decoding
//! anything (#179): the movie header (`mvhd`) gives the length, the first video track's header
//! (`tkhd`) the frame size. A field the file doesn't state, or states as unknown, is `None`.
use crate::MediaError;

/// Boxes read before the file is given up on, so a hostile file of tiny boxes can't spin.
const MAX_BOXES: usize = 4096;

/// What a movie's headers say about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MovieInfo {
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

fn damaged() -> MediaError {
    MediaError::Decode("the movie's headers are damaged".into())
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes)
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at.checked_add(8)?).and_then(|s| <[u8; 8]>::try_from(s).ok()).map(u64::from_be_bytes)
}

/// A header box: its four-letter type and its payload.
type Atom<'a> = ([u8; 4], &'a [u8]);

/// The boxes directly inside `b`.
fn boxes<'a>(b: &'a [u8], seen: &mut usize) -> Result<Vec<Atom<'a>>, MediaError> {
    let mut out = Vec::new();
    walk(b, seen, |atom| {
        out.push(atom);
        false
    })?;
    Ok(out)
}

/// Visits the boxes directly inside `b` until `stop` says so; nothing after that box is read.
fn walk<'a>(b: &'a [u8], seen: &mut usize, mut stop: impl FnMut(Atom<'a>) -> bool) -> Result<(), MediaError> {
    let mut at = 0usize;
    while at < b.len() {
        *seen = seen.saturating_add(1);
        if *seen > MAX_BOXES {
            return Err(MediaError::LimitExceeded("the movie has too many header boxes".into()));
        }
        let size = be32(b, at).ok_or_else(damaged)?;
        let kind: [u8; 4] = b.get(at.saturating_add(4)..at.saturating_add(8)).and_then(|s| s.try_into().ok()).ok_or_else(damaged)?;
        let (header, len) = match size {
            0 => (8, b.len() - at),
            1 => (16, usize::try_from(be64(b, at.saturating_add(8)).ok_or_else(damaged)?).map_err(|_| damaged())?),
            n => (8, usize::try_from(n).map_err(|_| damaged())?),
        };
        let end = at.checked_add(len).filter(|end| len >= header && *end <= b.len()).ok_or_else(damaged)?;
        let payload = b.get(at.saturating_add(header)..end).ok_or_else(damaged)?;
        if stop((kind, payload)) {
            break;
        }
        at = end;
    }
    Ok(())
}

/// The movie length from `mvhd`: its duration in its own timescale.
fn movie_ms(mvhd: &[u8]) -> Option<u64> {
    let (timescale, duration) = match mvhd.first()? {
        0 => (be32(mvhd, 12)?, be32(mvhd, 16).filter(|d| *d != u32::MAX).map(u64::from)?),
        1 => (be32(mvhd, 20)?, be64(mvhd, 24).filter(|d| *d != u64::MAX)?),
        _ => return None,
    };
    // Zero is what a fragmented file writes when the header doesn't know.
    if timescale == 0 || duration == 0 {
        return None;
    }
    // Rounded up: providers bill a started second, so a quote must never read 5.0004 s as 5 s.
    u64::try_from((u128::from(duration) * 1000).div_ceil(u128::from(timescale))).ok()
}

/// A track's frame size from `tkhd`: two 16.16 fixed-point numbers closing the header.
fn track_size(tkhd: &[u8]) -> Option<(u32, u32)> {
    let at = match tkhd.first()? {
        0 => 76,
        1 => 88,
        _ => return None,
    };
    let (w, h) = (be32(tkhd, at)? >> 16, be32(tkhd, at + 4)? >> 16);
    (w > 0 && h > 0).then_some((w, h))
}

/// Whether a track is a video track: its media handler is `vide`.
fn is_video(trak: &[u8], seen: &mut usize) -> Result<bool, MediaError> {
    for (kind, mdia) in boxes(trak, seen)? {
        if &kind == b"mdia" {
            for (kind, hdlr) in boxes(mdia, seen)? {
                if &kind == b"hdlr" {
                    return Ok(hdlr.get(8..12) == Some(b"vide"));
                }
            }
        }
    }
    Ok(false)
}

/// Reads an MP4 or QuickTime movie's length and frame size from its headers.
pub fn probe_movie(bytes: &[u8]) -> Result<MovieInfo, MediaError> {
    let mut seen = 0;
    // Stop at `moov`: a long fragmented file has thousands of boxes after it, and trailing bytes
    // that aren't boxes don't make its headers unreadable.
    let mut moov = None;
    walk(bytes, &mut seen, |(kind, payload)| {
        moov = (&kind == b"moov").then_some(payload);
        moov.is_some()
    })?;
    let moov = moov.ok_or(MediaError::Unrecognized)?;
    let mut info = MovieInfo::default();
    for (kind, payload) in boxes(moov, &mut seen)? {
        match &kind {
            b"mvhd" if info.duration_ms.is_none() => info.duration_ms = movie_ms(payload),
            b"trak" if info.width.is_none() && is_video(payload, &mut seen)? => {
                let tkhd = boxes(payload, &mut seen)?.into_iter().find(|(kind, _)| kind == b"tkhd").and_then(|(_, tkhd)| track_size(tkhd));
                if let Some((w, h)) = tkhd {
                    (info.width, info.height) = (Some(w), Some(h));
                }
            }
            _ => {}
        }
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = u32::try_from(payload.len() + 8).unwrap().to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(payload);
        b
    }
    fn mvhd(timescale: u32, duration: u32) -> Vec<u8> {
        let mut p = vec![0; 100];
        p[12..16].copy_from_slice(&timescale.to_be_bytes());
        p[16..20].copy_from_slice(&duration.to_be_bytes());
        boxed(b"mvhd", &p)
    }
    fn trak(handler: &[u8; 4], w: u32, h: u32) -> Vec<u8> {
        let mut tkhd = vec![0; 84];
        tkhd[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        let mut hdlr = vec![0; 24];
        hdlr[8..12].copy_from_slice(handler);
        [boxed(b"tkhd", &tkhd), boxed(b"mdia", &boxed(b"hdlr", &hdlr))].concat()
    }
    fn movie(moov: &[Vec<u8>]) -> Vec<u8> {
        [boxed(b"ftyp", b"isom\0\0\x02\0isom"), boxed(b"moov", &moov.concat()), boxed(b"mdat", b"x")].concat()
    }

    #[test]
    fn length_and_frame_size_come_from_the_headers() {
        let file = movie(&[mvhd(600, 2_700), boxed(b"trak", &trak(b"soun", 0, 0)), boxed(b"trak", &trak(b"vide", 854, 480))]);
        assert_eq!(probe_movie(&file).unwrap(), MovieInfo { duration_ms: Some(4_500), width: Some(854), height: Some(480) });
        // An unknown length stays unknown.
        assert_eq!(probe_movie(&movie(&[mvhd(600, 0)])).unwrap().duration_ms, None);
        assert_eq!(probe_movie(&movie(&[mvhd(0, 10)])).unwrap().duration_ms, None);
        assert_eq!(probe_movie(&movie(&[mvhd(600, u32::MAX)])).unwrap().duration_ms, None);
    }

    #[test]
    fn version_one_headers_large_boxes_and_open_ended_boxes_are_read() {
        let mut mvhd1 = vec![1; 112];
        mvhd1[20..24].copy_from_slice(&1000u32.to_be_bytes());
        mvhd1[24..32].copy_from_slice(&5_000_400u64.to_be_bytes());
        let mut tkhd1 = vec![1; 96];
        tkhd1[88..92].copy_from_slice(&(1280u32 << 16).to_be_bytes());
        tkhd1[92..96].copy_from_slice(&(720u32 << 16).to_be_bytes());
        let mut hdlr = vec![0; 24];
        hdlr[8..12].copy_from_slice(b"vide");
        let trak = boxed(b"trak", &[boxed(b"tkhd", &tkhd1), boxed(b"mdia", &boxed(b"hdlr", &hdlr))].concat());
        // A 64-bit `moov` size, then an `mdat` of size 0 that runs to the end of the file.
        let inner = [boxed(b"mvhd", &mvhd1), trak].concat();
        let mut file = [&1u32.to_be_bytes()[..], b"moov", &u64::try_from(inner.len() + 16).unwrap().to_be_bytes()].concat();
        file.extend_from_slice(&inner);
        file.extend_from_slice(&[0, 0, 0, 0]);
        file.extend_from_slice(b"mdat0123");
        assert_eq!(probe_movie(&file).unwrap(), MovieInfo { duration_ms: Some(5_000_400), width: Some(1280), height: Some(720) });
        // A third of a second rounds up, so a started second is never lost from a quote.
        assert_eq!(probe_movie(&movie(&[mvhd(3, 1)])).unwrap().duration_ms, Some(334));
        // Junk after `moov` isn't read.
        let mut tail = movie(&[mvhd(600, 2_700)]);
        tail.truncate(tail.len() - 9);
        tail.extend_from_slice(b"junk");
        assert_eq!(probe_movie(&tail).unwrap().duration_ms, Some(4_500));
    }

    #[test]
    fn damaged_headers_are_errors() {
        let good = movie(&[mvhd(600, 2_700)]);
        for cut in 0..good.len() {
            let _ = probe_movie(&good[..cut]);
        }
        assert!(probe_movie(b"not a movie").is_err());
        let mut lying = good.clone();
        lying[3] = 0xff;
        assert!(probe_movie(&lying).is_err());
        let many: Vec<u8> = std::iter::repeat_n(boxed(b"free", b""), MAX_BOXES + 1).flatten().collect();
        assert!(matches!(probe_movie(&many), Err(MediaError::LimitExceeded(_))));
    }
}
