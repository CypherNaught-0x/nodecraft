//! Brush masks and canvas expansion for inpainting and outpainting (#102).
//!
//! A mask is a list of brush strokes in normalized image coordinates: `x` and `y` run from 0 to
//! 1 across the image's width and height, and a stroke's `size` is its brush diameter as a
//! fraction of the image's longer side, so a mask follows its image through a resize or a new
//! version. `hardness` is the share of the brush radius at full strength; beyond it the strength
//! falls smoothly to zero at the edge. Strokes apply in order: painting raises the mask, erasing
//! lowers it.
//!
//! The rendered mask is one PNG in both conventions providers use: where the image should
//! change it is white and fully transparent, where it should stay it is black and opaque, and a
//! feathered edge is grey and partly transparent (alpha = 255 − value). Providers that read
//! transparency (OpenAI) and ones that read white (Stable Diffusion inpainting) take it as is.
use crate::image::{ImageError, ImageLimits, Raster};
use serde::{Deserialize, Serialize};

/// Most strokes in one mask.
pub const MAX_STROKES: usize = 1024;
/// Most points across all of a mask's strokes.
pub const MAX_POINTS: usize = 16_384;
/// Largest encoded mask, shared by its writer and reader.
pub const MAX_ENCODED_BYTES: usize = 256 * 1024;
/// Smallest brush, as a fraction of the longer side.
pub const MIN_SIZE: f64 = 1.0 / 4096.0;
/// Most pixels an expansion adds on one side.
pub const MAX_MARGIN: u32 = 4096;

/// One brush stroke: its points in order, in normalized coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stroke {
    pub size: f64,
    pub hardness: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub erase: bool,
    pub points: Vec<[f64; 2]>,
}

/// A mask's strokes, as stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Strokes {
    pub strokes: Vec<Stroke>,
    /// The image the strokes were painted over (#546); absent in projects saved before, and
    /// while nothing is painted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<Painted>,
}

/// The image a mask was painted over: its content hash (64 lowercase hex digits) and size in
/// pixels, before any expansion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Painted {
    pub hash: String,
    pub width: u32,
    pub height: u32,
}

/// Aspect ratios closer than this (relative) count as the same shape, so rounding to whole
/// pixels never reads as a reshape.
const SAME_SHAPE: f64 = 0.01;

impl Painted {
    /// Whether a `width` × `height` image has a different aspect ratio from this one.
    pub fn reshaped(&self, width: u32, height: u32) -> bool {
        let (a, b) = (f64::from(self.width) * f64::from(height), f64::from(width) * f64::from(self.height));
        (a - b).abs() > SAME_SHAPE * a.max(b)
    }
}

/// How a mask renders beyond its strokes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MaskOptions {
    /// Transparent pixels of the image are masked too, before the strokes apply: an expanded
    /// canvas's new margins, or a cut-out's background.
    pub transparent: bool,
    /// Everything is flipped at the end: the strokes mark what stays.
    pub invert: bool,
}

fn parameter(id: &'static str) -> ImageError {
    ImageError::Parameter(id)
}

impl Strokes {
    /// Reads stored strokes, bounded before parsing and checked after.
    pub fn parse(json: &str) -> Result<Self, ImageError> {
        if json.len() > MAX_ENCODED_BYTES {
            return Err(ImageError::RequestLimit);
        }
        let strokes: Self = serde_json::from_str(json).map_err(|_| ImageError::RequestSyntax)?;
        strokes.check()?;
        Ok(strokes)
    }

    /// The stored form, refused when it is larger than [`MAX_ENCODED_BYTES`].
    pub fn encode(&self) -> Result<String, ImageError> {
        self.check()?;
        let json = serde_json::to_string(self).map_err(|_| ImageError::RequestSyntax)?;
        if json.len() > MAX_ENCODED_BYTES {
            return Err(ImageError::RequestLimit);
        }
        Ok(json)
    }

    /// Every bound a stored mask keeps.
    pub fn check(&self) -> Result<(), ImageError> {
        if let Some(over) = &self.over
            && (over.hash.len() != 64 || !over.hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) || over.width == 0 || over.height == 0)
        {
            return Err(parameter("mask.over"));
        }
        if self.strokes.len() > MAX_STROKES {
            return Err(parameter("mask.strokes"));
        }
        let mut points = 0usize;
        for stroke in &self.strokes {
            if !stroke.size.is_finite() || !(MIN_SIZE..=1.0).contains(&stroke.size) {
                return Err(parameter("mask.size"));
            }
            if !stroke.hardness.is_finite() || !(0.0..=1.0).contains(&stroke.hardness) {
                return Err(parameter("mask.hardness"));
            }
            if stroke.points.is_empty() {
                return Err(parameter("mask.points"));
            }
            points = points.saturating_add(stroke.points.len());
            if points > MAX_POINTS {
                return Err(parameter("mask.points"));
            }
            // A little room outside the image, so a stroke can start past its edge.
            if stroke.points.iter().flatten().any(|v| !v.is_finite() || !(-1.0..=2.0).contains(v)) {
                return Err(parameter("mask.point"));
            }
        }
        Ok(())
    }

    /// Whether any stroke paints (an erase-only mask leaves nothing marked).
    pub fn paints(&self) -> bool {
        self.strokes.iter().any(|s| !s.erase)
    }
}

/// A stroke segment in pixel coordinates, with its brush.
struct Segment {
    a: (f64, f64),
    b: (f64, f64),
    radius: f64,
    inner: f64,
    erase: bool,
}

impl Segment {
    /// The pixel box it can touch, clamped to the image: x0..x1, y0..y1 (exclusive ends).
    fn bounds(&self, width: u32, height: u32) -> (u32, u32, u32, u32) {
        let clamp = |v: f64, max: u32| -> u32 {
            if v <= 0.0 {
                0
            } else if v >= f64::from(max) {
                max
            } else {
                v as u32
            }
        };
        let (x0, x1) = (self.a.0.min(self.b.0) - self.radius, self.a.0.max(self.b.0) + self.radius);
        let (y0, y1) = (self.a.1.min(self.b.1) - self.radius, self.a.1.max(self.b.1) + self.radius);
        (clamp(x0.floor(), width), clamp(x1.ceil(), width), clamp(y0.floor(), height), clamp(y1.ceil(), height))
    }

    /// Brush strength at pixel centre `p`, 0 to 255.
    fn strength(&self, p: (f64, f64)) -> u8 {
        let (dx, dy) = (self.b.0 - self.a.0, self.b.1 - self.a.1);
        let length = dx * dx + dy * dy;
        let t = if length > 0.0 { (((p.0 - self.a.0) * dx + (p.1 - self.a.1) * dy) / length).clamp(0.0, 1.0) } else { 0.0 };
        let (cx, cy) = (self.a.0 + t * dx - p.0, self.a.1 + t * dy - p.1);
        let d = (cx * cx + cy * cy).sqrt();
        let v = if d >= self.radius {
            0.0
        } else if d <= self.inner {
            1.0
        } else {
            let t = (self.radius - d) / (self.radius - self.inner);
            t * t * (3.0 - 2.0 * t)
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    }
}

fn segments(strokes: &Strokes, width: u32, height: u32) -> Vec<Segment> {
    let (w, h) = (f64::from(width), f64::from(height));
    let longer = w.max(h);
    let mut out = Vec::new();
    for stroke in &strokes.strokes {
        // At least half a pixel, so the smallest brush still marks the pixel it touches.
        let radius = (stroke.size * longer / 2.0).max(0.5);
        let inner = radius * stroke.hardness;
        let at = |p: &[f64; 2]| (p[0] * w, p[1] * h);
        let first = stroke.points.first().map(at);
        let pairs = stroke.points.windows(2).filter_map(|pair| Some((at(pair.first()?), at(pair.get(1)?))));
        let all: Vec<((f64, f64), (f64, f64))> = match first {
            Some(p) if stroke.points.len() == 1 => vec![(p, p)],
            _ => pairs.collect(),
        };
        out.extend(all.into_iter().map(|(a, b)| Segment { a, b, radius, inner, erase: stroke.erase }));
    }
    out
}

/// The work a mask of `strokes` over a `width` × `height` image takes: one unit per pixel, plus
/// one per pixel each stroke segment's box covers.
pub fn work(strokes: &Strokes, width: u32, height: u32) -> u64 {
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    segments(strokes, width, height).iter().fold(pixels, |n, s| {
        let (x0, x1, y0, y1) = s.bounds(width, height);
        n.saturating_add(u64::from(x1 - x0).saturating_mul(u64::from(y1 - y0)))
    })
}

/// The mask of `strokes` over `source`, at its size.
pub fn render(strokes: &Strokes, options: MaskOptions, source: &Raster, limits: ImageLimits) -> Result<Raster, ImageError> {
    strokes.check()?;
    let (width, height) = (source.width(), source.height());
    let bytes = limits.image_bytes(width, height)?;
    if work(strokes, width, height) > limits.work {
        return Err(ImageError::WorkLimit);
    }
    // The value plane, then the RGBA output, held together with the source.
    let pixels = usize::try_from(bytes / 4).map_err(|_| ImageError::Dimensions)?;
    if (source.capacity() as u64).saturating_add(bytes).saturating_add(bytes / 4) > limits.working_bytes {
        return Err(ImageError::ScratchLimit);
    }
    let mut values: Vec<u8> = Vec::new();
    values.try_reserve_exact(pixels).map_err(|_| ImageError::Allocation)?;
    if options.transparent {
        values.extend(source.pixels().as_chunks::<4>().0.iter().map(|p| 255 - p[3]));
    } else {
        values.resize(pixels, 0);
    }
    let row = width as usize;
    for segment in segments(strokes, width, height) {
        let (x0, x1, y0, y1) = segment.bounds(width, height);
        for y in y0..y1 {
            let start = (y as usize).saturating_mul(row);
            let Some(line) = values.get_mut(start + x0 as usize..start + x1 as usize) else { continue };
            for (x, value) in (x0..x1).zip(line.iter_mut()) {
                let s = segment.strength((f64::from(x) + 0.5, f64::from(y) + 0.5));
                *value = if segment.erase { (*value).min(255 - s) } else { (*value).max(s) };
            }
        }
    }
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(usize::try_from(bytes).map_err(|_| ImageError::Dimensions)?).map_err(|_| ImageError::Allocation)?;
    for v in values {
        let v = if options.invert { 255 - v } else { v };
        out.extend_from_slice(&[v, v, v, 255 - v]);
    }
    Raster::new(width, height, out, limits)
}

/// Pixels added on each side of an image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Margins {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

impl Margins {
    pub fn is_empty(self) -> bool {
        self == Self::default()
    }

    /// The expanded size of a `width` × `height` image, within `limits`.
    pub fn size(self, width: u32, height: u32, limits: ImageLimits) -> Result<(u32, u32), ImageError> {
        if [self.left, self.top, self.right, self.bottom].iter().any(|m| *m > MAX_MARGIN) {
            return Err(parameter("expand.margin"));
        }
        let w = width.checked_add(self.left).and_then(|w| w.checked_add(self.right)).ok_or(ImageError::Dimensions)?;
        let h = height.checked_add(self.top).and_then(|h| h.checked_add(self.bottom)).ok_or(ImageError::Dimensions)?;
        limits.image_bytes(w, h)?;
        Ok((w, h))
    }
}

/// `source` placed on a larger transparent canvas, `margins` from each edge.
pub fn expand(source: &Raster, margins: Margins, limits: ImageLimits) -> Result<Raster, ImageError> {
    let (w, h) = margins.size(source.width(), source.height(), limits)?;
    let bytes = limits.image_bytes(w, h)?;
    if (source.capacity() as u64).saturating_add(bytes) > limits.working_bytes {
        return Err(ImageError::ScratchLimit);
    }
    let len = usize::try_from(bytes).map_err(|_| ImageError::Dimensions)?;
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(len).map_err(|_| ImageError::Allocation)?;
    out.resize(len, 0);
    let (row, source_row) = (w as usize * 4, source.width() as usize * 4);
    for (y, line) in source.pixels().chunks_exact(source_row.max(1)).enumerate() {
        let start = (y + margins.top as usize) * row + margins.left as usize * 4;
        if let Some(target) = out.get_mut(start..start + source_row) {
            target.copy_from_slice(line);
        }
    }
    Raster::new(w, h, out, limits)
}

#[cfg(test)]
mod tests;
