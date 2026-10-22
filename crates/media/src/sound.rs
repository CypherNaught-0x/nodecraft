//! The length, sample rate and channel count of a sound file, read from its headers without
//! decoding any audio (#608): WAV, FLAC, Ogg (Vorbis and Opus), MP3, ADTS AAC and MP4 audio.
//!
//! The bytes are untrusted. Every read is bounds-checked, every walk is capped, and a file whose
//! headers can't be read, or don't state a length, comes back with that field `None`: an unknown
//! length, never an error, because the provider still checks what it is sent.
use crate::movie::probe_movie;

/// Chunks (WAV), metadata blocks (FLAC) or ID3 tags read before giving up.
const MAX_HEADERS: usize = 4096;
/// How far from the end an Ogg file's last page is looked for.
const OGG_TAIL: usize = 64 * 1024;
/// How far past its tags an MP3 file's first frame is looked for.
const MPEG_SEARCH: usize = 64 * 1024;
/// ADTS frames counted before giving up (about 6 hours at 48 kHz).
const MAX_ADTS_FRAMES: usize = 1 << 20;

/// What a sound file's headers say about it. An absent field is unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SoundInfo {
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at.checked_add(2)?).and_then(|s| <[u8; 2]>::try_from(s).ok()).map(u16::from_le_bytes)
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_le_bytes)
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at.checked_add(8)?).and_then(|s| <[u8; 8]>::try_from(s).ok()).map(u64::from_le_bytes)
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at.checked_add(4)?).and_then(|s| <[u8; 4]>::try_from(s).ok()).map(u32::from_be_bytes)
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at.checked_add(8)?).and_then(|s| <[u8; 8]>::try_from(s).ok()).map(u64::from_be_bytes)
}

/// `units` at `per_second` units a second, in milliseconds. Rounded up, as the movie probe is:
/// a provider bills a started second, and a limit check must not read 30.0004 s as 30 s.
fn millis(units: u128, per_second: u128) -> Option<u64> {
    if units == 0 || per_second == 0 {
        return None;
    }
    u64::try_from(units.checked_mul(1000)?.div_ceil(per_second)).ok()
}

/// Reads a sound file's length, sample rate and channel count from its headers.
pub fn probe_sound(bytes: &[u8]) -> SoundInfo {
    let at = |offset: usize, magic: &[u8]| bytes.get(offset..offset.saturating_add(magic.len())) == Some(magic);
    let info = if at(0, b"RIFF") && at(8, b"WAVE") {
        wav(bytes)
    } else if at(0, b"fLaC") {
        flac(bytes)
    } else if at(0, b"OggS") {
        ogg(bytes)
    } else if at(4, b"ftyp") {
        probe_movie(bytes).ok().map(|movie| SoundInfo { duration_ms: movie.duration_ms, ..SoundInfo::default() })
    } else if bytes.first() == Some(&0xFF) && bytes.get(1).is_some_and(|b1| b1 & 0xF6 == 0xF0) {
        adts(bytes)
    } else {
        mpeg(bytes)
    };
    info.unwrap_or_default()
}

/// WAV: `fmt ` gives the byte rate, `data` the length of the samples. Either may come first; a
/// walk cut short keeps what it has read.
fn wav(b: &[u8]) -> Option<SoundInfo> {
    let mut info = SoundInfo::default();
    let (mut byte_rate, mut samples) = (None, None);
    let mut at = 12usize;
    for _ in 0..MAX_HEADERS {
        let Some((id, stated, body)) = (|| Some((b.get(at..at.checked_add(4)?)?, le32(b, at.checked_add(4)?)?, at.checked_add(8)?)))() else { break };
        let left = b.len().saturating_sub(body);
        match id {
            b"fmt " => {
                info.channels = le16(b, body.saturating_add(2)).filter(|c| *c > 0);
                info.sample_rate = le32(b, body.saturating_add(4)).filter(|r| *r > 0);
                byte_rate = le32(b, body.saturating_add(8)).filter(|r| *r > 0);
            }
            b"data" => {
                // A streaming writer leaves the size unknown (0 or all ones): the samples run to
                // the end of the file. A size past the end is a cut-off file: its length is unknown.
                let size = if stated == 0 || stated == u32::MAX { left } else { usize::try_from(stated).unwrap_or(usize::MAX) };
                samples = Some((size <= left).then_some(size));
            }
            _ => {}
        }
        if byte_rate.is_some() && samples.is_some() {
            break;
        }
        // Chunks are padded to an even length.
        let Some(next) = usize::try_from(stated).ok().and_then(|size| body.checked_add(size)?.checked_add(size & 1)) else { break };
        at = next;
    }
    if let (Some(rate), Some(Some(samples))) = (byte_rate, samples) {
        info.duration_ms = millis(samples as u128, u128::from(rate));
    }
    Some(info)
}

/// FLAC: the first metadata block is STREAMINFO, which holds the total sample count.
fn flac(b: &[u8]) -> Option<SoundInfo> {
    // STREAMINFO is block type 0 and 34 bytes long; it must come first.
    if b.get(4).map(|h| h & 0x7F) != Some(0) {
        return None;
    }
    let packed = be64(b, 8 + 10)?;
    let rate = u32::try_from(packed >> 44).ok().filter(|r| *r > 0);
    let channels = u16::try_from((packed >> 41) & 0x7).ok().map(|c| c + 1);
    let samples = packed & 0xF_FFFF_FFFF;
    Some(SoundInfo { duration_ms: rate.and_then(|rate| millis(u128::from(samples), u128::from(rate))), sample_rate: rate, channels })
}

/// Ogg: the first page's packet names the codec and its rate, and the last page's granule
/// position is the stream's length in samples.
fn ogg(b: &[u8]) -> Option<SoundInfo> {
    let serial = le32(b, 14)?;
    let segments = usize::from(*b.get(26)?);
    let packet = b.get(27usize.checked_add(segments)?..)?;
    let (rate, granule_rate, channels, skip) = if packet.starts_with(b"\x01vorbis") {
        let rate = le32(packet, 12).filter(|r| *r > 0)?;
        (rate, rate, packet.get(11).copied(), 0)
    } else if packet.starts_with(b"OpusHead") {
        // Opus always counts granules at 48 kHz; the header's rate is the input's, for display.
        let input = le32(packet, 12).filter(|r| *r > 0).unwrap_or(48_000);
        (input, 48_000, packet.get(9).copied(), le16(packet, 10).unwrap_or(0))
    } else {
        return None;
    };
    let mut info = SoundInfo { sample_rate: Some(rate), channels: channels.filter(|c| *c > 0).map(u16::from), duration_ms: None };
    // The last page of this stream with a known granule position (all ones means none ends on it).
    let tail_start = b.len().saturating_sub(OGG_TAIL);
    let tail = b.get(tail_start..)?;
    let last = (0..tail.len().saturating_sub(27)).rev().find_map(|at| {
        let page = tail.get(at..)?;
        let granule = le64(page, 6)?;
        (page.starts_with(b"OggS") && page.get(4) == Some(&0) && le32(page, 14) == Some(serial) && granule != u64::MAX).then_some(granule)
    });
    info.duration_ms = last.and_then(|granule| millis(u128::from(granule.saturating_sub(u64::from(skip))), u128::from(granule_rate)));
    Some(info)
}

/// The bitrates of MPEG audio in kbit/s, by version (1, or 2 and 2.5) and layer (I, II, III).
const BITRATES: [[[u16; 15]; 3]; 2] = [
    [
        [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    ],
    [
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ],
];

/// One MPEG audio frame header.
#[derive(Clone, Copy)]
struct Frame {
    /// 1 for MPEG-1, 2 for MPEG-2 and MPEG-2.5.
    version: u8,
    /// 1, 2 or 3.
    layer: u8,
    kbps: u32,
    rate: u32,
    mono: bool,
    len: usize,
}

impl Frame {
    fn read(h: &[u8]) -> Option<Self> {
        let (b0, b1, b2, b3) = (*h.first()?, *h.get(1)?, *h.get(2)?, *h.get(3)?);
        if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
            return None;
        }
        let (rates, version) = match (b1 >> 3) & 0b11 {
            0b11 => ([44_100, 48_000, 32_000], 1),
            0b10 => ([22_050, 24_000, 16_000], 2),
            0b00 => ([11_025, 12_000, 8_000], 2),
            _ => return None,
        };
        let layer = match (b1 >> 1) & 0b11 {
            0b11 => 1,
            0b10 => 2,
            0b01 => 3,
            _ => return None,
        };
        let kbps = u32::from(*BITRATES.get(usize::from(version - 1))?.get(usize::from(layer - 1))?.get(usize::from(b2 >> 4))?);
        let rate = *rates.get(usize::from((b2 >> 2) & 0b11))?;
        if kbps == 0 {
            return None;
        }
        let padding = u32::from((b2 >> 1) & 1);
        let len = match (layer, version) {
            (1, _) => (12 * kbps * 1000 / rate + padding) * 4,
            (3, 2) => 72 * kbps * 1000 / rate + padding,
            _ => 144 * kbps * 1000 / rate + padding,
        };
        Some(Frame { version, layer, kbps, rate, mono: b3 >> 6 == 0b11, len: usize::try_from(len).ok().filter(|l| *l > 4)? })
    }
    fn samples(&self) -> u32 {
        match (self.layer, self.version) {
            (1, _) => 384,
            (3, 2) => 576,
            _ => 1152,
        }
    }
}

/// MP3 (and MP2): past any ID3v2 tags, the first frame. A Xing, Info or VBRI header in it counts
/// the frames of a variable-bitrate file; otherwise the bitrate is constant and the size says it.
fn mpeg(b: &[u8]) -> Option<SoundInfo> {
    let mut start = 0usize;
    for _ in 0..MAX_HEADERS {
        let tag = b.get(start..)?;
        if !tag.starts_with(b"ID3") {
            break;
        }
        let size = tag.get(6..10)?.iter().try_fold(0usize, |size, byte| (byte & 0x80 == 0).then_some((size << 7) | usize::from(*byte)))?;
        let footer = if tag.get(5)? & 0x10 != 0 { 10 } else { 0 };
        start = start.checked_add(10)?.checked_add(size)?.checked_add(footer)?;
    }
    // The first frame whose successor also starts a frame, so a stray 0xFF in a tag is passed by.
    let search = b.get(start..)?;
    let (offset, frame) = (0..search.len().min(MPEG_SEARCH)).find_map(|at| {
        let frame = Frame::read(search.get(at..)?)?;
        let next = search.get(at.checked_add(frame.len)?..);
        // A file of exactly one frame has nothing after it.
        (next.is_none_or(<[u8]>::is_empty) || next.and_then(Frame::read).is_some()).then_some((at, frame))
    })?;
    let first = search.get(offset..)?;
    let info = SoundInfo { sample_rate: Some(frame.rate), channels: Some(if frame.mono { 1 } else { 2 }), duration_ms: None };
    let side = match (frame.version, frame.mono) {
        (1, false) => 32,
        (1, true) | (_, false) => 17,
        (_, true) => 9,
    };
    let xing = 4 + side;
    let frames = if frame.layer == 3 && matches!(first.get(xing..xing + 4), Some(b"Xing" | b"Info")) {
        be32(first, xing + 4).filter(|flags| flags & 1 != 0).and_then(|_| be32(first, xing + 8))
    } else if first.get(36..40) == Some(b"VBRI") {
        be32(first, 36 + 14)
    } else {
        None
    };
    let duration_ms = match frames {
        Some(frames) => millis(u128::from(frames) * u128::from(frame.samples()), u128::from(frame.rate)),
        None => {
            // Constant bitrate: the audio's bytes at its bitrate, less the tags at the end.
            let audio = trailing_tags(b).saturating_sub(start).saturating_sub(offset);
            millis(audio as u128 * 8, u128::from(frame.kbps) * 1000)
        }
    };
    Some(SoundInfo { duration_ms, ..info })
}

/// Where an MP3's audio ends: before an ID3v1 tag, an APEv2 tag and a Lyrics3 v2 tag, which
/// writers append in that order from the end.
fn trailing_tags(b: &[u8]) -> usize {
    let mut end = b.len();
    let tail = |end: usize, len: usize| end.checked_sub(len).and_then(|start| b.get(start..end));
    if tail(end, 128).is_some_and(|tag| tag.starts_with(b"TAG")) {
        end = end.saturating_sub(128);
    }
    // Either of the other two may sit nearer the end; each is looked for once.
    for _ in 0..2 {
        if let Some(footer) = tail(end, 32).filter(|f| f.starts_with(b"APETAGEX")) {
            // The size counts the items and the footer; a flagged header adds 32 bytes more.
            let size = le32(footer, 12).and_then(|s| usize::try_from(s).ok());
            let header = if le32(footer, 20).is_some_and(|flags| flags & (1 << 31) != 0) { 32 } else { 0 };
            if let Some(start) = size.and_then(|size| end.checked_sub(size)?.checked_sub(header)) {
                end = start;
                continue;
            }
        }
        if let Some(footer) = tail(end, 15).filter(|f| f.ends_with(b"LYRICS200")) {
            // Six digits give the size of the tag before them.
            let size = footer.get(..6).and_then(|d| std::str::from_utf8(d).ok()).and_then(|d| d.parse::<usize>().ok());
            if let Some(start) = size.and_then(|size| end.checked_sub(15)?.checked_sub(size)) {
                end = start;
                continue;
            }
        }
        break;
    }
    end
}

/// The ADTS sample rates, by index.
const ADTS_RATES: [u32; 13] = [96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000, 7_350];

/// ADTS AAC: no header states the length, so the frames are counted, 1024 samples a block.
fn adts(b: &[u8]) -> Option<SoundInfo> {
    let mut at = 0usize;
    let mut samples = 0u64;
    let mut first = None;
    // Whether the frames ran out before the cap: a count cut short by it isn't the length.
    let mut counted = false;
    for _ in 0..MAX_ADTS_FRAMES {
        let h = match b.get(at..at.saturating_add(7)) {
            Some(h) if h.len() == 7 => h,
            _ => {
                counted = true;
                break;
            }
        };
        let (b1, b2, b3) = (*h.get(1)?, *h.get(2)?, *h.get(3)?);
        if *h.first()? != 0xFF || b1 & 0xF6 != 0xF0 {
            counted = true;
            break;
        }
        let rate = *ADTS_RATES.get(usize::from((b2 >> 2) & 0x0F))?;
        let channels = u16::from(((b2 & 1) << 2) | (b3 >> 6));
        let len = (usize::from(b3 & 0b11) << 11) | (usize::from(*h.get(4)?) << 3) | usize::from(*h.get(5)? >> 5);
        // A cut-off last frame, or a change of rate (not one stream), ends the count.
        let Some(end) = at.checked_add(len).filter(|end| len >= 7 && *end <= b.len()) else {
            counted = true;
            break;
        };
        if first.is_some_and(|(r, _)| r != rate) {
            counted = true;
            break;
        }
        first.get_or_insert((rate, channels));
        samples = samples.saturating_add(1024 * (u64::from(*h.get(6)? & 0b11) + 1));
        at = end;
    }
    let (rate, channels) = first?;
    let duration_ms = if counted || at >= b.len() { millis(u128::from(samples), u128::from(rate)) } else { None };
    Some(SoundInfo { duration_ms, sample_rate: Some(rate), channels: Some(channels).filter(|c| *c > 0) })
}

#[cfg(test)]
mod tests;
