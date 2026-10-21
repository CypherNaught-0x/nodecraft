//! What an image file says about its colour space, read by walking its chunk or marker headers
//! and seeking over their bodies (#393). The walks never read a profile itself, so a hostile size
//! field can't force an allocation, and the reader probe's byte budget is never at risk.
use super::ColorTag;
use image::ImageFormat;
use std::io::{Read, Seek, SeekFrom};

/// The walk reads at most this many bytes, well inside the reader probe's 512 KiB budget. A file
/// whose headers need more says nothing about its colour space.
const MAX_WALK_BYTES: usize = 16 * 1024;
/// At most this many chunks or markers are walked.
const MAX_WALK_STEPS: usize = 4096;

/// The colour signals a walk found.
#[derive(Debug, Default, Clone, Copy)]
struct Signals {
    /// A PNG `cICP` chunk: coding-independent code points, which outrank a profile.
    cicp: bool,
    icc: bool,
    srgb: bool,
    /// A PNG `gAMA` or `cHRM` chunk.
    gamma_or_chromaticities: bool,
    /// EXIF can name a colour space too and isn't read, so it makes the tag unknown.
    exif: bool,
}

impl Signals {
    fn tag(self) -> Option<ColorTag> {
        Some(if self.cicp {
            ColorTag::Other
        } else if self.icc {
            ColorTag::IccProfile
        } else if self.srgb {
            ColorTag::Srgb
        } else if self.gamma_or_chromaticities {
            ColorTag::Other
        } else if self.exif {
            return None;
        } else {
            ColorTag::Untagged
        })
    }
}

/// The colour tag of an image whose format is `format`. Only PNG, JPEG and WebP, whose every
/// common colour signal this build reads, get one; BMP (v4/v5 colour spaces), GIF and TIFF are
/// unknown. A walk that fails or hits a bound is unknown too. Leaves `input` anywhere.
pub(super) fn color_tag(format: ImageFormat, input: &mut (impl Read + Seek)) -> Option<ColorTag> {
    let mut walk = Walk { input, read: 0 };
    match format {
        ImageFormat::Png => png(&mut walk),
        ImageFormat::Jpeg => jpeg(&mut walk),
        ImageFormat::WebP => webp(&mut walk),
        _ => None,
    }?
    .tag()
}

struct Walk<'a, R> {
    input: &'a mut R,
    read: usize,
}

impl<R: Read + Seek> Walk<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> Option<()> {
        self.read = self.read.checked_add(out.len()).filter(|n| *n <= MAX_WALK_BYTES)?;
        self.input.read_exact(out).ok()
    }

    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let mut out = [0u8; N];
        self.read(&mut out)?;
        Some(out)
    }

    /// Reads up to `N` bytes of a body `len` long, returning how many it read.
    fn prefix<const N: usize>(&mut self, len: u64) -> Option<([u8; N], u64)> {
        let mut out = [0u8; N];
        let n = len.min(N as u64);
        self.read(out.get_mut(..usize::try_from(n).ok()?)?)?;
        Some((out, n))
    }

    fn skip(&mut self, n: u64) -> Option<()> {
        self.input.seek(SeekFrom::Current(i64::try_from(n).ok()?)).ok().map(drop)
    }

    fn seek(&mut self, to: SeekFrom) -> Option<u64> {
        self.input.seek(to).ok()
    }
}

/// Walks every chunk up to `IEND`, including any after the image data (an `eXIf` may follow it).
/// Text chunks are read only as far as their keyword: ImageMagick stores profiles as
/// `Raw profile type icc` (or `icm`, `exif`) text.
fn png(walk: &mut Walk<'_, impl Read + Seek>) -> Option<Signals> {
    walk.seek(SeekFrom::Start(8))?;
    let mut found = Signals::default();
    for _ in 0..MAX_WALK_STEPS {
        let [l0, l1, l2, l3, t0, t1, t2, t3] = walk.array::<8>()?;
        let len = u64::from(u32::from_be_bytes([l0, l1, l2, l3]));
        let mut seen = 0;
        match &[t0, t1, t2, t3] {
            b"IEND" => return Some(found),
            b"iCCP" => found.icc = true,
            b"sRGB" => found.srgb = true,
            b"cICP" => found.cicp = true,
            b"gAMA" | b"cHRM" => found.gamma_or_chromaticities = true,
            b"eXIf" => found.exif = true,
            b"tEXt" | b"zTXt" | b"iTXt" => {
                let (keyword, n) = walk.prefix::<22>(len)?;
                seen = n;
                let keyword = keyword.get(..usize::try_from(n).ok()?)?;
                if keyword.starts_with(b"Raw profile type icc\0") || keyword.starts_with(b"Raw profile type icm\0") {
                    found.icc = true;
                } else if keyword.starts_with(b"Raw profile type exif\0") || keyword.starts_with(b"Raw profile type APP1\0") {
                    found.exif = true;
                }
            }
            _ => {}
        }
        // The rest of the body and its CRC.
        walk.skip(len.checked_sub(seen)?.checked_add(4)?)?;
    }
    None
}

/// Walks the markers up to the first scan. An ICC profile is an `APP2` segment starting
/// `ICC_PROFILE\0`; EXIF is an `APP1` segment starting `Exif\0\0`. A profile split across damaged
/// segments still counts as a profile.
fn jpeg(walk: &mut Walk<'_, impl Read + Seek>) -> Option<Signals> {
    walk.seek(SeekFrom::Start(0))?;
    if walk.array::<2>()? != [0xff, 0xd8] {
        return None;
    }
    let mut found = Signals::default();
    for _ in 0..MAX_WALK_STEPS {
        let [0xff, mut code] = walk.array::<2>()? else { return None };
        while code == 0xff {
            [code] = walk.array::<1>()?;
        }
        match code {
            0xda => return Some(found),
            0x01 | 0xd0..=0xd7 => continue,
            0x00 | 0xd8 | 0xd9 => return None,
            _ => {}
        }
        let len = u64::from(u16::from_be_bytes(walk.array::<2>()?)).checked_sub(2)?;
        let (body, n) = walk.prefix::<12>(len)?;
        let body = body.get(..usize::try_from(n).ok()?)?;
        match code {
            0xe2 if body.starts_with(b"ICC_PROFILE\0") => found.icc = true,
            0xe1 if body.starts_with(b"Exif\0\0") => found.exif = true,
            _ => {}
        }
        walk.skip(len.checked_sub(n)?)?;
    }
    None
}

/// Walks the RIFF chunks. A `VP8X` header's flags announce a profile (0x20) or EXIF (0x08) as well
/// as the chunks themselves; either counts.
fn webp(walk: &mut Walk<'_, impl Read + Seek>) -> Option<Signals> {
    let length = walk.seek(SeekFrom::End(0))?;
    walk.seek(SeekFrom::Start(0))?;
    let [r0, r1, r2, r3, s0, s1, s2, s3, w0, w1, w2, w3] = walk.array::<12>()?;
    if [r0, r1, r2, r3] != *b"RIFF" || [w0, w1, w2, w3] != *b"WEBP" {
        return None;
    }
    let end = u64::from(u32::from_le_bytes([s0, s1, s2, s3])).checked_add(8)?.min(length);
    let mut at = 12u64;
    let mut found = Signals::default();
    for _ in 0..MAX_WALK_STEPS {
        if at.checked_add(8)? > end {
            return Some(found);
        }
        let [t0, t1, t2, t3, l0, l1, l2, l3] = walk.array::<8>()?;
        let len = u64::from(u32::from_le_bytes([l0, l1, l2, l3]));
        let mut seen = 0;
        match &[t0, t1, t2, t3] {
            b"ICCP" => found.icc = true,
            b"EXIF" => found.exif = true,
            b"VP8X" => {
                let (flags, n) = walk.prefix::<1>(len)?;
                seen = n;
                let [flags] = flags;
                if n == 1 {
                    found.icc |= flags & 0x20 != 0;
                    found.exif |= flags & 0x08 != 0;
                }
            }
            _ => {}
        }
        // Bodies are padded to an even length.
        let padded = len.checked_add(len % 2)?;
        at = at.checked_add(8)?.checked_add(padded)?;
        if at > end {
            return Some(found);
        }
        walk.skip(padded.checked_sub(seen)?)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use image::codecs::webp::WebPEncoder;
    use std::io::Cursor;

    fn riff(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (kind, data) in chunks {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() % 2 == 1 {
                body.push(0);
            }
        }
        [&b"RIFF"[..], &(body.len() as u32).to_le_bytes(), &body].concat()
    }

    fn tag(bytes: &[u8]) -> Option<ColorTag> {
        color_tag(ImageFormat::WebP, &mut Cursor::new(bytes))
    }

    #[test]
    fn webp_chunks_and_vp8x_flags_decide_the_tag() {
        let vp8x = |flags: u8| [flags, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(tag(&riff(&[(b"VP8L", b"x")])), Some(ColorTag::Untagged));
        assert_eq!(tag(&riff(&[(b"VP8X", &vp8x(0)), (b"ICCP", b"profile"), (b"VP8L", b"x")])), Some(ColorTag::IccProfile));
        // The flags announce a profile or EXIF even when the chunk itself is missing.
        assert_eq!(tag(&riff(&[(b"VP8X", &vp8x(0x20)), (b"VP8L", b"x")])), Some(ColorTag::IccProfile));
        assert_eq!(tag(&riff(&[(b"VP8X", &vp8x(0x08)), (b"VP8L", b"x")])), None);
        // EXIF after the image data still counts.
        assert_eq!(tag(&riff(&[(b"VP8X", &vp8x(0)), (b"VP8L", b"x"), (b"EXIF", b"MM\0*")])), None);
        assert_eq!(tag(b"RIFF\0\0\0\0WEBQ"), None);
    }

    /// A hostile size field is seeked over, never allocated, and a walk needing more than its
    /// byte budget says nothing.
    #[test]
    fn hostile_sizes_and_long_walks_stay_bounded() {
        let mut huge = riff(&[(b"VP8L", b"x"), (b"ICCP", b"ab")]);
        let at = huge.len() - 6;
        huge[at..at + 4].copy_from_slice(&0xffff_fff0u32.to_le_bytes());
        assert_eq!(tag(&huge), Some(ColorTag::IccProfile));
        let many: Vec<(&[u8; 4], &[u8])> = vec![(b"JUNK", b""); MAX_WALK_STEPS + 1];
        assert_eq!(tag(&riff(&many)), None);
        // A JPEG whose markers say nothing for longer than the budget; four are fine.
        let jpeg = |segments: usize| {
            let mut jpeg = vec![0xff, 0xd8];
            for _ in 0..segments {
                jpeg.extend_from_slice(&[0xff, 0xe3, 0, 14]);
                jpeg.extend_from_slice(&[0; 12]);
            }
            jpeg.extend_from_slice(&[0xff, 0xda, 0, 2]);
            color_tag(ImageFormat::Jpeg, &mut Cursor::new(jpeg))
        };
        assert_eq!(jpeg(4), Some(ColorTag::Untagged));
        assert_eq!(jpeg(MAX_WALK_BYTES / 16 + 1), None);
    }

    /// A real WebP with a profile far larger than the reader probe's budget is tagged alike by both
    /// probes, without reading the profile.
    #[test]
    fn a_large_webp_profile_is_tagged_by_both_probes() {
        let mut out = Vec::new();
        let mut encoder = WebPEncoder::new_lossless(&mut out);
        encoder.set_icc_profile(vec![7; 600 * 1024]).unwrap();
        encoder.write_image(&[9; 4 * 3 * 3], 4, 3, image::ExtendedColorType::Rgb8).unwrap();
        let limits = crate::image_io::Limits::default();
        let meta = crate::image_io::probe_image(&out, &limits).unwrap();
        assert_eq!(meta.color_tag, Some(ColorTag::IccProfile));
        let len = out.len() as u64;
        assert_eq!(crate::image_io::probe_image_reader(Cursor::new(out), len, &limits).unwrap(), meta);
    }
}
