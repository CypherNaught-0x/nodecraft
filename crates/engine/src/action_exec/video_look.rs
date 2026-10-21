//! The video looks and marks' executor (#108): grade, effect, key, watermark and duplicate. The
//! first four import the video into the video service the Timeline renders on and render one
//! composition of it to a WebM, sound kept; Duplicate only repeats the reference. Natively only
//! (but Duplicate): browsers have no video service.
use super::video::thousandths;
use super::{LocalActions, number, word};
use nodecraft_nodes::video_look::{DUPLICATE, FILTER, GRADE, KEY, MAX_COPIES, WATERMARK};
use nodecraft_types::{ParamValues, Value};

/// One effect over the whole picture; the amounts in thousandths (of a pixel for the blur).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Effect {
    Grayscale,
    Sepia,
    Invert,
    Blur(u16),
    Sharpen(u16),
    Vignette(u16),
}

/// What a watermark is.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Mark {
    /// Text this many thousandths of the video's height tall, in a straight-alpha colour.
    Text { text: String, size: u16, color: [u8; 4] },
    /// The connected picture, this many thousandths of the video's width wide.
    Image { width: u16 },
}

/// The settings an action runs with, checked before any video is imported.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Job {
    /// In the media look's units: brightness -1000..=1000, the rest thousandths.
    Grade {
        brightness: i16,
        contrast: u16,
        saturation: u16,
        gamma: u16,
    },
    Filter(Effect),
    /// The colour taken out, how near a colour must be to go (and its soft edge), in
    /// thousandths, and the colour behind when no picture is connected.
    Key {
        color: [u8; 3],
        similarity: u16,
        blend: u16,
        behind: [u8; 3],
    },
    /// The mark, its column and row (0 to 2 each), its margin in thousandths of the height and
    /// its opacity in thousandths.
    Watermark {
        mark: Mark,
        spot: (u8, u8),
        margin: u16,
        opacity: u16,
    },
    Duplicate(usize),
}

/// A percent setting in thousandths (tenths of a percent), from `min` to `max` percent.
fn per_mille(params: &ParamValues, name: &str, min: f64, max: f64) -> Result<u16, String> {
    let percent = number(params, name)?;
    if !(min..=max).contains(&percent) {
        return Err(format!("the {} is from {min} to {max}%", name.replace('_', " ")));
    }
    // At most 100%, so it fits.
    Ok((percent * 10.0).round() as u16)
}

fn rgba(params: &ParamValues, name: &str) -> Result<[u8; 4], String> {
    nodecraft_actions::layers::parse_color(word(params, name)?).map_err(|_| format!("the {name} is #rrggbb or #rrggbbaa"))
}

fn job(action: &str, params: &ParamValues, inputs: usize) -> Result<Job, String> {
    if inputs == 0 {
        return Err("connect a video first".into());
    }
    Ok(match action {
        GRADE => {
            let brightness = (number(params, "brightness")? * 1000.0).round();
            if !(-1000.0..=1000.0).contains(&brightness) {
                return Err("the brightness is from -1 to 1".into());
            }
            Job::Grade {
                // In range, so it fits.
                brightness: brightness as i16,
                contrast: thousandths(params, "contrast", 0, 3000)?,
                saturation: thousandths(params, "saturation", 0, 3000)?,
                gamma: thousandths(params, "gamma", 100, 4000)?,
            }
        }
        FILTER => Job::Filter(match word(params, "effect")? {
            "grayscale" => Effect::Grayscale,
            "sepia" => Effect::Sepia,
            "invert" => Effect::Invert,
            "blur" => Effect::Blur(thousandths(params, "blur", 100, 50_000)?),
            "sharpen" => Effect::Sharpen(thousandths(params, "sharpen", 100, 3000)?),
            "vignette" => Effect::Vignette(thousandths(params, "vignette", 50, 1000)?),
            _ => return Err("the effect is black and white, sepia, negative, blur, sharpen or vignette".into()),
        }),
        KEY => {
            let [r, g, b, _] = rgba(params, "key")?;
            let [br, bg, bb, _] = rgba(params, "background_color")?;
            Job::Key {
                color: [r, g, b],
                similarity: thousandths(params, "similarity", 10, 1000)?,
                blend: thousandths(params, "blend", 0, 1000)?,
                behind: [br, bg, bb],
            }
        }
        WATERMARK => {
            let mark = match word(params, "source")? {
                "text" => {
                    let text = word(params, "mark_text")?.trim();
                    if text.is_empty() {
                        return Err("type the watermark's text first".into());
                    }
                    if text.len() > nodecraft_actions::layers::caption::MAX_CAPTION_TEXT {
                        return Err("the watermark's text is too long".into());
                    }
                    Mark::Text { text: text.to_owned(), size: per_mille(params, "text_size", 1.0, 30.0)?, color: rgba(params, "text_color")? }
                }
                "image" => {
                    if inputs < 2 {
                        return Err("connect a picture to the mark input first".into());
                    }
                    Mark::Image { width: per_mille(params, "scale", 2.0, 100.0)? }
                }
                _ => return Err("the mark is text or the connected picture".into()),
            };
            let spot = match word(params, "position")? {
                "top_left" => (0, 0),
                "top" => (1, 0),
                "top_right" => (2, 0),
                "left" => (0, 1),
                "center" => (1, 1),
                "right" => (2, 1),
                "bottom_left" => (0, 2),
                "bottom" => (1, 2),
                "bottom_right" => (2, 2),
                _ => return Err("the position is a corner, an edge's middle or the centre".into()),
            };
            Job::Watermark { mark, spot, margin: per_mille(params, "margin", 0.0, 25.0)?, opacity: thousandths(params, "opacity", 0, 1000)? }
        }
        DUPLICATE => {
            let copies = super::whole(params, "copies")?;
            if !(2..=MAX_COPIES).contains(&copies) {
                return Err(format!("Duplicate makes 2 to {MAX_COPIES} copies"));
            }
            // At most 16, so it fits.
            Job::Duplicate(copies as usize)
        }
        _ => return Err(format!("{action} isn't a video action this host runs")),
    })
}

/// Where a `size` picture's top-left corner goes in a `frame` so that it sits at column (or
/// row) `spot` of 0 to 2, `margin` pixels in from the edge it is near, on an even pixel within
/// the frame.
fn corner(spot: u8, size: u32, frame: u32, margin: u32) -> u32 {
    let room = frame.saturating_sub(size);
    let at = match spot {
        0 => margin,
        1 => room / 2,
        _ => room.saturating_sub(margin),
    };
    at.min(room) & !1
}

/// The placement of a `picture` mark `width` thousandths of a `frame`'s width wide, at `spot`,
/// `margin` pixels in, with `opacity`, in the compositor's terms: where its centre is and its
/// size against the size at which it would fit the frame, both in thousandths.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn placed(picture: (u32, u32), frame: (u32, u32), width: u16, spot: (u8, u8), margin: u32) -> (i32, i32, u16) {
    let (pw, ph) = (f64::from(picture.0.max(1)), f64::from(picture.1.max(1)));
    let (fw, fh) = (f64::from(frame.0.max(1)), f64::from(frame.1.max(1)));
    let fit = (fw / pw).min(fh / ph);
    let scale = (fw * f64::from(width) / (pw * fit)).round().clamp(10.0, 4000.0);
    let (w, h) = (pw * fit * scale / 1000.0, ph * fit * scale / 1000.0);
    let centre = |spot: u8, size: f64, side: f64| {
        let m = f64::from(margin);
        let at = match spot {
            0 => m + size / 2.0,
            1 => side / 2.0,
            _ => side - m - size / 2.0,
        };
        (at / side * 1000.0).round().clamp(-1000.0, 2000.0) as i32
    };
    (centre(spot.0, w, fw), centre(spot.1, h, fh), scale as u16)
}

impl LocalActions {
    pub(super) fn video_look(&self, action: &str, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
        match job(action, params, inputs.len())? {
            Job::Duplicate(copies) => {
                let video = inputs.first().ok_or("connect a video first")?;
                Ok(Value::List(vec![video.clone(); copies]))
            }
            job => self.render_look(job, params, inputs, cancelled),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl LocalActions {
    fn render_look(&self, _: Job, _: &ParamValues, _: &[Value], _: &dyn Fn() -> bool) -> Result<Value, String> {
        Err("editing videos needs the desktop app".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::super::timeline::native::{closed, message, service, wait};
    use super::super::video::{frame_for, quality};
    use super::{Effect, Job, LocalActions, Mark, corner, placed};
    use nodecraft_media::video::{self as media, ComposeCaption, ComposeClip, ComposeSpec, Look, Placement, StillAsset, VideoError, VideoService};
    use nodecraft_types::{ParamValues, Value};
    use std::sync::Arc;

    fn still(service: &VideoService, rgba: &[u8], (width, height): (u32, u32), cancelled: &dyn Fn() -> bool) -> Result<StillAsset, String> {
        let job = service.stage_still(Arc::from(rgba), width, height).map_err(|(error, _)| match error {
            VideoError::Invalid => "the connected picture is larger than 8192 pixels a side".to_owned(),
            other => message(closed(other)),
        })?;
        wait(job, cancelled).map_err(message)
    }

    impl LocalActions {
        pub(super) fn render_look(&self, job: Job, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
            let quality = quality(params)?;
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to edit videos")?;
            let service = service(tools, cancelled)?;
            let video = self.import_video(&service, inputs.first(), "connect a video first", cancelled)?;
            let m = video.metadata();
            let length = m.duration_us;
            let (size, fps) = frame_for(m.width, m.height, m.sar, m.rotation, m.frame_rate);
            let whole = ComposeClip::new(video.clone(), 0, length).map_err(|_| "the video has no length".to_owned())?;
            let looked = |look: Look| whole.clone().with_look(look).map_err(|_| "the look can't be used".to_owned());
            let (mut background, mut main, mut layers, mut captions) = ([0; 3], Vec::new(), Vec::new(), Vec::new());
            match job {
                Job::Grade { brightness, contrast, saturation, gamma } => main.push(looked(Look { brightness, contrast, saturation, gamma, ..Look::NONE })?),
                Job::Filter(effect) => {
                    let effect = match effect {
                        Effect::Grayscale => media::Effect::Grayscale,
                        Effect::Sepia => media::Effect::Sepia,
                        Effect::Invert => media::Effect::Invert,
                        Effect::Blur(sigma) => media::Effect::Blur(sigma),
                        Effect::Sharpen(amount) => media::Effect::Sharpen(amount),
                        Effect::Vignette(strength) => media::Effect::Vignette(strength),
                    };
                    main.push(looked(Look { effect, ..Look::NONE })?);
                }
                Job::Key { color, similarity, blend, behind } => {
                    // The picture behind (or a plain colour) is the main track, held for the
                    // video's length; the keyed video plays over it as a layer, sound and all.
                    let back = match inputs.get(1) {
                        Some(value) => {
                            let picture = self.raster(Some(value)).map_err(|e| e.replace("the input image", "the connected picture"))?;
                            still(&service, picture.pixels(), (picture.width(), picture.height()), cancelled)?
                        }
                        None => {
                            background = behind;
                            let [r, g, b] = behind;
                            still(&service, &[r, g, b, 255].repeat(16 * 16), (16, 16), cancelled)?
                        }
                    };
                    main.push(ComposeClip::still(back, length).map_err(|_| "the video has no length".to_owned())?);
                    layers.push((looked(Look { key: Some(media::Key { color, similarity, blend }), ..Look::NONE })?, 0));
                }
                Job::Watermark { mark, spot, margin, opacity } => {
                    main.push(whole.clone());
                    let margin = u32::try_from(u64::from(size.1) * u64::from(margin) / 1000).unwrap_or(0);
                    match mark {
                        Mark::Image { width } => {
                            let picture =
                                self.raster(inputs.get(1)).map_err(|e| e.replace("connect an image first", "connect a picture to the mark input first"))?;
                            let shape = (picture.width(), picture.height());
                            let (pos_x, pos_y, scale) = placed(shape, size, width, spot, margin);
                            let placement = Placement { pos_x, pos_y, scale, opacity, ..Placement::IDENTITY };
                            let clip =
                                ComposeClip::still(still(&service, picture.pixels(), shape, cancelled)?, length).and_then(|c| c.with_placement(placement));
                            layers.push((clip.map_err(|_| "the mark can't be placed".to_owned())?, 0));
                        }
                        Mark::Text { text, size: tall, color } => {
                            use nodecraft_actions::layers::{Align, caption as draw};
                            let [r, g, b, a] = color;
                            // The opacity folds into the colour, the compositor drawing captions as they are.
                            let alpha = u8::try_from(u32::from(a) * u32::from(opacity) / 1000).unwrap_or(u8::MAX);
                            let style = draw::CaptionStyle {
                                text: &text,
                                font: nodecraft_actions::layers::BUILTIN_FONT,
                                size: (f64::from(size.1) * f64::from(tall) / 1000.0).max(nodecraft_actions::layers::MIN_TEXT_SIZE),
                                align: [Align::Left, Align::Center, Align::Right].get(usize::from(spot.0)).copied().unwrap_or(Align::Center),
                                vertical: draw::Vertical::Middle,
                                color: [r, g, b, alpha],
                                stroke: None,
                            };
                            let drawn = draw::caption(&style, size.0, size.1, nodecraft_actions::image::ImageLimits::default())
                                .map_err(|e| format!("the watermark can't be drawn: {e}"))?;
                            if let Some(drawn) = drawn {
                                let shape = (drawn.raster.width(), drawn.raster.height());
                                let (x, y) = (corner(spot.0, shape.0, size.0, margin), corner(spot.1, shape.1, size.1, margin));
                                let mark = still(&service, drawn.raster.pixels(), shape, cancelled)?;
                                captions.push(ComposeCaption::new(mark, x, y, 0, length).map_err(|_| "the watermark can't be placed".to_owned())?);
                            }
                        }
                    }
                }
                Job::Duplicate(_) => return Err("duplicating a video renders nothing".into()),
            }
            let spec = ComposeSpec::new(size.0, size.1, fps, background, main)
                .and_then(|s| s.with_layers(layers))
                .and_then(|s| s.with_captions(captions))
                .map_err(|error| match error {
                    VideoError::Limit => "the output would be longer than an hour".to_owned(),
                    _ => "the video is shorter than one frame".to_owned(),
                })?
                .with_quality(quality);
            self.render(&service, spec, size, cancelled)
        }
    }
}

#[cfg(test)]
mod tests;
