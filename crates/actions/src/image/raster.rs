use super::{ImageError, ImageLimits};
use std::sync::Arc;

/// Immutable tightly packed RGBA8 sRGB, straight alpha. Clones share validated capacity.
#[derive(Clone)]
pub struct Raster {
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
}
impl std::fmt::Debug for Raster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Raster").field("width", &self.width).field("height", &self.height).field("capacity", &self.pixels.capacity()).finish()
    }
}
impl Raster {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>, limits: ImageLimits) -> Result<Self, ImageError> {
        limits.validate()?;
        let bytes = limits.image_bytes(width, height)?;
        if pixels.len() as u64 != bytes {
            return Err(ImageError::BufferShape);
        }
        if pixels.capacity() as u64 > limits.input_bytes {
            return Err(ImageError::InputLimit);
        }
        if (pixels.capacity() as u64).checked_add(64).is_none_or(|n| n > limits.working_bytes) {
            return Err(ImageError::ScratchLimit);
        }
        Ok(Self { width, height, pixels: Arc::new(pixels) })
    }
    pub fn copy_from(width: u32, height: u32, pixels: &[u8], limits: ImageLimits) -> Result<Self, ImageError> {
        limits.validate()?;
        if limits.image_bytes(width, height)? != pixels.len() as u64 {
            return Err(ImageError::BufferShape);
        }
        if pixels.len() as u64 > limits.input_bytes {
            return Err(ImageError::InputLimit);
        }
        // Borrowed source extent remains alive through the new copy; charge both plus Arc/header.
        if (pixels.len() as u64).checked_mul(2).and_then(|n| n.checked_add(64)).is_none_or(|n| n > limits.working_bytes) {
            return Err(ImageError::ScratchLimit);
        }
        let mut out = buffer(pixels.len())?;
        out.copy_from_slice(pixels);
        Self::new(width, height, out, limits)
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
    pub fn capacity(&self) -> usize {
        self.pixels.capacity()
    }
    pub(super) fn output(width: u32, height: u32, pixels: Vec<u8>, limits: ImageLimits) -> Result<Self, ImageError> {
        if pixels.capacity() as u64 > limits.working_bytes {
            return Err(ImageError::ScratchLimit);
        }
        if pixels.len() as u64 != limits.image_bytes(width, height)? {
            return Err(ImageError::BufferShape);
        }
        Ok(Self { width, height, pixels: Arc::new(pixels) })
    }
}
#[cfg(test)]
thread_local! {static ALLOCATIONS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
pub(super) fn allocations() -> usize {
    ALLOCATIONS.get()
}
pub(crate) fn buffer(bytes: usize) -> Result<Vec<u8>, ImageError> {
    #[cfg(test)]
    ALLOCATIONS.set(ALLOCATIONS.get() + 1);
    let mut out = Vec::new();
    out.try_reserve_exact(bytes).map_err(|_| ImageError::Allocation)?;
    if out.capacity() != bytes {
        return Err(ImageError::Allocation);
    }
    out.resize(bytes, 0);
    Ok(out)
}

/// Borrowed tightly packed RGBA8. Only the visible slice extent is known, not its owner's capacity.
#[derive(Clone, Copy)]
pub struct RasterView<'a> {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pixels: &'a [u8],
}
impl<'a> RasterView<'a> {
    pub fn new(width: u32, height: u32, pixels: &'a [u8], l: ImageLimits) -> Result<Self, ImageError> {
        l.validate()?;
        if l.image_bytes(width, height)? != pixels.len() as u64 {
            return Err(ImageError::BufferShape);
        }
        if pixels.len() as u64 > l.input_bytes {
            return Err(ImageError::InputLimit);
        }
        Ok(Self { width, height, pixels })
    }
    pub fn width(self) -> u32 {
        self.width
    }
    pub fn height(self) -> u32 {
        self.height
    }
    pub fn pixels(self) -> &'a [u8] {
        self.pixels
    }
}
impl std::fmt::Debug for RasterView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterView").field("width", &self.width).field("height", &self.height).finish()
    }
}
