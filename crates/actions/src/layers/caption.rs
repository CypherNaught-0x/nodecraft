//! Captions (#574): a line or a few of text laid out as a text layer is, filled over an optional
//! outline, and placed in a video frame. Only the part of the frame the text covers is drawn, so
//! a video compositor can lay the picture over its frames where it says.
use super::{Align, Content, Layer, bad, paint, text, within};
use crate::image::{ImageError, ImageLimits, Raster};
use vello_cpu::kurbo::{Affine, Join, Stroke};
use vello_cpu::{Level, Pixmap, RenderContext, RenderSettings, Resources};

/// Where a caption sits up and down the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vertical {
    Top,
    Middle,
    Bottom,
}

/// How a caption looks and where it sits.
#[derive(Clone, Copy, Debug)]
pub struct CaptionStyle<'a> {
    pub text: &'a str,
    pub font: &'a str,
    /// In frame pixels.
    pub size: f64,
    pub align: Align,
    pub vertical: Vertical,
    /// Straight-alpha RGBA.
    pub color: [u8; 4],
    /// The outline's colour and how far it reaches past each letter, in frame pixels.
    pub stroke: Option<([u8; 4], f64)>,
}

/// A caption drawn: the picture, and where its top-left corner goes in the frame. The corner
/// and both sides are even, for 4:2:0 video.
#[derive(Debug)]
pub struct PlacedCaption {
    pub raster: Raster,
    pub x: u32,
    pub y: u32,
}

/// Longest caption text, in bytes.
pub const MAX_CAPTION_TEXT: usize = 4096;
/// Widest outline, in frame pixels.
pub const MAX_CAPTION_STROKE: f64 = 256.0;

/// The caption `style` in a `width`×`height` frame, kept a twentieth of the frame from its
/// edges; None when none of it falls in the frame.
pub fn caption(style: &CaptionStyle<'_>, width: u32, height: u32, limits: ImageLimits) -> Result<Option<PlacedCaption>, ImageError> {
    limits.validate()?;
    if style.text.len() > MAX_CAPTION_TEXT {
        return Err(bad("caption text"));
    }
    within(style.size, super::MIN_TEXT_SIZE, super::MAX_TEXT_SIZE, "text size")?;
    let outline = match style.stroke {
        Some((_, w)) => {
            within(w, 0.0, MAX_CAPTION_STROKE, "stroke width")?;
            w
        }
        None => 0.0,
    };
    if width == 0 || height == 0 || width > limits.edge || height > limits.edge || u64::from(width) * u64::from(height) > limits.pixels {
        return Err(ImageError::Dimensions);
    }
    let mut layer = Layer {
        id: "caption".into(),
        name: String::new(),
        visible: true,
        locked: false,
        opacity: 1.0,
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
        rotation: 0.0,
        flip_x: false,
        flip_y: false,
        content: Content::Text {
            text: style.text.into(),
            font: style.font.into(),
            size: style.size,
            line_spacing: 1.0,
            tracking: 0.0,
            align: style.align,
            color: String::new(),
        },
    };
    let (tw, th) = text::text_size(&layer)?;
    layer.width = tw;
    layer.height = th;
    let (fw, fh) = (f64::from(width), f64::from(height));
    let (mx, my) = (fw / 20.0, fh / 20.0);
    let left = match style.align {
        Align::Left => mx,
        Align::Center => (fw - tw) / 2.0,
        Align::Right => fw - mx - tw,
    };
    let top = match style.vertical {
        Vertical::Top => my,
        Vertical::Middle => (fh - th) / 2.0,
        Vertical::Bottom => fh - my - th,
    };
    // Room for the outline and for glyphs that reach past the line box.
    let pad = outline + style.size / 4.0 + 2.0;
    let span = |from: f64, to: f64, edge: u32| -> Option<(u32, u32)> {
        let start = (from.floor().max(0.0) as u64 & !1).min(u64::from(edge));
        let mut end = (to.ceil().max(0.0) as u64).min(u64::from(edge));
        if end <= start {
            return None;
        }
        if (end - start) % 2 == 1 {
            // The frame's sides are even, so an odd end is short of the edge.
            end = if end < u64::from(edge) { end + 1 } else { end - 1 };
        }
        let (start, end) = (u32::try_from(start).ok()?, u32::try_from(end).ok()?);
        (end - start >= 2).then_some((start, end - start))
    };
    let (Some((x, w)), Some((y, h))) = (span(left - pad, left + tw + pad, width), span(top - pad, top + th + pad, height)) else {
        return Ok(None);
    };
    let out_bytes = u64::from(w) * u64::from(h) * 4;
    if out_bytes.saturating_mul(2) > limits.working_bytes {
        return Err(ImageError::Dimensions);
    }
    if (out_bytes / 2).saturating_add((style.text.len() as u64).saturating_mul(4096)) > limits.work {
        return Err(ImageError::WorkLimit);
    }
    let (Ok(pw), Ok(ph)) = (u16::try_from(w), u16::try_from(h)) else { return Err(ImageError::Dimensions) };
    let path = text::outline(&layer)?;
    let mut ctx = RenderContext::new_with(pw, ph, RenderSettings { level: Level::baseline(), num_threads: 0 });
    ctx.set_transform(Affine::translate((left - f64::from(x), top - f64::from(y))));
    if let Some((c, _)) = style.stroke.filter(|_| outline > 0.0) {
        // Stroked twice as wide and filled over, so the outline reaches `outline` past each letter.
        ctx.set_paint(paint(c, 1.0));
        ctx.set_stroke(Stroke::new(outline * 2.0).with_join(Join::Round));
        ctx.stroke_path(&path);
    }
    ctx.set_paint(paint(style.color, 1.0));
    ctx.fill_path(&path);
    ctx.flush();
    let mut target = Pixmap::new(pw, ph);
    ctx.render(&mut target, &mut Resources::new());
    let mut pixels = crate::image::buffer(out_bytes as usize)?;
    for (out, px) in pixels.as_chunks_mut::<4>().0.iter_mut().zip(target.take_unpremultiplied()) {
        out.copy_from_slice(&[px.r, px.g, px.b, px.a]);
    }
    Ok(Some(PlacedCaption { raster: Raster::new(w, h, pixels, limits)?, x, y }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(text: &str) -> CaptionStyle<'_> {
        CaptionStyle {
            text,
            font: "lato",
            size: 40.0,
            align: Align::Center,
            vertical: Vertical::Bottom,
            color: [255, 255, 255, 255],
            stroke: Some(([0, 0, 0, 255], 3.0)),
        }
    }

    fn px(c: &PlacedCaption, x: u32, y: u32) -> [u8; 4] {
        let at = ((y * c.raster.width() + x) * 4) as usize;
        let p = c.raster.pixels();
        [p[at], p[at + 1], p[at + 2], p[at + 3]]
    }

    #[test]
    fn a_caption_is_drawn_where_it_sits_with_its_outline() {
        let limits = ImageLimits::default();
        let placed = caption(&style("Hello"), 640, 360, limits).unwrap().unwrap();
        let (w, h) = (placed.raster.width(), placed.raster.height());
        assert!([placed.x, placed.y, w, h].iter().all(|n| n.is_multiple_of(2)), "{placed:?}");
        assert!(placed.x + w <= 640 && placed.y + h <= 360);
        // Centred, near the bottom, a twentieth of the frame up.
        let middle = f64::from(placed.x) + f64::from(w) / 2.0;
        assert!((middle - 320.0).abs() < 4.0, "{middle}");
        assert!(placed.y > 240, "{placed:?}");
        let inked = (0..h).filter(|y| (0..w).any(|x| px(&placed, x, *y)[3] > 0)).max().unwrap();
        assert!(placed.y + inked <= 360 - 18 + 4, "the outline's lowest row stays a twentieth up: {inked}");
        let all: Vec<[u8; 4]> = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| px(&placed, x, y)).collect();
        assert!(all.contains(&[255, 255, 255, 255]), "white letters");
        assert!(all.contains(&[0, 0, 0, 255]), "a black outline");
        assert_eq!(px(&placed, 0, 0)[3], 0, "clear around the text");

        // No outline: no black; at the top, on the left.
        let plain = CaptionStyle { stroke: None, align: Align::Left, vertical: Vertical::Top, ..style("Hello") };
        let placed = caption(&plain, 640, 360, limits).unwrap().unwrap();
        assert!(placed.x <= 32 && placed.y <= 18, "{placed:?}");
        let pixels = placed.raster.pixels();
        assert!(!pixels.chunks(4).any(|p| p == [0, 0, 0, 255]));
        let right = CaptionStyle { align: Align::Right, vertical: Vertical::Middle, ..plain };
        let placed = caption(&right, 640, 360, limits).unwrap().unwrap();
        assert!(placed.x + placed.raster.width() > 600 && placed.y > 120 && placed.y < 180, "{placed:?}");
    }

    #[test]
    fn a_caption_wider_than_the_frame_is_cut_to_it_and_bad_styles_are_refused() {
        let limits = ImageLimits::default();
        let long = "A long caption that runs past both sides of a small frame";
        let placed = caption(&style(long), 64, 64, limits).unwrap().unwrap();
        assert_eq!((placed.x, placed.raster.width()), (0, 64));
        assert!(caption(&CaptionStyle { size: f64::NAN, ..style("x") }, 64, 64, limits).is_err());
        assert!(caption(&CaptionStyle { stroke: Some(([0; 4], -1.0)), ..style("x") }, 64, 64, limits).is_err());
        assert!(caption(&style(&"x".repeat(MAX_CAPTION_TEXT + 1)), 64, 64, limits).is_err());
        assert!(caption(&style("x"), 0, 64, limits).is_err());
        assert!(caption(&style("x"), 65_536, 65_536, limits).is_err());
        // An odd frame still gets an even picture inside it.
        let placed = caption(&style(long), 63, 63, limits).unwrap().unwrap();
        assert!(placed.raster.width().is_multiple_of(2) && placed.x + placed.raster.width() <= 63);
    }
}
