//! The frame-based video actions' executor (#108): extract frames, contact sheet, long exposure,
//! reverse and pan and zoom. Pictures are decoded one frame at a time on the video service and
//! staged as PNG; videos are drawn here a frame at a time and pushed through the service's frame
//! renderer, so memory holds one frame, never the whole video. Every run is bounded in frames,
//! pixels and seconds, and checks for a cancel between frames. Natively only: browsers have no
//! video service.
use super::{LocalActions, number, whole, word};
use nodecraft_nodes::video_frames::{CONTACT_SHEET, FRAMES, KEN_BURNS, LONG_EXPOSURE, MAX_FRAME_SIDE, MAX_PAN_SECONDS, MAX_PAN_SIDE, MAX_PICTURES, REVERSE};
use nodecraft_types::ParamValues;

/// Which frames Extract frames takes.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Pick {
    /// The frame at this time.
    One(u64),
    /// This many, evenly spaced.
    Count(u64),
    /// One every this many microseconds, from the start.
    Every(u64),
}

/// How Long exposure blends its frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blend {
    Average,
    Lighten,
    Darken,
}

/// Where Pan and zoom moves across the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pan {
    None,
    Right,
    Left,
    Down,
    Up,
}

/// Pan and zoom's motion.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Motion {
    zoom_from: f64,
    zoom_to: f64,
    pan: Pan,
    smooth: bool,
}

/// The settings an action runs with, checked before any video is imported.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Job {
    Frames { pick: Pick, size: u32 },
    Sheet { columns: u32, rows: u32, cell: u32, gap: u32, background: [u8; 4] },
    Exposure { blend: Blend, start: u64, end: u64, frames: u64, size: u32 },
    Reverse { boomerang: bool },
    Pan { length_us: u64, fps: u32, size: u32, motion: Motion },
}

/// A whole-number setting from `min` to `max`.
fn within(params: &ParamValues, name: &str, what: &str, min: u64, max: u64) -> Result<u32, String> {
    let value = whole(params, name)?;
    if !(min..=max).contains(&value) {
        return Err(format!("the {what} is from {min} to {max}"));
    }
    u32::try_from(value).map_err(|_| format!("the {what} is from {min} to {max}"))
}

/// A time setting in microseconds, from 0 to `max` seconds.
fn micros(params: &ParamValues, name: &str, max: f64) -> Result<u64, String> {
    let seconds = number(params, name)?;
    if !(0.0..=max).contains(&seconds) {
        return Err(format!("the {name} is from 0 to {max} seconds"));
    }
    // In range, so the product fits.
    Ok((seconds * 1_000_000.0).round() as u64)
}

fn zoom(params: &ParamValues, name: &str) -> Result<f64, String> {
    let value = number(params, name)?;
    if !(1.0..=4.0).contains(&value) {
        return Err("a zoom is from 1 to 4".into());
    }
    Ok(value)
}

fn job(action: &str, params: &ParamValues, inputs: usize) -> Result<Job, String> {
    let what = if action == KEN_BURNS { "a picture" } else { "a video" };
    if inputs == 0 {
        return Err(format!("connect {what} first"));
    }
    Ok(match action {
        FRAMES => {
            let pick = match word(params, "pick")? {
                "one" => Pick::One(micros(params, "at", nodecraft_nodes::video_edit::MAX_SECONDS)?),
                "count" => Pick::Count(u64::from(within(params, "frames", "number of frames", 2, MAX_PICTURES)?)),
                "every" => {
                    let every = micros(params, "every", 600.0)?;
                    if every < 100_000 {
                        return Err("frames are at least 0.1 seconds apart".into());
                    }
                    Pick::Every(every)
                }
                _ => return Err("the frames are one, evenly spaced or every few seconds".into()),
            };
            Job::Frames { pick, size: within(params, "size", "longest side", 16, MAX_FRAME_SIDE)? }
        }
        CONTACT_SHEET => {
            let (columns, rows) = (within(params, "columns", "number of columns", 1, 16)?, within(params, "rows", "number of rows", 1, 16)?);
            if u64::from(columns) * u64::from(rows) > MAX_PICTURES {
                return Err(format!("a contact sheet holds at most {MAX_PICTURES} frames; use fewer rows or columns"));
            }
            Job::Sheet {
                columns,
                rows,
                cell: within(params, "cell", "frame size", 32, 1024)?,
                gap: within(params, "gap", "gap", 0, 64)?,
                background: super::document::rgba(word(params, "background")?)?,
            }
        }
        LONG_EXPOSURE => {
            let blend = match word(params, "blend")? {
                "average" => Blend::Average,
                "lighten" => Blend::Lighten,
                "darken" => Blend::Darken,
                _ => return Err("the blend is average, lighten or darken".into()),
            };
            let max = nodecraft_nodes::video_edit::MAX_SECONDS;
            let (start, end) = (micros(params, "start", max)?, micros(params, "end", max)?);
            if end != 0 && start >= end {
                return Err("the start comes at or after the end".into());
            }
            Job::Exposure {
                blend,
                start,
                end,
                frames: u64::from(within(params, "frames", "number of frames", 2, MAX_PICTURES)?),
                size: within(params, "size", "longest side", 16, MAX_FRAME_SIDE)?,
            }
        }
        REVERSE => Job::Reverse {
            boomerang: match word(params, "mode")? {
                "reverse" => false,
                "boomerang" => true,
                _ => return Err("the video plays backwards or forwards then backwards".into()),
            },
        },
        KEN_BURNS => {
            let length_us = micros(params, "seconds", MAX_PAN_SECONDS)?;
            if length_us < 1_000_000 {
                return Err("the video is at least 1 second long".into());
            }
            let pan = match word(params, "pan")? {
                "none" => Pan::None,
                "right" => Pan::Right,
                "left" => Pan::Left,
                "down" => Pan::Down,
                "up" => Pan::Up,
                _ => return Err("the pan is none, right, left, down or up".into()),
            };
            let smooth = match word(params, "easing")? {
                "smooth" => true,
                "linear" => false,
                _ => return Err("the motion is smooth or linear".into()),
            };
            let fps = match word(params, "fps")? {
                "24" => 24,
                "30" => 30,
                "60" => 60,
                _ => return Err("the frame rate is 24, 30 or 60".into()),
            };
            let motion = Motion { zoom_from: zoom(params, "zoom_from")?, zoom_to: zoom(params, "zoom_to")?, pan, smooth };
            Job::Pan { length_us, fps, size: within(params, "size", "longest side", 64, MAX_PAN_SIDE)?, motion }
        }
        _ => return Err(format!("{action} isn't a video action this host runs")),
    })
}

/// `n` times spread evenly over `[start, end)`, each in the middle of its share.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn spread(n: u64, start: u64, end: u64) -> Vec<u64> {
    let span = u128::from(end.saturating_sub(start));
    let n = n.max(1);
    (0..n)
        .map(|i| {
            let offset = span * (2 * u128::from(i) + 1) / (2 * u128::from(n));
            start.saturating_add(u64::try_from(offset).unwrap_or(u64::MAX))
        })
        .collect()
}

/// The times Extract frames decodes in a video `length_us` long.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn picks(pick: Pick, length_us: u64) -> Result<Vec<u64>, String> {
    use super::timeline::seconds;
    match pick {
        Pick::One(at) if at < length_us => Ok(vec![at]),
        Pick::One(at) => Err(format!("the time ({}) is at or after the video's end ({})", seconds(at), seconds(length_us))),
        Pick::Count(n) => Ok(spread(n, 0, length_us)),
        Pick::Every(every) => {
            let n = length_us.div_ceil(every.max(1));
            if n > MAX_PICTURES {
                return Err(format!("that's {n} frames; Extract frames takes at most {MAX_PICTURES}, so space them further apart"));
            }
            Ok((0..n).map(|i| i.saturating_mul(every)).collect())
        }
    }
}

/// The number of frames a video `length_us` long shows at `fps`, at least one.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn frame_count(length_us: u64, fps: (u32, u32)) -> u64 {
    let (n, d) = (u128::from(fps.0), u128::from(fps.1).max(1));
    let count = u128::from(length_us) * n / (d * 1_000_000);
    u64::try_from(count).unwrap_or(u64::MAX).max(1)
}

/// When frame `index` at `fps` starts (rounded down), inside a video `length_us` long. A frame is
/// asked for by its start: the video service returns the first frame at or after a time.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn frame_start(index: u64, fps: (u32, u32), length_us: u64) -> u64 {
    let (n, d) = (u128::from(fps.0).max(1), u128::from(fps.1));
    let time = u128::from(index) * d * 1_000_000 / n;
    u64::try_from(time).unwrap_or(u64::MAX).min(length_us.saturating_sub(1))
}

/// The start of the frame showing at `time_us` in a video `length_us` long at `fps`.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn on_frame(time_us: u64, fps: (u32, u32), length_us: u64) -> u64 {
    let (n, d) = (u128::from(fps.0), u128::from(fps.1).max(1));
    let index = u64::try_from(u128::from(time_us) * n / (d * 1_000_000)).unwrap_or(u64::MAX);
    frame_start(index.min(frame_count(length_us, fps).saturating_sub(1)), fps, length_us)
}

/// The source frames, in the order Reverse shows them, for a video of `count` frames: last to
/// first, then (for a boomerang) on to the last again without showing either end twice.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn reversed(count: u64, boomerang: bool) -> Vec<u64> {
    let back = (0..count).rev();
    if boomerang && count > 1 { (0..count).chain(back.skip(1).take(usize::try_from(count - 2).unwrap_or(0))).collect() } else { back.collect() }
}

/// `pixels`, a `from` RGBA picture, centred on an opaque black `to` canvas (cropped if larger).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn fit(pixels: &[u8], from: (u32, u32), to: (u32, u32)) -> Vec<u8> {
    let (fw, fh, tw, th) = (from.0 as usize, from.1 as usize, to.0 as usize, to.1 as usize);
    if from == to && pixels.len() == fw * fh * 4 {
        return pixels.to_vec();
    }
    let mut out = [0, 0, 0, 255].repeat(tw * th);
    let (copy_w, copy_h) = (fw.min(tw), fh.min(th));
    let (src_x, src_y, dst_x, dst_y) = ((fw - copy_w) / 2, (fh - copy_h) / 2, (tw - copy_w) / 2, (th - copy_h) / 2);
    for row in 0..copy_h {
        let src = ((src_y + row) * fw + src_x) * 4;
        let dst = ((dst_y + row) * tw + dst_x) * 4;
        if let (Some(from), Some(to)) = (pixels.get(src..src + copy_w * 4), out.get_mut(dst..dst + copy_w * 4)) {
            to.copy_from_slice(from);
        }
    }
    out
}

/// `(width, height)` fitted so its longer side is at most `max`, even, at least 16 a side.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn within_side((w, h): (u32, u32), max: u32) -> (u32, u32) {
    let longest = w.max(h).max(1);
    let side = |v: u32| {
        let fitted = if longest > max { u64::from(v) * u64::from(max) / u64::from(longest) } else { u64::from(v) };
        u32::try_from(fitted).unwrap_or(max).clamp(16, max.max(16)) & !1
    };
    (side(w), side(h))
}

/// A picture `width`×`height` scaled so its longer side is `size`: even sides of at least 16.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn scaled((width, height): (u32, u32), size: u32) -> (u32, u32) {
    let (w, h) = (u64::from(width.max(1)), u64::from(height.max(1)));
    let longest = w.max(h);
    let side = |v: u64| u32::try_from((v * u64::from(size) + longest / 2) / longest).unwrap_or(size).clamp(16, size.max(16)) & !1;
    (side(w), side(h))
}

/// Frames blended into one picture.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct Exposure {
    blend: Blend,
    width: u32,
    height: u32,
    /// Per channel: the sum (average) or the lightest or darkest value so far.
    values: Vec<u32>,
    frames: u32,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl Exposure {
    fn new(blend: Blend, width: u32, height: u32) -> Self {
        let start = if blend == Blend::Darken { 255 } else { 0 };
        Self { blend, width, height, values: vec![start; width as usize * height as usize * 4], frames: 0 }
    }

    /// Adds a frame the exposure's size.
    fn add(&mut self, pixels: &[u8]) {
        for (value, &p) in self.values.iter_mut().zip(pixels) {
            let p = u32::from(p);
            *value = match self.blend {
                Blend::Average => value.saturating_add(p),
                Blend::Lighten => (*value).max(p),
                Blend::Darken => (*value).min(p),
            };
        }
        self.frames = self.frames.saturating_add(1);
    }

    fn pixels(&self) -> Vec<u8> {
        let frames = self.frames.max(1);
        self.values
            .iter()
            .map(|&v| if self.blend == Blend::Average { u8::try_from((v + frames / 2) / frames).unwrap_or(255) } else { u8::try_from(v).unwrap_or(255) })
            .collect()
    }
}

/// A contact sheet being filled: `columns` across, each cell one frame's shape, `gap` apart and
/// around, over `background`.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct Sheet {
    columns: u32,
    cell: (u32, u32),
    gap: u32,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
impl Sheet {
    fn new(columns: u32, rows: u32, cell: (u32, u32), gap: u32, background: [u8; 4]) -> Result<Self, String> {
        let (cw, ch, g) = (u64::from(cell.0), u64::from(cell.1), u64::from(gap));
        let (w, h) = (u64::from(columns) * (cw + g) + g, u64::from(rows) * (ch + g) + g);
        let limits = nodecraft_actions::image::ImageLimits::default();
        if w > u64::from(limits.edge) || h > u64::from(limits.edge) || w.saturating_mul(h) > limits.pixels {
            return Err("the contact sheet would be too large; use smaller frames or fewer of them".into());
        }
        let too_large = |_| "the contact sheet would be too large".to_owned();
        let (width, height) = (u32::try_from(w).map_err(too_large)?, u32::try_from(h).map_err(too_large)?);
        Ok(Self { columns: columns.max(1), cell, gap, width, height, pixels: background.repeat(width as usize * height as usize) })
    }

    /// Cell `n`'s top left corner.
    fn corner(&self, n: u32) -> (usize, usize) {
        let (step_x, step_y) = (self.cell.0 as usize + self.gap as usize, self.cell.1 as usize + self.gap as usize);
        ((n % self.columns) as usize * step_x + self.gap as usize, (n / self.columns) as usize * step_y + self.gap as usize)
    }

    /// Puts a `from` RGBA frame in cell `n`, fitted to the cell's shape.
    fn put(&mut self, n: u32, pixels: &[u8], from: (u32, u32)) {
        let cell = fit(pixels, from, self.cell);
        let (x, y) = self.corner(n);
        let (line, stride) = (self.cell.0 as usize * 4, self.width as usize * 4);
        for (row, source) in cell.chunks_exact(line.max(1)).enumerate() {
            let at = (y + row) * stride + x * 4;
            if let Some(to) = self.pixels.get_mut(at..at + line) {
                to.copy_from_slice(source);
            }
        }
    }
}

/// Pan and zoom's view of a `width`×`height` picture at `t` (0 to 1 through the video): its zoom
/// and the centre of the part shown, in the picture's pixels.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn view(motion: Motion, t: f64, (width, height): (u32, u32)) -> (f64, f64, f64) {
    let t = t.clamp(0.0, 1.0);
    let eased = if motion.smooth { t * t * (3.0 - 2.0 * t) } else { t };
    let zoom = motion.zoom_from + (motion.zoom_to - motion.zoom_from) * eased;
    let (w, h) = (f64::from(width), f64::from(height));
    let (vw, vh) = (w / zoom, h / zoom);
    let (px, py) = match motion.pan {
        Pan::None => (0.5, 0.5),
        Pan::Right => (eased, 0.5),
        Pan::Left => (1.0 - eased, 0.5),
        Pan::Down => (0.5, eased),
        Pan::Up => (0.5, 1.0 - eased),
    };
    (zoom, vw / 2.0 + (w - vw) * px, vh / 2.0 + (h - vh) * py)
}

/// One `out`-sized frame of a `size` RGBA picture zoomed `zoom` times about `(cx, cy)`, sampled
/// smoothly and kept inside the picture.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn pan_frame(pixels: &[u8], size: (u32, u32), out: (u32, u32), (zoom, cx, cy): (f64, f64, f64)) -> Vec<u8> {
    let (sw, sh) = (size.0.max(1) as usize, size.1.max(1) as usize);
    let (ow, oh) = (out.0 as usize, out.1 as usize);
    let (vw, vh) = (sw as f64 / zoom, sh as f64 / zoom);
    // Each output column's and row's two source columns or rows and the weight of the second.
    let taps = |count: usize, from: f64, step: f64, limit: usize| -> Vec<(usize, usize, f64)> {
        (0..count)
            .map(|i| {
                let at = (from + (i as f64 + 0.5) * step - 0.5).clamp(0.0, (limit - 1) as f64);
                let low = at.floor() as usize;
                (low, (low + 1).min(limit - 1), at - low as f64)
            })
            .collect()
    };
    let columns = taps(ow, cx - vw / 2.0, vw / ow.max(1) as f64, sw);
    let rows = taps(oh, cy - vh / 2.0, vh / oh.max(1) as f64, sh);
    let at = |x: usize, y: usize, c: usize| f64::from(pixels.get((y * sw + x) * 4 + c).copied().unwrap_or(0));
    let mut frame = Vec::with_capacity(ow * oh * 4);
    for &(y0, y1, fy) in &rows {
        for &(x0, x1, fx) in &columns {
            for c in 0..4 {
                let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
                let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
                frame.push((top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    frame
}

#[cfg(target_arch = "wasm32")]
impl LocalActions {
    pub(super) fn video_frames(
        &self,
        action: &str,
        params: &ParamValues,
        inputs: &[nodecraft_types::Value],
        _: &dyn Fn() -> bool,
    ) -> Result<nodecraft_types::Value, String> {
        job(action, params, inputs.len())?;
        Err("working with video frames needs the desktop app".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::super::timeline::native::{BUSY_WAIT, POLL, closed, message, service, wait};
    use super::super::video::{frame_for, quality};
    use super::{
        Exposure, Job, LocalActions, Sheet, fit, frame_count, frame_start, job, on_frame, pan_frame, picks, reversed, scaled, spread, view, within_side,
    };
    use nodecraft_actions::image::{ImageLimits, Raster};
    use nodecraft_media::video::{
        FramePacket, FrameRequest, FrameSender, RenderSpec, RenderedVideo, VideoAsset, VideoError, VideoFrame, VideoJob, VideoService,
    };
    use nodecraft_nodes::video_frames::{MAX_FRAME_SIDE, MAX_REVERSED};
    use nodecraft_types::{ParamValues, Value};
    use std::sync::Arc;
    use std::time::Instant;

    /// The frame at `time_us`, fitted within `width`×`height`.
    fn grab(service: &VideoService, video: &VideoAsset, time_us: u64, (width, height): (u32, u32), cancelled: &dyn Fn() -> bool) -> Result<VideoFrame, String> {
        let request = FrameRequest::new(time_us, width, height).map_err(message)?;
        let started = Instant::now();
        let job = loop {
            match service.frame(video, request) {
                Err(VideoError::Busy) if started.elapsed() < BUSY_WAIT && !cancelled() => std::thread::sleep(POLL),
                other => break other.map_err(|error| message(closed(error)))?,
            }
        };
        wait(job, cancelled).map_err(|error| match error {
            VideoError::NoFrame => format!("the video has no frame at {}", super::super::timeline::seconds(time_us)),
            other => message(other),
        })
    }

    /// The frame renderer for `spec`, once the service's one render slot is free.
    fn renderer(service: &VideoService, spec: RenderSpec, cancelled: &dyn Fn() -> bool) -> Result<(FrameSender, VideoJob<RenderedVideo>), String> {
        let started = Instant::now();
        loop {
            match service.render(spec) {
                Err(VideoError::Busy) if started.elapsed() < BUSY_WAIT && !cancelled() => std::thread::sleep(POLL.saturating_mul(10)),
                other => return other.map_err(|error| message(closed(error))),
            }
        }
    }

    /// Hands frame `index` to the renderer, waiting while its queue is full. A renderer that
    /// stopped says why through its job.
    fn push(sender: &mut FrameSender, job: &mut VideoJob<RenderedVideo>, index: u32, rgba: Vec<u8>, cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        let mut packet = FramePacket { index, rgba: Arc::from(rgba) };
        loop {
            if cancelled() {
                return Err(message(VideoError::Cancelled));
            }
            match sender.try_send(packet) {
                Ok(()) => return Ok(()),
                Err(refused) if refused.error == VideoError::Busy => {
                    packet = refused.packet;
                    std::thread::sleep(POLL);
                }
                Err(refused) => {
                    return Err(match job.try_result() {
                        Err(error) => message(closed(error)),
                        _ => message(closed(refused.error)),
                    });
                }
            }
        }
    }

    impl LocalActions {
        pub(in crate::action_exec) fn video_frames(
            &self,
            action: &str,
            params: &ParamValues,
            inputs: &[Value],
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            let job = job(action, params, inputs.len())?;
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to work with videos")?;
            if let Job::Pan { length_us, fps, size, motion } = job {
                let picture = self.raster(inputs.first())?;
                let service = service(tools, cancelled)?;
                return self.pan(&service, &picture, length_us, fps, size, motion, params, cancelled);
            }
            let service = service(tools, cancelled)?;
            let video = self.import_video(&service, inputs.first(), "connect a video first", cancelled)?;
            let length = video.metadata().duration_us;
            let m = video.metadata();
            let (shape, fps) = frame_for(m.width, m.height, m.sar, m.rotation, m.frame_rate);
            // Each picture is the frame showing at its time.
            let at = |time| on_frame(time, fps, length);
            match job {
                Job::Frames { pick, size } => {
                    let mut images = Vec::new();
                    for time in picks(pick, length)? {
                        let frame = grab(&service, &video, at(time), (size, size), cancelled)?;
                        images.push(self.keep_frame(&frame)?);
                    }
                    Ok(Value::List(images))
                }
                Job::Sheet { columns, rows, cell, gap, background } => {
                    let times = spread(u64::from(columns) * u64::from(rows), 0, length);
                    let mut sheet: Option<Sheet> = None;
                    for (n, time) in (0..).zip(times) {
                        let frame = grab(&service, &video, at(time), (cell, cell), cancelled)?;
                        let sheet = match &mut sheet {
                            Some(sheet) => sheet,
                            // The first frame sets every cell's shape.
                            empty => empty.insert(Sheet::new(columns, rows, (frame.width, frame.height), gap, background)?),
                        };
                        sheet.put(n, frame.pixels(), (frame.width, frame.height));
                    }
                    let sheet = sheet.ok_or("the video has no frames")?;
                    self.keep_raster(sheet.width, sheet.height, sheet.pixels)
                }
                Job::Exposure { blend, start, end, frames, size } => {
                    let end = if end == 0 { length } else { end };
                    if end > length || start >= end {
                        let s = super::super::timeline::seconds;
                        return Err(format!("the stretch ({} to {}) isn't inside the video ({})", s(start), s(end), s(length)));
                    }
                    let mut exposure: Option<Exposure> = None;
                    for time in spread(frames, start, end) {
                        let frame = grab(&service, &video, at(time), (size, size), cancelled)?;
                        let exposure = exposure.get_or_insert_with(|| Exposure::new(blend, frame.width, frame.height));
                        exposure.add(&fit(frame.pixels(), (frame.width, frame.height), (exposure.width, exposure.height)));
                    }
                    let exposure = exposure.ok_or("the video has no frames")?;
                    self.keep_raster(exposure.width, exposure.height, exposure.pixels())
                }
                Job::Reverse { boomerang } => {
                    let size = within_side(shape, u32::try_from(MAX_FRAME_SIDE).unwrap_or(2048));
                    let count = frame_count(length, fps);
                    if count > MAX_REVERSED {
                        return Err(format!("the video has {count} frames; Reverse plays at most {MAX_REVERSED}, so trim it first"));
                    }
                    let order = reversed(count, boomerang);
                    let out = u32::try_from(order.len()).map_err(|_| "the video is too long")?;
                    let spec = RenderSpec::new(size.0, size.1, fps, out, [0; 3]).map_err(message)?.with_quality(quality(params)?);
                    let (mut sender, mut render) = renderer(&service, spec, cancelled)?;
                    for (index, source) in order.into_iter().enumerate() {
                        let frame = grab(&service, &video, frame_start(source, fps, length), size, cancelled)?;
                        let index = u32::try_from(index).map_err(|_| "the video is too long")?;
                        push(&mut sender, &mut render, index, fit(frame.pixels(), (frame.width, frame.height), size), cancelled)?;
                    }
                    self.finish(&service, sender, render, size, fps, out, cancelled)
                }
                Job::Pan { .. } => Err("pan and zoom takes a picture".into()),
            }
        }

        /// Pan and zoom over `picture`: `length_us` at `fps`, its longer side `size`.
        #[allow(clippy::too_many_arguments)]
        fn pan(
            &self,
            service: &VideoService,
            picture: &Raster,
            length_us: u64,
            fps: u32,
            size: u32,
            motion: super::Motion,
            params: &ParamValues,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            let source = (picture.width(), picture.height());
            let out = scaled(source, size);
            let count = u32::try_from(frame_count(length_us, (fps, 1))).map_err(|_| "the video is too long")?;
            let spec = RenderSpec::new(out.0, out.1, (fps, 1), count, [0; 3]).map_err(message)?.with_quality(quality(params)?);
            let (mut sender, mut render) = renderer(service, spec, cancelled)?;
            let last = f64::from(count.saturating_sub(1).max(1));
            for index in 0..count {
                let frame = pan_frame(picture.pixels(), source, out, view(motion, f64::from(index) / last, source));
                push(&mut sender, &mut render, index, frame, cancelled)?;
            }
            self.finish(service, sender, render, out, (fps, 1), count, cancelled)
        }

        /// Ends a frame render of `count` frames at `fps` and stages the soundless video.
        #[allow(clippy::too_many_arguments)]
        fn finish(
            &self,
            service: &VideoService,
            sender: FrameSender,
            render: VideoJob<RenderedVideo>,
            size: (u32, u32),
            fps: (u32, u32),
            count: u32,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            sender.finish().map_err(|_| "not every frame reached the renderer".to_owned())?;
            let rendered = wait(render, cancelled).map_err(message)?;
            let duration_us = u64::try_from(u128::from(count) * u128::from(fps.1) * 1_000_000 / u128::from(fps.0.max(1))).unwrap_or(u64::MAX);
            self.keep(service, &rendered, size, duration_us, false, cancelled)
        }

        fn keep_raster(&self, width: u32, height: u32, pixels: Vec<u8>) -> Result<Value, String> {
            let raster = Raster::new(width, height, pixels, ImageLimits::default()).map_err(|e| format!("the picture can't be used: {e}"))?;
            self.stage(&raster).map_err(|e| format!("the picture can't be kept ({e}); make it smaller"))
        }

        fn keep_frame(&self, frame: &VideoFrame) -> Result<Value, String> {
            self.keep_raster(frame.width, frame.height, frame.pixels().to_vec())
                .map_err(|e| e.replace("the picture", "a frame").replace("make it smaller", "take fewer or smaller frames"))
        }
    }
}

#[cfg(test)]
mod tests;
