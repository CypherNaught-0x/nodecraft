//! Probe and reserve decode/conversion lifetimes before pixels are decoded.
use super::validate::{add, mul};
use super::{CpuExecutor, ImageError as E, ImageInputs, ImageLimits, ImageRequest, ImageResult, Raster};
use nodecraft_media::image::DynamicImage;
use nodecraft_media::{ColorModel, ImageMeta};
fn media_error(error: nodecraft_media::MediaError) -> E {
    match error {
        nodecraft_media::MediaError::TooLarge { .. } => E::InputLimit,
        nodecraft_media::MediaError::LimitExceeded(_) => E::DecodeLimit,
        nodecraft_media::MediaError::Unsupported { .. } => E::UnsupportedFormat,
        _ => E::Decode,
    }
}
fn media_limits(l: ImageLimits, alloc: u64) -> nodecraft_media::Limits {
    nodecraft_media::Limits { max_input_bytes: l.encoded_bytes, max_width: l.edge, max_height: l.edge, max_alloc: alloc }
}
fn decoded_bytes(m: ImageMeta) -> Result<u64, E> {
    if m.bits_per_channel > 16 {
        return Err(E::UnsupportedColorModel);
    }
    let channels = match m.color {
        ColorModel::Gray => 1,
        ColorModel::GrayAlpha => 2,
        ColorModel::Rgb => 3,
        ColorModel::Rgba => 4,
    };
    mul(mul(u64::from(m.width), u64::from(m.height))?, channels * u64::from(m.bits_per_channel.max(8)).div_ceil(8))
}
pub(super) fn reserve(m: ImageMeta, other: u64, l: ImageLimits) -> Result<u64, E> {
    let rgba = l.image_bytes(m.width, m.height)?;
    let decoded = decoded_bytes(m)?;
    // Public ImageMeta hides orientation: reserve a decoded-size copy conservatively.
    let available = l.working_bytes.checked_sub(add(add(other, rgba)?, 4096)?).ok_or(E::ScratchLimit)?;
    if mul(decoded, 2)? > available {
        return Err(E::ScratchLimit);
    }
    Ok(available)
}
fn capacity(image: &DynamicImage) -> Result<u64, E> {
    Ok(match image {
        DynamicImage::ImageLuma8(i) => i.as_raw().capacity() as u64,
        DynamicImage::ImageLumaA8(i) => i.as_raw().capacity() as u64,
        DynamicImage::ImageRgb8(i) => i.as_raw().capacity() as u64,
        DynamicImage::ImageRgba8(i) => i.as_raw().capacity() as u64,
        DynamicImage::ImageLuma16(i) => mul(i.as_raw().capacity() as u64, 2)?,
        DynamicImage::ImageLumaA16(i) => mul(i.as_raw().capacity() as u64, 2)?,
        DynamicImage::ImageRgb16(i) => mul(i.as_raw().capacity() as u64, 2)?,
        DynamicImage::ImageRgba16(i) => mul(i.as_raw().capacity() as u64, 2)?,
        _ => return Err(E::UnsupportedColorModel),
    })
}
fn convert(image: &DynamicImage, meta: ImageMeta, other: u64, l: ImageLimits) -> Result<Raster, E> {
    let bytes = l.image_bytes(meta.width, meta.height)?;
    if add(add(other, capacity(image)?)?, bytes)? > l.working_bytes {
        return Err(E::ScratchLimit);
    }
    let mut out = super::raster::buffer(bytes as usize)?;
    fn copy<T: Copy>(src: &[T], out: &mut [u8], channels: usize, gray: bool, alpha: bool, to_byte: impl Fn(T) -> u8) {
        for (input, output) in src.chunks_exact(channels).zip(out.as_chunks_mut::<4>().0.iter_mut()) {
            let mut values = input.iter().copied().map(&to_byte);
            let r = values.next().unwrap_or(0);
            let g = if gray { r } else { values.next().unwrap_or(0) };
            let b = if gray { r } else { values.next().unwrap_or(0) };
            let a = if alpha { values.next().unwrap_or(255) } else { 255 };
            output.copy_from_slice(&[r, g, b, a]);
        }
    }
    let u16byte = |v: u16| ((u32::from(v) + 128) / 257) as u8;
    match image {
        DynamicImage::ImageLuma8(i) => copy(i.as_raw(), &mut out, 1, true, false, |v| v),
        DynamicImage::ImageLumaA8(i) => copy(i.as_raw(), &mut out, 2, true, true, |v| v),
        DynamicImage::ImageRgb8(i) => copy(i.as_raw(), &mut out, 3, false, false, |v| v),
        DynamicImage::ImageRgba8(i) => copy(i.as_raw(), &mut out, 4, false, true, |v| v),
        DynamicImage::ImageLuma16(i) => copy(i.as_raw(), &mut out, 1, true, false, u16byte),
        DynamicImage::ImageLumaA16(i) => copy(i.as_raw(), &mut out, 2, true, true, u16byte),
        DynamicImage::ImageRgb16(i) => copy(i.as_raw(), &mut out, 3, false, false, u16byte),
        DynamicImage::ImageRgba16(i) => copy(i.as_raw(), &mut out, 4, false, true, u16byte),
        _ => return Err(E::UnsupportedColorModel),
    }
    Raster::new(meta.width, meta.height, out, l)
}
impl ImageRequest {
    /// Encoded adapters probe action feasibility before decode, then retain decode/conversion
    /// reservations through each phase. Caller-owned encoded buffers remain charged throughout.
    pub fn execute_encoded(&self, source: &[u8], overlay: Option<&[u8]>, exec: &mut CpuExecutor, l: ImageLimits) -> Result<ImageResult, E> {
        l.validate()?;
        super::validate::parameters(self)?;
        if matches!(self, Self::Overlay { .. }) != overlay.is_some() {
            return Err(E::InputRoles);
        }
        let encoded = add(source.len() as u64, overlay.map_or(0, |b| b.len() as u64))?;
        if source.len() as u64 > l.encoded_bytes || overlay.is_some_and(|b| b.len() as u64 > l.encoded_bytes) || encoded > l.input_bytes {
            return Err(E::InputLimit);
        }
        let remaining = l.working_bytes.checked_sub(encoded).ok_or(E::ScratchLimit)?;
        let limits = ImageLimits { working_bytes: remaining, ..l };
        let first = nodecraft_media::probe_image(source, &media_limits(l, remaining)).map_err(media_error)?;
        decoded_bytes(first)?;
        let first_rgba = l.image_bytes(first.width, first.height)?;
        let second = overlay.map(|bytes| nodecraft_media::probe_image(bytes, &media_limits(l, remaining)).map_err(media_error)).transpose()?;
        if let Some(meta) = second {
            decoded_bytes(meta)?;
            l.image_bytes(meta.width, meta.height)?;
        }
        let second_rgba = second.map(|m| l.image_bytes(m.width, m.height)).transpose()?.unwrap_or(0);
        if add(first_rgba, second_rgba)? > l.input_bytes {
            return Err(E::InputLimit);
        }
        let plan = super::geometry::layout(self, first.width, first.height, second.map(|m| (m.width, m.height)))?;
        super::validate::admit(self, &plan, add(first_rgba, second_rgba)?, limits)?;
        let first_alloc = reserve(first, 0, limits)?;
        let second_alloc = second.map(|m| reserve(m, first_rgba, limits)).transpose()?;
        let (meta, decoded) = nodecraft_media::decode_image(source, &media_limits(l, first_alloc)).map_err(media_error)?;
        if meta != first {
            return Err(E::Decode);
        }
        let source = convert(&decoded, meta, 0, limits)?;
        drop(decoded);
        let second = if let (Some(bytes), Some(expected), Some(alloc)) = (overlay, second, second_alloc) {
            let (meta, decoded) = nodecraft_media::decode_image(bytes, &media_limits(l, alloc)).map_err(media_error)?;
            if meta != expected {
                return Err(E::Decode);
            }
            let result = convert(&decoded, meta, source.capacity() as u64, limits)?;
            drop(decoded);
            Some(result)
        } else {
            None
        };
        self.execute(ImageInputs { source: &source, overlay: second.as_ref() }, exec, limits)
    }
}

fn meta(image: &DynamicImage) -> Result<ImageMeta, E> {
    let (color, bits) = match image {
        DynamicImage::ImageLuma8(_) => (ColorModel::Gray, 8),
        DynamicImage::ImageLumaA8(_) => (ColorModel::GrayAlpha, 8),
        DynamicImage::ImageRgb8(_) => (ColorModel::Rgb, 8),
        DynamicImage::ImageRgba8(_) => (ColorModel::Rgba, 8),
        DynamicImage::ImageLuma16(_) => (ColorModel::Gray, 16),
        DynamicImage::ImageLumaA16(_) => (ColorModel::GrayAlpha, 16),
        DynamicImage::ImageRgb16(_) => (ColorModel::Rgb, 16),
        DynamicImage::ImageRgba16(_) => (ColorModel::Rgba, 16),
        _ => return Err(E::UnsupportedColorModel),
    };
    Ok(ImageMeta {
        mime: "application/x-nodecraft-decoded",
        width: image.width(),
        height: image.height(),
        color,
        bits_per_channel: bits,
        has_alpha: matches!(color, ColorModel::GrayAlpha | ColorModel::Rgba),
        color_tag: None,
    })
}
impl ImageRequest {
    /// Borrowed decoded SDR input. Admit actual owned buffer capacities plus conversion overlap;
    /// unsupported float/HDR images fail before any RGBA normalization allocation.
    pub fn execute_decoded(&self, source: &DynamicImage, overlay: Option<&DynamicImage>, exec: &mut CpuExecutor, l: ImageLimits) -> Result<ImageResult, E> {
        l.validate()?;
        super::validate::parameters(self)?;
        if matches!(self, Self::Overlay { .. }) != overlay.is_some() {
            return Err(E::InputRoles);
        }
        let first_meta = meta(source)?;
        let second_meta = overlay.map(meta).transpose()?;
        l.image_bytes(first_meta.width, first_meta.height)?;
        if let Some(m) = second_meta {
            l.image_bytes(m.width, m.height)?;
        }
        let first_capacity = capacity(source)?;
        let second_capacity = overlay.map(capacity).transpose()?.unwrap_or(0);
        let original = add(first_capacity, second_capacity)?;
        if original > l.input_bytes {
            return Err(E::InputLimit);
        }
        let remaining = ImageLimits { working_bytes: l.working_bytes.checked_sub(original).ok_or(E::ScratchLimit)?, ..l };
        let normalized =
            add(l.image_bytes(first_meta.width, first_meta.height)?, second_meta.map(|m| l.image_bytes(m.width, m.height)).transpose()?.unwrap_or(0))?;
        let plan = super::geometry::layout(self, first_meta.width, first_meta.height, second_meta.map(|m| (m.width, m.height)))?;
        super::validate::admit(self, &plan, normalized, remaining)?;
        let first = convert(source, first_meta, second_capacity, l)?;
        let second =
            if let (Some(image), Some(m)) = (overlay, second_meta) { Some(convert(image, m, add(first_capacity, first.capacity() as u64)?, l)?) } else { None };
        self.execute(ImageInputs { source: &first, overlay: second.as_ref() }, exec, remaining)
    }
}
