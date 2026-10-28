//! Format detection from the leading bytes ("magic numbers"), never from file names, so a renamed
//! or extension-less paste is still recognised and a mislabelled file can't pick the wrong decoder.

/// The broad kind of a piece of media; decides which source node kind a paste or drop becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Model3d,
    Document,
    Text,
    /// A trained style's descriptor ([`crate::style`]).
    Style,
}

/// The result of [`sniff`]: a MIME type and its kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Sniffed {
    pub mime: &'static str,
    pub kind: MediaKind,
}

impl Sniffed {
    const fn new(mime: &'static str, kind: MediaKind) -> Self {
        Self { mime, kind }
    }
}

/// How much of the input [`sniff`] looks at.
const SNIFF_WINDOW: usize = 1024;

/// Detects the format of `bytes` from its signature. Returns `None` for empty input and for
/// binary data NodeCraft doesn't know. Valid UTF-8 without control characters is `text/plain`;
/// text with a UTF-16 byte-order mark is `text/plain; charset=utf-16le` (or `be`).
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    use MediaKind::*;
    let head = bytes.get(..bytes.len().min(SNIFF_WINDOW)).unwrap_or(bytes);
    let at = |offset: usize, magic: &[u8]| offset.checked_add(magic.len()).and_then(|end| head.get(offset..end)).is_some_and(|s| s == magic);

    let u32_le = |offset: usize| head.get(offset..offset.checked_add(4)?).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);

    let found = if at(0, b"\xFF\xFE") {
        Sniffed::new("text/plain; charset=utf-16le", Text)
    } else if at(0, b"\xFE\xFF") {
        Sniffed::new("text/plain; charset=utf-16be", Text)
    } else if at(0, b"\x89PNG\r\n\x1a\n") {
        Sniffed::new("image/png", Image)
    } else if at(0, b"\x76\x2f\x31\x01") {
        // Recognition only: generic OpenEXR magic does not certify the ACES subset.
        Sniffed::new("image/x-exr", Image)
    } else if at(0, b"#?RGBE\n") || at(0, b"#?RADIANCE\n") {
        // The complete first line outranks text, even in a header-only input.
        Sniffed::new("image/vnd.radiance", Image)
    } else if at(0, b"\xFF\xD8\xFF") {
        Sniffed::new("image/jpeg", Image)
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        Sniffed::new("image/gif", Image)
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        Sniffed::new("image/webp", Image)
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        Sniffed::new("audio/wav", Audio)
    } else if at(0, b"II*\0") || at(0, b"MM\0*") || at(0, b"II+\0") || at(0, b"MM\0+") {
        Sniffed::new("image/tiff", Image)
    } else if at(0, b"BM") && at(6, b"\0\0\0\0") && u32_le(14).is_some_and(|dib| matches!(dib, 12 | 40 | 52 | 56 | 64 | 108 | 124)) {
        // The reserved field must be zero and the DIB header size a known one, so prose that
        // happens to start with "BM" stays text.
        Sniffed::new("image/bmp", Image)
    } else if at(4, b"ftyp") {
        sniff_iso_bmff(head)
    } else if at(0, b"\x1A\x45\xDF\xA3") {
        if contains(head, b"webm") { Sniffed::new("video/webm", Video) } else { Sniffed::new("video/x-matroska", Video) }
    } else if at(0, b"OggS") {
        Sniffed::new("audio/ogg", Audio)
    } else if at(0, b"fLaC") {
        Sniffed::new("audio/flac", Audio)
    } else if at(0, b"ID3") && head.get(3).is_some_and(|v| (2..=4).contains(v)) && head.get(6..10).is_some_and(|size| size.iter().all(|b| b & 0x80 == 0)) {
        Sniffed::new("audio/mpeg", Audio)
    } else if let Some(audio) = sniff_mpeg_frame(head) {
        audio
    } else if at(0, b"%PDF-") {
        Sniffed::new("application/pdf", Document)
    } else if at(0, b"glTF") {
        Sniffed::new("model/gltf-binary", Model3d)
    } else if crate::mesh::recognizes_stl(bytes) {
        Sniffed::new("model/stl", Model3d)
    } else if crate::mesh::recognizes_ply(bytes) {
        Sniffed::new("model/ply", Model3d)
    } else if at(0, crate::style::MAGIC) {
        Sniffed::new(crate::style::MIME, Style)
    } else if crate::svg::recognizes(bytes) {
        Sniffed::new(crate::svg::MIME, Image)
    } else if looks_like_text(head, bytes.len() > head.len()) {
        if looks_like_gltf_json(head) {
            Sniffed::new("model/gltf+json", Model3d)
        } else if looks_like_obj(head, bytes.len() > head.len()) {
            Sniffed::new("model/obj", Model3d)
        } else {
            Sniffed::new("text/plain", Text)
        }
    } else {
        return None;
    };
    Some(found)
}

/// ISO base media files (MP4, MOV, AVIF, HEIF) share the `ftyp` box. Its major brand, then its
/// compatible brands, tell them apart: an AVIF file may have the generic `mif1` as major brand.
fn sniff_iso_bmff(head: &[u8]) -> Sniffed {
    use MediaKind::*;
    let box_end = head.get(0..4).and_then(|b| b.try_into().ok()).map_or(0, u32::from_be_bytes);
    let box_end = usize::try_from(box_end).unwrap_or(usize::MAX).min(head.len());
    let major = head.get(8..12);
    let compatible = head.get(16..box_end).unwrap_or_default().as_chunks::<4>().0.iter().map(|c| c.as_slice());
    let brands: Vec<&[u8]> = major.into_iter().chain(compatible).collect();
    let any = |set: &[&[u8]]| brands.iter().any(|b| set.contains(b));
    if any(&[b"avif", b"avis"]) {
        return Sniffed::new("image/avif", Image);
    }
    if any(&[b"heic", b"heix", b"heim", b"heis"]) {
        return Sniffed::new("image/heic", Image);
    }
    match major {
        Some(b"mif1" | b"msf1") => Sniffed::new("image/heif", Image),
        Some(b"qt  ") => Sniffed::new("video/quicktime", Video),
        Some(b"M4A " | b"M4B ") => Sniffed::new("audio/mp4", Audio),
        _ => Sniffed::new("video/mp4", Video),
    }
}

/// A bare MPEG audio frame header (MP3 without ID3 tags) or an ADTS AAC header. Four header
/// bytes are checked, including the bitrate and sample-rate fields, to keep false positives rare.
fn sniff_mpeg_frame(head: &[u8]) -> Option<Sniffed> {
    let (&b0, &b1, &b2) = (head.first()?, head.get(1)?, head.get(2)?);
    head.get(3)?;
    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let layer = (b1 >> 1) & 0b11;
    let version = (b1 >> 3) & 0b11;
    if layer == 0 && b1 & 0xF0 == 0xF0 {
        // ADTS: sampling-frequency index 0..=12 are defined.
        ((b2 >> 2) & 0x0F <= 12).then_some(Sniffed::new("audio/aac", MediaKind::Audio))
    } else if layer != 0 && version != 0b01 && !matches!(b2 >> 4, 0 | 0x0F) && (b2 >> 2) & 0b11 != 0b11 {
        Some(Sniffed::new("audio/mpeg", MediaKind::Audio))
    } else {
        None
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// glTF JSON: an object with an `asset` naming version 2 and one of the keys a scene needs, all
/// in the window (exporters write `asset` first). Other JSON stays text.
fn looks_like_gltf_json(head: &[u8]) -> bool {
    let body = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head).trim_ascii_start();
    body.starts_with(b"{")
        && contains(body, b"\"asset\"")
        && contains(body, b"\"version\"")
        && contains(body, b"\"2.")
        && [b"\"scenes\"".as_slice(), b"\"nodes\"", b"\"meshes\"", b"\"buffers\"", b"\"accessors\""].iter().any(|key| contains(body, key))
}

/// Wavefront OBJ: every complete line in the window is blank, a comment or an OBJ statement, and
/// at least one is a vertex with three numbers. Prose doesn't pass that.
fn looks_like_obj(head: &[u8], truncated: bool) -> bool {
    let mut lines: Vec<&[u8]> = head.split(|&b| b == b'\n').collect();
    if truncated {
        lines.pop();
    }
    let mut vertex = false;
    for line in lines {
        let line = line.trim_ascii();
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let Ok(line) = std::str::from_utf8(line) else { return false };
        let mut words = line.split_ascii_whitespace();
        match words.next() {
            Some("v") => {
                let numbers = words.map(|w| w.parse::<f32>()).collect::<Result<Vec<_>, _>>();
                if !numbers.is_ok_and(|n| matches!(n.len(), 3 | 4 | 6 | 7)) {
                    return false;
                }
                vertex = true;
            }
            Some("vn" | "vt" | "vp" | "f" | "o" | "g" | "s" | "mtllib" | "usemtl" | "l" | "p") => {}
            _ => return false,
        }
    }
    vertex
}

/// UTF-8 without NULs or control characters other than tab, CR, LF and form feed. `truncated`
/// means the window was cut short, so an incomplete final character is allowed.
fn looks_like_text(head: &[u8], truncated: bool) -> bool {
    if head.is_empty() {
        return false;
    }
    let valid = match std::str::from_utf8(head) {
        Ok(s) => s,
        Err(e) if truncated && e.error_len().is_none() => match head.get(..e.valid_up_to()).map(std::str::from_utf8) {
            Some(Ok(s)) => s,
            _ => return false,
        },
        Err(_) => return false,
    };
    valid.chars().all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r' | '\x0C'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mime(bytes: &[u8]) -> Option<&'static str> {
        sniff(bytes).map(|s| s.mime)
    }

    #[test]
    fn recognises_images() {
        assert_eq!(mime(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"), Some("image/png"));
        assert_eq!(mime(b"\xFF\xD8\xFF\xE0\0\x10JFIF"), Some("image/jpeg"));
        assert_eq!(mime(b"GIF89a\x01\0\x01\0"), Some("image/gif"));
        assert_eq!(mime(b"RIFF\x24\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(mime(b"II*\0\x08\0\0\0"), Some("image/tiff"));
        assert_eq!(mime(b"MM\0*\0\0\0\x08"), Some("image/tiff"));
        let mut bmp = b"BM".to_vec();
        bmp.resize(54, 0);
        bmp[14] = 40;
        assert_eq!(mime(&bmp), Some("image/bmp"));
        assert_eq!(mime(b"\0\0\0\x1cftypavif\0\0\0\0"), Some("image/avif"));
        assert_eq!(mime(b"\0\0\0\x18ftypheic\0\0\0\0"), Some("image/heic"));
        assert_eq!(mime(b"\0\0\0\x18ftypmif1\0\0\0\0avif"), Some("image/avif"), "AVIF by compatible brand");
        assert_eq!(mime(b"\0\0\0\x18ftypmif1\0\0\0\0heic"), Some("image/heic"));
        assert_eq!(mime(b"\0\0\0\x10ftypmif1\0\0\0\0"), Some("image/heif"));
        assert_eq!(mime(b"\0\0\0\x10ftypmif1\0\0\0\0avif"), Some("image/heif"), "brands past the box end are ignored");
        assert_eq!(mime(b"II+\0\x08\0\0\0"), Some("image/tiff"), "BigTIFF");
    }

    #[test]
    fn recognises_video_audio_and_documents() {
        assert_eq!(mime(b"\0\0\0\x20ftypisom\0\0\x02\0"), Some("video/mp4"));
        assert_eq!(mime(b"\0\0\0\x14ftypqt  \0\0\0\0"), Some("video/quicktime"));
        assert_eq!(mime(b"\0\0\0\x20ftypM4A \0\0\0\0"), Some("audio/mp4"));
        assert_eq!(mime(b"\x1A\x45\xDF\xA3\x9f\x42\x82\x84webm"), Some("video/webm"));
        assert_eq!(mime(b"\x1A\x45\xDF\xA3\x9f\x42\x82\x88matroska"), Some("video/x-matroska"));
        assert_eq!(mime(b"RIFF\x24\0\0\0WAVEfmt "), Some("audio/wav"));
        assert_eq!(mime(b"OggS\0\x02"), Some("audio/ogg"));
        assert_eq!(mime(b"fLaC\0\0\0\x22"), Some("audio/flac"));
        assert_eq!(mime(b"ID3\x04\0\0\0\0\x01\x7f"), Some("audio/mpeg"));
        assert_eq!(mime(b"\xFF\xFB\x90\x64"), Some("audio/mpeg"));
        assert_eq!(mime(b"\xFF\xF1\x50\x80"), Some("audio/aac"));
        assert_eq!(mime(b"%PDF-1.7\n"), Some("application/pdf"));
        assert_eq!(mime(b"glTF\x02\0\0\0"), Some("model/gltf-binary"));
    }

    #[test]
    fn recognises_obj_and_gltf_json_but_not_prose_or_other_json() {
        assert_eq!(mime(b"# Blender\nmtllib cube.mtl\no Cube\nv 1 1 -1\nv 1 -1 -1\nvn 0 1 0\nf 1//1 2//1 3//1\n"), Some("model/obj"));
        assert_eq!(mime(b"v 0 0 0\r\nv 1 0 0 0.5 0.5 0.5\r\n"), Some("model/obj"));
        assert_eq!(mime(b"v is a letter\n"), Some("text/plain"));
        assert_eq!(mime(b"# just a comment\n"), Some("text/plain"));
        assert_eq!(mime(b"o object\ng group\n"), Some("text/plain"), "OBJ needs a vertex");
        assert_eq!(mime(br#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0]}]}"#), Some("model/gltf+json"));
        assert_eq!(mime(br#"{"asset":"logo.png","version":"2.1"}"#), Some("text/plain"));
        assert_eq!(mime(br#"{"name":"cube"}"#), Some("text/plain"));
    }

    #[test]
    fn text_and_unknown_binary() {
        assert_eq!(mime(b"a cat on a skateboard\n"), Some("text/plain"));
        assert_eq!(mime("Grüße, 猫".as_bytes()), Some("text/plain"));
        assert_eq!(mime(b""), None);
        assert_eq!(mime(b"\0\x01\x02\x03binary"), None);
        assert_eq!(mime(&[0xC3]), None, "a lone lead byte is not text when nothing was cut off");
        // A multi-byte character cut by the sniff window is still text.
        let mut long = "a".repeat(SNIFF_WINDOW - 1).into_bytes();
        long.extend_from_slice("é and more".as_bytes());
        assert_eq!(mime(&long), Some("text/plain"));
    }

    #[test]
    fn prose_that_starts_like_a_signature_stays_text() {
        assert_eq!(mime(b"BMX bikes are great for tricks, really"), Some("text/plain"));
        assert_eq!(mime(b"ID3 tags are metadata"), Some("text/plain"));
        assert_eq!(mime(b"ID3\x04\0\0\0\0\x01"), None, "short ID3 header is not audio");
        assert_eq!(mime(b"\xFF\xFEH\0i\0"), Some("text/plain; charset=utf-16le"));
        assert_eq!(mime(b"\xFE\xFF\0H\0i"), Some("text/plain; charset=utf-16be"));
        assert_eq!(mime(b"\xFF\xFF\xFF\xFF"), None, "bitrate index 15 is invalid");
        assert_eq!(mime(b"\xFF\xFB\x0C\x64"), None, "sample-rate index 3 is invalid");
        assert_eq!(mime(b"\xFF\xFB\x90"), None, "a frame header is four bytes");
    }

    #[test]
    fn short_or_truncated_signatures_never_panic() {
        for len in 0..16 {
            for fill in [0x00, 0xFF, b'R', b'B'] {
                let _ = sniff(&vec![fill; len]);
            }
            let _ = sniff(b"\x89PNG\r\n\x1a\n".get(..len.min(8)).unwrap_or_default());
            let _ = sniff(b"RIFF\0\0\0\0WEB".get(..len.min(11)).unwrap_or_default());
        }
        assert_eq!(mime(b"BM"), Some("text/plain"), "a bare 'BM' is too short to be a bitmap");
    }
}

#[cfg(test)]
mod hdr394_recognition {
    use super::*;
    #[test]
    fn hdr394_signatures_precede_text_and_near_misses_stay_unchanged() {
        for (bytes, mime) in [
            (b"\x76\x2f\x31\x01".as_slice(), "image/x-exr"),
            (b"#?RGBE\n".as_slice(), "image/vnd.radiance"),
            (b"#?RADIANCE\n".as_slice(), "image/vnd.radiance"),
            (b"#?RGBE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\xff\x80\x40\x81".as_slice(), "image/vnd.radiance"),
        ] {
            assert_eq!(sniff(bytes), Some(Sniffed { mime, kind: MediaKind::Image }));
        }
        for text in [b"#?RGBE".as_slice(), b"#?RGBE prose\n", b"#?RADIANCEX\n", b" #?RGBE\n", b"#?RGBE\r\n"] {
            assert_eq!(sniff(text).unwrap().kind, MediaKind::Text);
        }
        assert_eq!(sniff(b"\x76\x2f\x31\x02"), None);
        assert_eq!(sniff(b"\x76\x2f\x31").unwrap().kind, MediaKind::Text);
        assert_eq!(sniff(b""), None);
    }
}
