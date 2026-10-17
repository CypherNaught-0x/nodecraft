//! Image metadata and decoding behind size caps.

use std::io::Cursor;

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};

use crate::{MediaError, MediaKind, sniff};

/// Caps on what [`probe_image`] and [`decode_image`] accept. The defaults suit pasted and
/// generated images; lower them for untrusted bulk imports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Largest encoded input, in bytes.
    pub max_input_bytes: u64,
    /// Widest image, in pixels.
    pub max_width: u32,
    /// Tallest image, in pixels.
    pub max_height: u32,
    /// Most memory the decoder may allocate at once, decoded pixels included.
    pub max_alloc: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_input_bytes: 256 * 1024 * 1024, max_width: 16_384, max_height: 16_384, max_alloc: 1024 * 1024 * 1024 }
    }
}

impl Limits {
    fn for_decoder(&self) -> image::Limits {
        let mut l = image::Limits::default();
        l.max_image_width = Some(self.max_width);
        l.max_image_height = Some(self.max_height);
        l.max_alloc = Some(self.max_alloc);
        l
    }
}

/// Encodes 8-bit RGBA pixels (`width × height × 4` bytes, row-major) as PNG. The pixel buffer
/// must match the size exactly, and the image is held to `limits`' width and height.
pub fn encode_png(width: u32, height: u32, rgba: &[u8], limits: &Limits) -> Result<Vec<u8>, MediaError> {
    if width == 0 || height == 0 || width > limits.max_width || height > limits.max_height {
        return Err(MediaError::LimitExceeded(format!("{width}×{height} is outside {}×{}", limits.max_width, limits.max_height)));
    }
    let expected = u64::from(width).checked_mul(u64::from(height)).and_then(|n| n.checked_mul(4));
    if expected != Some(rgba.len() as u64) {
        return Err(MediaError::InvalidParameter("the pixel buffer doesn't match the image size"));
    }
    let buffer =
        image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or(MediaError::InvalidParameter("the pixel buffer doesn't match the image size"))?;
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(buffer).write_to(&mut out, ImageFormat::Png).map_err(|e| MediaError::Decode(format!("can't encode PNG: {e}")))?;
    Ok(out.into_inner())
}

/// The channel layout of an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorModel {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
}

/// What an image is, read from its header without decoding the pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageMeta {
    pub mime: &'static str,
    /// Width as displayed, after the file's EXIF orientation is applied.
    pub width: u32,
    /// Height as displayed, after the file's EXIF orientation is applied.
    pub height: u32,
    /// The colour layout stored in the file.
    pub color: ColorModel,
    /// Bits per channel stored in the file (1, 2, 4, 8, 16 or 32).
    pub bits_per_channel: u8,
    pub has_alpha: bool,
}

/// Reads an image's format, size and colour layout from its header, without decoding the pixels.
///
/// Fails with [`MediaError::LimitExceeded`] for an image [`decode_image`] would refuse.
pub fn probe_image(bytes: &[u8], limits: &Limits) -> Result<ImageMeta, MediaError> {
    let (meta, _, _) = open(bytes, limits)?;
    Ok(meta)
}

/// Decodes an image (the first frame of an animation) within `limits`, upright: the file's EXIF
/// orientation is applied.
pub fn decode_image(bytes: &[u8], limits: &Limits) -> Result<(ImageMeta, DynamicImage), MediaError> {
    let (meta, orientation, decoder) = open(bytes, limits)?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok((meta, image))
}

/// Sniffs, checks the caps and reads the header. The decoders only check width and height
/// themselves (and most ignore `max_alloc`), so the decoded size, plus the copy a rotation makes,
/// is reserved against `max_alloc` here before any pixels are allocated.
fn open<'a>(bytes: &'a [u8], limits: &Limits) -> Result<(ImageMeta, Orientation, impl ImageDecoder + 'a), MediaError> {
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > limits.max_input_bytes {
        return Err(MediaError::TooLarge { size, limit: limits.max_input_bytes });
    }
    let sniffed = sniff(bytes).ok_or(MediaError::Unrecognized)?;
    let format = decodable_format(sniffed.mime).filter(|_| sniffed.kind == MediaKind::Image).ok_or(MediaError::Unsupported { mime: sniffed.mime })?;
    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.set_format(format);
    reader.limits(limits.for_decoder());
    let mut decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 {
        return Err(MediaError::Decode(format!("image has no pixels ({w}×{h})")));
    }
    // A damaged EXIF block is not worth refusing the image for; show it as stored.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut budget = limits.for_decoder();
    budget.reserve(decoder.total_bytes())?;
    if orientation != Orientation::NoTransforms {
        budget.reserve(decoder.total_bytes())?;
    }
    let mut meta = meta_of(sniffed.mime, &decoder);
    if matches!(orientation, Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH) {
        std::mem::swap(&mut meta.width, &mut meta.height);
    }
    Ok((meta, orientation, decoder))
}

/// The formats this build decodes. The sniffed MIME type picks the decoder, never a file name.
fn decodable_format(mime: &str) -> Option<ImageFormat> {
    Some(match mime {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        "image/bmp" => ImageFormat::Bmp,
        "image/tiff" => ImageFormat::Tiff,
        _ => return None,
    })
}

fn meta_of(mime: &'static str, decoder: &impl ImageDecoder) -> ImageMeta {
    let (width, height) = decoder.dimensions();
    let stored = decoder.original_color_type();
    let decoded = decoder.color_type();
    let channels = decoded.channel_count();
    let color = match (channels, decoded.has_alpha()) {
        (1, _) => ColorModel::Gray,
        (2, _) => ColorModel::GrayAlpha,
        (_, false) => ColorModel::Rgb,
        (_, true) => ColorModel::Rgba,
    };
    let bits_per_channel = match stored {
        image::ExtendedColorType::L1 | image::ExtendedColorType::La1 => 1,
        image::ExtendedColorType::L2 | image::ExtendedColorType::La2 => 2,
        image::ExtendedColorType::L4 | image::ExtendedColorType::La4 => 4,
        other => {
            let bits = other.bits_per_pixel();
            let per = bits.checked_div(u16::from(other.channel_count())).unwrap_or(8);
            u8::try_from(per).unwrap_or(8)
        }
    };
    ImageMeta { mime, width, height, color, bits_per_channel, has_alpha: decoded.has_alpha() }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::{ImageBuffer, Luma, Rgb, Rgba};

    pub(crate) fn png_rgba(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| Rgba([x as u8, y as u8, 128, 200]));
        encode(DynamicImage::ImageRgba8(img), ImageFormat::Png)
    }

    fn encode(img: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn encodes_png_round_trip_and_checks_sizes() {
        let rgba: Vec<u8> = (0..4 * 3 * 4).map(|i| i as u8).collect();
        let png = encode_png(4, 3, &rgba, &Limits::default()).unwrap();
        let (meta, img) = decode_image(&png, &Limits::default()).unwrap();
        assert_eq!((meta.width, meta.height), (4, 3));
        assert_eq!(img.to_rgba8().into_raw(), rgba);
        assert!(encode_png(4, 3, &rgba[1..], &Limits::default()).is_err(), "wrong buffer size");
        assert!(encode_png(0, 3, &[], &Limits::default()).is_err());
        assert!(encode_png(u32::MAX, u32::MAX, &[], &Limits::default()).is_err(), "no overflow, no allocation");
    }

    #[test]
    fn probes_png_without_decoding() {
        let meta = probe_image(&png_rgba(40, 30), &Limits::default()).unwrap();
        assert_eq!(meta.mime, "image/png");
        assert_eq!((meta.width, meta.height), (40, 30));
        assert_eq!(meta.color, ColorModel::Rgba);
        assert_eq!(meta.bits_per_channel, 8);
        assert!(meta.has_alpha);
    }

    #[test]
    fn decodes_each_supported_format() {
        let rgb = DynamicImage::ImageRgb8(ImageBuffer::from_fn(8, 6, |x, _| Rgb([x as u8 * 30, 0, 0])));
        for (format, mime) in [
            (ImageFormat::Png, "image/png"),
            (ImageFormat::Jpeg, "image/jpeg"),
            (ImageFormat::Gif, "image/gif"),
            (ImageFormat::WebP, "image/webp"),
            (ImageFormat::Bmp, "image/bmp"),
            (ImageFormat::Tiff, "image/tiff"),
        ] {
            let source = if format == ImageFormat::Gif { DynamicImage::ImageRgba8(rgb.to_rgba8()) } else { rgb.clone() };
            let bytes = encode(source, format);
            let (meta, img) = decode_image(&bytes, &Limits::default()).unwrap();
            assert_eq!(meta.mime, mime);
            assert_eq!((img.width(), img.height()), (8, 6), "{mime}");
        }
    }

    #[test]
    fn gray_16_bit_metadata() {
        let img = ImageBuffer::from_fn(4, 4, |x, _| Luma([x as u16 * 1000]));
        let meta = probe_image(&encode(DynamicImage::ImageLuma16(img), ImageFormat::Png), &Limits::default()).unwrap();
        assert_eq!(meta.color, ColorModel::Gray);
        assert_eq!(meta.bits_per_channel, 16);
        assert!(!meta.has_alpha);
    }

    #[test]
    fn input_size_cap() {
        let bytes = png_rgba(4, 4);
        let limits = Limits { max_input_bytes: 10, ..Limits::default() };
        assert!(matches!(decode_image(&bytes, &limits), Err(MediaError::TooLarge { limit: 10, .. })));
    }

    #[test]
    fn dimension_caps() {
        let bytes = png_rgba(100, 20);
        let narrow = Limits { max_width: 50, ..Limits::default() };
        assert!(matches!(probe_image(&bytes, &narrow), Err(MediaError::LimitExceeded(_))));
        let short = Limits { max_height: 10, ..Limits::default() };
        assert!(matches!(decode_image(&bytes, &short), Err(MediaError::LimitExceeded(_))));
    }

    #[test]
    fn huge_header_is_refused_before_allocating() {
        // A PNG whose header claims 100000×100000 pixels but carries no data: must be refused by
        // the limits, not by trying to allocate 40 GB.
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend_from_slice(&100_000u32.to_be_bytes());
        ihdr.extend_from_slice(&100_000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(&ihdr);
        bytes.extend_from_slice(&crc32(&ihdr).to_be_bytes());
        assert!(matches!(decode_image(&bytes, &Limits::default()), Err(MediaError::LimitExceeded(_))));
        let roomy = Limits { max_width: u32::MAX, max_height: u32::MAX, ..Limits::default() };
        assert!(decode_image(&bytes, &roomy).is_err());
    }

    #[test]
    fn allocation_cap_applies_to_every_format() {
        let rgba = DynamicImage::ImageRgba8(ImageBuffer::from_fn(1024, 1024, |x, y| Rgba([x as u8, y as u8, 3, 255])));
        let tight = Limits { max_alloc: 1024 * 1024, ..Limits::default() };
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::WebP, ImageFormat::Bmp, ImageFormat::Tiff] {
            let source = if format == ImageFormat::Jpeg { DynamicImage::ImageRgb8(rgba.to_rgb8()) } else { rgba.clone() };
            let bytes = encode(source, format);
            assert!(matches!(decode_image(&bytes, &tight), Err(MediaError::LimitExceeded(_))), "{format:?}");
            assert!(matches!(probe_image(&bytes, &tight), Err(MediaError::LimitExceeded(_))), "{format:?}");
            assert!(decode_image(&bytes, &Limits::default()).is_ok(), "{format:?}");
        }
    }

    #[test]
    fn bmp_header_claiming_a_huge_image_is_refused_before_allocating() {
        // A 54-byte BMP header for 16000×16000 at 32 bpp (1 GB of pixels) with no pixel data.
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&54u32.to_le_bytes()); // file size (a lie)
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset
        bmp.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        bmp.extend_from_slice(&16_000i32.to_le_bytes());
        bmp.extend_from_slice(&16_000i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        let limits = Limits { max_alloc: 64 * 1024 * 1024, ..Limits::default() };
        assert!(matches!(decode_image(&bmp, &limits), Err(MediaError::LimitExceeded(_))));
    }

    #[test]
    fn exif_orientation_is_applied() {
        // A 4×2 JPEG tagged "rotate 90° clockwise" (EXIF orientation 6) decodes as 2×4.
        let img = DynamicImage::ImageRgb8(ImageBuffer::from_fn(4, 2, |x, _| Rgb([x as u8 * 60, 0, 0])));
        let plain = encode(img, ImageFormat::Jpeg);
        let mut exif = b"Exif\0\0MM\0*\0\0\0\x08\0\x01".to_vec();
        exif.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]);
        let len = u16::try_from(exif.len() + 2).unwrap();
        let mut tagged = plain[..2].to_vec();
        tagged.extend_from_slice(&[0xFF, 0xE1]);
        tagged.extend_from_slice(&len.to_be_bytes());
        tagged.extend_from_slice(&exif);
        tagged.extend_from_slice(&plain[2..]);
        let meta = probe_image(&tagged, &Limits::default()).unwrap();
        assert_eq!((meta.width, meta.height), (2, 4));
        let (_, decoded) = decode_image(&tagged, &Limits::default()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 4));
    }

    #[test]
    fn non_images_and_garbage_are_errors() {
        let l = Limits::default();
        assert_eq!(decode_image(b"", &l).unwrap_err(), MediaError::Unrecognized);
        assert_eq!(decode_image(b"%PDF-1.7", &l).unwrap_err(), MediaError::Unsupported { mime: "application/pdf" });
        assert_eq!(probe_image(b"\0\0\0\x1cftypavif\0", &l).unwrap_err(), MediaError::Unsupported { mime: "image/avif" });
        assert_eq!(decode_image(b"hello", &l).unwrap_err(), MediaError::Unsupported { mime: "text/plain" });
        assert!(matches!(decode_image(b"\x89PNG\r\n\x1a\nnot really", &l), Err(MediaError::Decode(_))));
    }

    #[test]
    fn truncated_and_mutated_images_never_panic() {
        let limits = Limits::default();
        let rgb = DynamicImage::ImageRgb8(ImageBuffer::from_fn(16, 16, |x, y| Rgb([x as u8, y as u8, 7])));
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::WebP, ImageFormat::Bmp, ImageFormat::Tiff] {
            let source = if format == ImageFormat::Gif { DynamicImage::ImageRgba8(rgb.to_rgba8()) } else { rgb.clone() };
            let good = encode(source, format);
            for cut in 0..good.len() {
                let _ = decode_image(&good[..cut], &limits);
            }
            for _ in 0..300 {
                let mut bad = good.clone();
                for _ in 0..1 + next() % 8 {
                    let i = (next() % bad.len() as u64) as usize;
                    bad[i] = next() as u8;
                }
                let _ = decode_image(&bad, &limits);
            }
        }
    }

    /// CRC-32 (IEEE) for building PNG chunks in tests.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
}
