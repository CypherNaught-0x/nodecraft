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
/// binary data NodeCraft doesn't know. Valid UTF-8 without control characters is `text/plain`.
pub fn sniff(bytes: &[u8]) -> Option<Sniffed> {
    use MediaKind::*;
    let head = bytes.get(..bytes.len().min(SNIFF_WINDOW)).unwrap_or(bytes);
    let at = |offset: usize, magic: &[u8]| offset.checked_add(magic.len()).and_then(|end| head.get(offset..end)).is_some_and(|s| s == magic);

    let found = if at(0, b"\x89PNG\r\n\x1a\n") {
        Sniffed::new("image/png", Image)
    } else if at(0, b"\xFF\xD8\xFF") {
        Sniffed::new("image/jpeg", Image)
    } else if at(0, b"GIF87a") || at(0, b"GIF89a") {
        Sniffed::new("image/gif", Image)
    } else if at(0, b"RIFF") && at(8, b"WEBP") {
        Sniffed::new("image/webp", Image)
    } else if at(0, b"RIFF") && at(8, b"WAVE") {
        Sniffed::new("audio/wav", Audio)
    } else if at(0, b"II*\0") || at(0, b"MM\0*") {
        Sniffed::new("image/tiff", Image)
    } else if at(0, b"BM") && head.len() >= 26 {
        Sniffed::new("image/bmp", Image)
    } else if at(4, b"ftyp") {
        sniff_iso_bmff(head)
    } else if at(0, b"\x1A\x45\xDF\xA3") {
        if contains(head, b"webm") { Sniffed::new("video/webm", Video) } else { Sniffed::new("video/x-matroska", Video) }
    } else if at(0, b"OggS") {
        Sniffed::new("audio/ogg", Audio)
    } else if at(0, b"fLaC") {
        Sniffed::new("audio/flac", Audio)
    } else if at(0, b"ID3") {
        Sniffed::new("audio/mpeg", Audio)
    } else if let Some(audio) = sniff_mpeg_frame(head) {
        audio
    } else if at(0, b"%PDF-") {
        Sniffed::new("application/pdf", Document)
    } else if at(0, b"glTF") {
        Sniffed::new("model/gltf-binary", Model3d)
    } else if looks_like_text(head, bytes.len() > head.len()) {
        Sniffed::new("text/plain", Text)
    } else {
        return None;
    };
    Some(found)
}

/// ISO base media files (MP4, MOV, AVIF, HEIC) share the `ftyp` box; its major brand tells them apart.
fn sniff_iso_bmff(head: &[u8]) -> Sniffed {
    use MediaKind::*;
    match head.get(8..12) {
        Some(b"avif" | b"avis") => Sniffed::new("image/avif", Image),
        Some(b"heic" | b"heix" | b"heim" | b"heis" | b"mif1" | b"msf1") => Sniffed::new("image/heic", Image),
        Some(b"qt  ") => Sniffed::new("video/quicktime", Video),
        Some(b"M4A " | b"M4B ") => Sniffed::new("audio/mp4", Audio),
        _ => Sniffed::new("video/mp4", Video),
    }
}

/// A bare MPEG audio frame header (MP3 without ID3 tags) or an ADTS AAC header.
fn sniff_mpeg_frame(head: &[u8]) -> Option<Sniffed> {
    let (&b0, &b1) = (head.first()?, head.get(1)?);
    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let layer = (b1 >> 1) & 0b11;
    let version = (b1 >> 3) & 0b11;
    if layer == 0 && b1 & 0xF0 == 0xF0 {
        Some(Sniffed::new("audio/aac", MediaKind::Audio))
    } else if layer != 0 && version != 0b01 {
        Some(Sniffed::new("audio/mpeg", MediaKind::Audio))
    } else {
        None
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
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
        assert_eq!(mime(&bmp), Some("image/bmp"));
        assert_eq!(mime(b"\0\0\0\x1cftypavif\0\0\0\0"), Some("image/avif"));
        assert_eq!(mime(b"\0\0\0\x18ftypheic\0\0\0\0"), Some("image/heic"));
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
        assert_eq!(mime(b"ID3\x04\0\0"), Some("audio/mpeg"));
        assert_eq!(mime(b"\xFF\xFB\x90\x64"), Some("audio/mpeg"));
        assert_eq!(mime(b"\xFF\xF1\x50\x80"), Some("audio/aac"));
        assert_eq!(mime(b"%PDF-1.7\n"), Some("application/pdf"));
        assert_eq!(mime(b"glTF\x02\0\0\0"), Some("model/gltf-binary"));
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
