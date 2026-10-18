//! Header-only reader probes. Complete content integrity and pixel decoding are separate.
use super::{ImageMeta, Limits, decodable_format, meta_of};
use crate::{MediaError, MediaKind, sniff};
use image::{ImageDecoder, ImageFormat, ImageReader, metadata::Orientation};
use std::{
    cell::Cell,
    io::{self, BufReader, Read, Seek, SeekFrom},
    rc::Rc,
};

pub const MAX_IMAGE_PROBE_BYTES: u64 = 512 * 1024;
#[derive(Clone, Copy, Default)]
struct State {
    read: u64,
    budget_failed: bool,
    io_failed: bool,
}
struct Bounded<R> {
    inner: R,
    length: u64,
    position: u64,
    state: Rc<Cell<State>>,
}
impl<R: Read> Read for Bounded<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.position > self.length {
            let mut state = self.state.get();
            state.io_failed = true;
            self.state.set(state);
            return Err(io::Error::other("image probe position exceeds input"));
        }
        if out.is_empty() || self.position == self.length {
            return Ok(0);
        }
        let mut state = self.state.get();
        let remaining = MAX_IMAGE_PROBE_BYTES.saturating_sub(state.read);
        if remaining == 0 {
            state.budget_failed = true;
            self.state.set(state);
            return Err(io::Error::other("image header probe budget exceeded"));
        }
        let available = self.length.checked_sub(self.position).ok_or_else(|| io::Error::other("image probe position exceeds input"))?;
        let cap = available.min(remaining).min(64 * 1024).min(out.len() as u64);
        let cap = usize::try_from(cap).map_err(|_| io::Error::other("image probe extent overflow"))?;
        let destination = out.get_mut(..cap).ok_or_else(|| io::Error::other("image probe extent mismatch"))?;
        let n = match self.inner.read(destination) {
            Ok(n) if n > 0 && n <= cap => n,
            _ => {
                state.io_failed = true;
                self.state.set(state);
                return Err(io::Error::other("cannot read complete image header"));
            }
        };
        state.read = state.read.checked_add(n as u64).ok_or_else(|| io::Error::other("image probe count overflow"))?;
        self.position = self.position.checked_add(n as u64).ok_or_else(|| io::Error::other("image probe position overflow"))?;
        self.state.set(state);
        Ok(n)
    }
}
impl<R: Seek> Seek for Bounded<R> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => self.position.checked_add_signed(n),
            SeekFrom::End(n) => self.length.checked_add_signed(n),
        };
        let target = target.filter(|n| *n <= self.length).ok_or_else(|| io::Error::other("image header seek exceeds input"))?;
        match self.inner.seek(SeekFrom::Start(target)) {
            Ok(n) if n == target => {
                self.position = n;
                Ok(n)
            }
            _ => {
                let mut state = self.state.get();
                state.io_failed = true;
                self.state.set(state);
                Err(io::Error::other("cannot seek image header"))
            }
        }
    }
}
fn checked(state: &Cell<State>) -> Result<(), MediaError> {
    if state.get().budget_failed {
        Err(budget_error())
    } else if state.get().io_failed {
        Err(MediaError::Decode("cannot read complete image header".into()))
    } else {
        Ok(())
    }
}
fn budget_error() -> MediaError {
    MediaError::LimitExceeded("image header exceeds the 512 KiB probe budget".into())
}

/// Reads supported image header metadata from a bounded seekable source, never pixels.
/// All source reads, including rereads after seeks and optional orientation work, share
/// a 512 KiB cumulative budget. Large/complex headers may be refused even when a full
/// decoder could accept them. Total encoded size and decoded-size limits remain explicit.
/// A successful result does not prove complete image/pixel validity.
pub fn probe_image_reader<R: Read + Seek>(reader: R, length: u64, limits: &Limits) -> Result<ImageMeta, MediaError> {
    if length > limits.max_input_bytes {
        return Err(MediaError::TooLarge { size: length, limit: limits.max_input_bytes });
    }
    let state = Rc::new(Cell::new(State::default()));
    let mut input = Bounded { inner: reader, length, position: 0, state: state.clone() };
    input.seek(SeekFrom::Start(0)).map_err(|_| MediaError::Decode("cannot seek image header".into()))?;
    let mut prefix = [0u8; 64];
    let n = usize::try_from(length.min(prefix.len() as u64)).map_err(|_| budget_error())?;
    input.read_exact(prefix.get_mut(..n).ok_or_else(budget_error)?).map_err(|_| MediaError::Decode("cannot read image signature".into()))?;
    let sniffed = sniff(prefix.get(..n).ok_or_else(budget_error)?).ok_or(MediaError::Unrecognized)?;
    let format = decodable_format(sniffed.mime).filter(|_| sniffed.kind == MediaKind::Image).ok_or(MediaError::Unsupported { mime: sniffed.mime })?;
    input.seek(SeekFrom::Start(0)).map_err(|_| MediaError::Decode("cannot seek image header".into()))?;
    if format == ImageFormat::Jpeg {
        // image0.25's JPEG constructor buffers its entire reader. Supply only the
        // bounded complete marker headers through SOS, which decode_headers accepts.
        let headers = jpeg_headers(&mut input).map_err(|error| checked(&state).err().unwrap_or(error))?;
        let result = super::probe_image(&headers, limits);
        checked(&state)?;
        return result;
    }
    let mut reader = ImageReader::new(BufReader::with_capacity(8192, input));
    reader.set_format(format);
    let mut header_limits = limits.for_decoder();
    header_limits.max_alloc = Some(limits.max_alloc.min(MAX_IMAGE_PROBE_BYTES));
    reader.limits(header_limits);
    let result = (|| {
        let mut decoder = reader.into_decoder()?;
        let (width, height) = decoder.dimensions();
        if width == 0 || height == 0 {
            return Err(MediaError::Decode("image header has no pixels".into()));
        }
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let mut decoded_budget = limits.for_decoder();
        decoded_budget.reserve(decoder.total_bytes())?;
        if orientation != Orientation::NoTransforms {
            decoded_budget.reserve(decoder.total_bytes())?;
        }
        let mut meta = meta_of(sniffed.mime, &decoder);
        if matches!(orientation, Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH) {
            std::mem::swap(&mut meta.width, &mut meta.height);
        }
        Ok(meta)
    })();
    checked(&state)?;
    result
}
fn append_read(reader: &mut impl Read, bytes: &mut Vec<u8>, count: usize) -> Result<(), MediaError> {
    let next = bytes.len().checked_add(count).filter(|n| *n as u64 <= MAX_IMAGE_PROBE_BYTES).ok_or_else(budget_error)?;
    bytes.try_reserve_exact(count).map_err(|_| MediaError::LimitExceeded("cannot allocate bounded image header".into()))?;
    let at = bytes.len();
    bytes.resize(next, 0);
    reader.read_exact(bytes.get_mut(at..next).ok_or_else(budget_error)?).map_err(|_| MediaError::Decode("truncated JPEG marker header".into()))
}
fn jpeg_headers(reader: &mut impl Read) -> Result<Vec<u8>, MediaError> {
    let mut bytes = Vec::new();
    append_read(reader, &mut bytes, 2)?;
    if bytes.as_slice() != b"\xff\xd8" {
        return Err(MediaError::Decode("invalid JPEG start marker".into()));
    }
    loop {
        let mut marker = [0u8; 2];
        reader.read_exact(&mut marker).map_err(|_| MediaError::Decode("missing JPEG scan header".into()))?;
        if marker.first() != Some(&0xff) {
            return Err(MediaError::Decode("invalid JPEG marker framing".into()));
        }
        let mut code = marker.get(1).copied().ok_or_else(budget_error)?;
        // Fill bytes are legal before markers; preserve them under the same bound.
        while code == 0xff {
            if bytes.len() as u64 >= MAX_IMAGE_PROBE_BYTES {
                return Err(budget_error());
            }
            bytes.try_reserve_exact(1).map_err(|_| budget_error())?;
            bytes.push(0xff);
            let mut byte = [0];
            reader.read_exact(&mut byte).map_err(|_| MediaError::Decode("truncated JPEG marker".into()))?;
            code = byte.first().copied().ok_or_else(budget_error)?;
        }
        if code == 0 || (0xd0..=0xd9).contains(&code) {
            return Err(MediaError::Decode("unexpected JPEG marker before scan".into()));
        }
        if bytes.len().checked_add(2).is_none_or(|n| n as u64 > MAX_IMAGE_PROBE_BYTES) {
            return Err(budget_error());
        }
        bytes.try_reserve_exact(2).map_err(|_| budget_error())?;
        bytes.extend_from_slice(&[0xff, code]);
        if code == 1 {
            continue;
        }
        let mut length = [0u8; 2];
        reader.read_exact(&mut length).map_err(|_| MediaError::Decode("truncated JPEG segment length".into()))?;
        let length = usize::from(u16::from_be_bytes(length));
        if length < 2 {
            return Err(MediaError::Decode("invalid JPEG segment length".into()));
        }
        let next = bytes.len().checked_add(length).filter(|n| *n as u64 <= MAX_IMAGE_PROBE_BYTES).ok_or_else(budget_error)?;
        bytes.try_reserve_exact(length).map_err(|_| budget_error())?;
        bytes.extend_from_slice(&(length as u16).to_be_bytes());
        append_read(reader, &mut bytes, length - 2)?;
        if bytes.len() != next {
            return Err(MediaError::Decode("JPEG header extent mismatch".into()));
        }
        if code == 0xda {
            return Ok(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;
    struct Counted {
        input: Cursor<Vec<u8>>,
        reads: Rc<Cell<u64>>,
    }
    impl Read for Counted {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = self.input.read(out)?;
            self.reads.set(self.reads.get() + n as u64);
            Ok(n)
        }
    }
    impl Seek for Counted {
        fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
            self.input.seek(from)
        }
    }
    fn source(bytes: Vec<u8>) -> (Counted, Rc<Cell<u64>>, u64) {
        let len = bytes.len() as u64;
        let reads = Rc::new(Cell::new(0));
        (Counted { input: Cursor::new(bytes), reads: reads.clone() }, reads, len)
    }
    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }
    #[test]
    fn bounded_reader_rejects_invalid_position_without_reading_and_preserves_failed_seek() {
        let (inner, reads, _) = source(vec![1, 2, 3]);
        let state = Rc::new(Cell::new(State::default()));
        let mut reader = Bounded { inner, length: 3, position: 4, state: state.clone() };
        assert!(reader.read(&mut [0]).is_err());
        assert!(reader.read(&mut []).is_err());
        assert!(state.get().io_failed);
        assert_eq!(reads.get(), 0);
        reader.position = 3;
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        assert!(reader.seek(SeekFrom::Start(4)).is_err());
        assert_eq!(reader.position, 3);
        assert_eq!(reads.get(), 0);
    }
    #[test]
    fn reader_headers_match_existing_metadata_for_every_supported_image() {
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(17, 9, |x, y| Rgb([x as u8, y as u8, 200])));
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::WebP, ImageFormat::Bmp, ImageFormat::Tiff] {
            let image = if format == ImageFormat::Gif { DynamicImage::ImageRgba8(image.to_rgba8()) } else { image.clone() };
            let bytes = encode(image, format);
            let expected = super::super::probe_image(&bytes, &Limits::default()).unwrap();
            let (input, reads, len) = source(bytes);
            let actual = probe_image_reader(input, len, &Limits::default()).unwrap();
            assert_eq!(actual, expected, "{format:?}");
            assert!(reads.get() <= MAX_IMAGE_PROBE_BYTES);
        }
    }
    #[test]
    fn large_real_jpeg_uses_only_headers_and_preserves_exif_orientation() {
        let mut seed = 1u32;
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(1024, 1024, |_, _| {
            let mut value = [0; 3];
            for channel in &mut value {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *channel = (seed >> 24) as u8;
            }
            Rgb(value)
        }));
        let plain = encode(image, ImageFormat::Jpeg);
        assert!(plain.len() as u64 > MAX_IMAGE_PROBE_BYTES);
        let mut exif = b"Exif\0\0MM\0*\0\0\0\x08\0\x01".to_vec();
        exif.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0]);
        let mut tagged = plain[..2].to_vec();
        tagged.extend_from_slice(&[0xff, 0xe1]);
        tagged.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        tagged.extend_from_slice(&exif);
        tagged.extend_from_slice(&plain[2..]);
        let (input, reads, len) = source(tagged.clone());
        let meta = probe_image_reader(input, len, &Limits::default()).unwrap();
        assert_eq!(meta, super::super::probe_image(&tagged, &Limits::default()).unwrap());
        assert!(reads.get() < 16 * 1024);
        let headers = jpeg_headers(&mut Cursor::new(tagged)).unwrap();
        assert!(headers.len() < 16 * 1024);
        // This is the exact synthetic/truncated-at-SOS input given to the pinned decoder.
        assert_eq!(super::super::probe_image(&headers, &Limits::default()).unwrap(), meta);
        assert!(headers.len() < plain.len());
        // The pinned pixel decoder is permissive about missing entropy. Header success
        // and that decoder's success are not complete image-validity proofs.
        // A non-square control makes the orientation assertion discriminating.
        let image = DynamicImage::ImageRgb8(ImageBuffer::from_fn(40, 20, |_, _| Rgb([3, 4, 5])));
        let plain = encode(image, ImageFormat::Jpeg);
        let mut tagged = plain[..2].to_vec();
        tagged.extend_from_slice(&[0xff, 0xe1]);
        tagged.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        tagged.extend_from_slice(&exif);
        tagged.extend_from_slice(&plain[2..]);
        let len = tagged.len() as u64;
        let meta = probe_image_reader(Cursor::new(tagged), len, &Limits::default()).unwrap();
        assert_eq!((meta.width, meta.height), (20, 40));
    }
    #[test]
    fn hostile_segment_lengths_eof_and_header_budgets_fail_boundedly() {
        for bytes in [
            vec![],
            vec![0xff, 0xd8],
            vec![0xff, 0xd8, 0xff],
            vec![0xff, 0xd8, 0xff, 0xe1, 0, 0],
            vec![0xff, 0xd8, 0xff, 0xe1, 0, 1],
            vec![0xff, 0xd8, 0xff, 0xda, 0, 10, 1],
        ] {
            let (input, reads, len) = source(bytes);
            assert!(probe_image_reader(input, len, &Limits::default()).is_err());
            assert!(reads.get() <= MAX_IMAGE_PROBE_BYTES);
        }
        let mut excessive = vec![0xff, 0xd8];
        for _ in 0..10 {
            excessive.extend_from_slice(&[0xff, 0xe2, 0xff, 0xff]);
            excessive.resize(excessive.len() + 65533, 0);
        }
        let (input, reads, len) = source(excessive);
        assert!(matches!(probe_image_reader(input, len, &Limits::default()), Err(MediaError::LimitExceeded(_))));
        assert!(reads.get() <= MAX_IMAGE_PROBE_BYTES);
        let mut absent = vec![0xff, 0xd8];
        for _ in 0..100 {
            absent.extend_from_slice(&[0xff, 0x01]);
        }
        let (input, reads, len) = source(absent);
        assert!(probe_image_reader(input, len, &Limits::default()).is_err());
        assert!(reads.get() <= MAX_IMAGE_PROBE_BYTES);
    }
    #[test]
    fn encoded_and_decoded_caps_and_short_sources_still_refuse() {
        let bytes = encode(DynamicImage::ImageRgb8(ImageBuffer::from_fn(100, 20, |_, _| Rgb([1, 2, 3]))), ImageFormat::Png);
        let len = bytes.len() as u64;
        let limits = Limits { max_input_bytes: 1, ..Limits::default() };
        let (input, reads, _) = source(bytes.clone());
        assert!(matches!(probe_image_reader(input, len, &limits), Err(MediaError::TooLarge { .. })));
        assert_eq!(reads.get(), 0);
        let limits = Limits { max_width: 50, ..Limits::default() };
        assert!(probe_image_reader(Cursor::new(bytes.clone()), len, &limits).is_err());
        let limits = Limits { max_alloc: 100, ..Limits::default() };
        assert!(probe_image_reader(Cursor::new(bytes.clone()), len, &limits).is_err());
        assert!(probe_image_reader(Cursor::new(bytes[..20].to_vec()), len, &Limits::default()).is_err());
    }
}
