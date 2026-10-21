//! The video actions' executor (#108): trim, speed, stitch and replace sound. Each imports its
//! videos (and sound) into the video service the Timeline renders on, builds one composition and
//! renders it to a WebM. The tools are the ones the settings name (`[media]`), never a project's.
//! Natively only: browsers have no video service.
use super::{LocalActions, number, word};
use nodecraft_nodes::video_edit::{MAX_SECONDS, MAX_STITCHED, REPLACE_AUDIO, SPEED, STITCH, TRIM};
use nodecraft_types::ParamValues;

/// A time setting in microseconds: from 0 to an hour.
fn micros(params: &ParamValues, name: &str) -> Result<u64, String> {
    let seconds = number(params, name)?;
    if !(0.0..=MAX_SECONDS).contains(&seconds) {
        return Err(format!("the {name} is from 0 to {MAX_SECONDS} seconds"));
    }
    // In range, so the product fits.
    Ok((seconds * 1_000_000.0).round() as u64)
}

/// A factor setting (1 is as it is) in thousandths, from `min` to `max` thousandths.
fn thousandths(params: &ParamValues, name: &str, min: u16, max: u16) -> Result<u16, String> {
    let factor = number(params, name)?;
    let value = (factor * 1000.0).round();
    if !(f64::from(min)..=f64::from(max)).contains(&value) {
        return Err(format!("the {name} is from {} to {}", f64::from(min) / 1000.0, f64::from(max) / 1000.0));
    }
    Ok(value as u16)
}

/// The settings an action runs with, checked before any video is imported.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Edit {
    /// `[start, end)`; an `end` of 0 is the video's end.
    Trim {
        start: u64,
        end: u64,
    },
    Speed {
        speed: u16,
        mute: bool,
    },
    /// The crossfade between videos, or 0 for a cut.
    Stitch {
        crossfade: u64,
    },
    ReplaceAudio {
        keep_own: bool,
        volume: u16,
        offset: u64,
    },
}

fn edit(action: &str, params: &ParamValues, inputs: usize) -> Result<Edit, String> {
    use nodecraft_nodes::timeline::{MAX_SPEED, MAX_VOLUME, MIN_SPEED};
    let needs = |count: usize, what: &str| if inputs < count { Err(format!("connect {what} first")) } else { Ok(()) };
    Ok(match action {
        TRIM => {
            needs(1, "a video")?;
            let (start, end) = (micros(params, "start")?, micros(params, "end")?);
            if end != 0 && start >= end {
                return Err("the start comes at or after the end".into());
            }
            Edit::Trim { start, end }
        }
        SPEED => {
            needs(1, "a video")?;
            let mute = match word(params, "sound")? {
                "keep" => false,
                "mute" => true,
                _ => return Err("the sound is kept or silenced".into()),
            };
            Edit::Speed { speed: thousandths(params, "speed", MIN_SPEED, MAX_SPEED)?, mute }
        }
        STITCH => {
            needs(1, "the videos to stitch")?;
            if inputs > MAX_STITCHED {
                return Err(format!("{inputs} videos are connected; Stitch plays at most {MAX_STITCHED}"));
            }
            let crossfade = match word(params, "transition")? {
                "cut" => 0,
                "crossfade" => {
                    let length = micros(params, "transition_length")?;
                    if length < nodecraft_nodes::timeline::MIN_TRANSITION_US {
                        return Err("the crossfade is at least 0.01 seconds".into());
                    }
                    length
                }
                _ => return Err("the transition is a cut or a crossfade".into()),
            };
            Edit::Stitch { crossfade }
        }
        REPLACE_AUDIO => {
            needs(2, "a video and a sound")?;
            let keep_own = match word(params, "mode")? {
                "replace" => false,
                "mix" => true,
                _ => return Err("the video's own sound is replaced or mixed".into()),
            };
            Edit::ReplaceAudio { keep_own, volume: thousandths(params, "volume", 0, MAX_VOLUME)?, offset: micros(params, "offset")? }
        }
        _ => return Err(format!("{action} isn't a video action this host runs")),
    })
}

#[cfg(not(target_arch = "wasm32"))]
/// The render quality the action asks for; a job planned before it had one renders at Standard.
fn quality(params: &ParamValues) -> Result<nodecraft_media::video::Quality, String> {
    use nodecraft_media::video::Quality;
    match word(params, "quality").unwrap_or("standard") {
        "draft" => Ok(Quality::Draft),
        "standard" => Ok(Quality::Standard),
        "high" => Ok(Quality::High),
        _ => Err("the quality is draft, standard or high".into()),
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// The output picture for a video `width`×`height` pixels of aspect `sar`, turned `rotation`
/// degrees, at `rate`: its upright shape, fitted within 4096 a side and rounded to even sides
/// of at least 16, at its own rate when the renderer takes it and 30 a second otherwise.
fn frame_for(width: u32, height: u32, sar: (u32, u32), rotation: u16, rate: Option<(u64, u64)>) -> ((u32, u32), (u32, u32)) {
    use nodecraft_nodes::timeline::{MAX_FRAME, MIN_FRAME};
    let wide = if sar.0 == 0 || sar.1 == 0 { u64::from(width) } else { u64::from(width).saturating_mul(u64::from(sar.0)) / u64::from(sar.1) };
    let (w, h) = if rotation % 180 == 90 { (u64::from(height), wide) } else { (wide, u64::from(height)) };
    let (w, h) = (w.max(1), h.max(1));
    let longest = w.max(h);
    let side = |v: u64| {
        let fitted = if longest > MAX_FRAME { v.saturating_mul(MAX_FRAME) / longest } else { v };
        // Even, within the renderer's sides; both are at most 4096, so they fit.
        u32::try_from((fitted & !1).clamp(MIN_FRAME, MAX_FRAME)).unwrap_or(16)
    };
    let fps = rate
        .and_then(|(n, d)| {
            let common = gcd(n, d).max(1);
            let (n, d) = (u32::try_from(n / common).ok()?, u32::try_from(d / common).ok()?);
            nodecraft_media::video::RenderSpec::new(16, 16, (n, d), 1, [0; 3]).ok().map(|_| (n, d))
        })
        .unwrap_or((30, 1));
    ((side(w), side(h)), fps)
}

#[cfg(not(target_arch = "wasm32"))]
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(target_arch = "wasm32")]
impl LocalActions {
    pub(super) fn video_edit(
        &self,
        action: &str,
        params: &ParamValues,
        inputs: &[nodecraft_types::Value],
        _: &dyn Fn() -> bool,
    ) -> Result<nodecraft_types::Value, String> {
        edit(action, params, inputs.len())?;
        Err("editing videos needs the desktop app".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::super::timeline::native::{closed, message, service, wait};
    use super::super::timeline::seconds;
    use super::{Edit, LocalActions, edit, frame_for, quality};
    use nodecraft_media::video::{AudioAsset, ColorPolicy, ComposeClip, ComposeSpec, VideoAsset, VideoError, VideoService, layer_room_us};
    use nodecraft_types::{ParamValues, Value};

    impl LocalActions {
        fn import_video(&self, service: &VideoService, value: Option<&Value>, unconnected: &str, cancelled: &dyn Fn() -> bool) -> Result<VideoAsset, String> {
            let job = service.import(self.video_bytes(value, false, unconnected)?, ColorPolicy::default()).map_err(|(error, _)| message(closed(error)))?;
            wait(job, cancelled).map_err(message)
        }

        fn import_sound(&self, service: &VideoService, value: Option<&Value>, cancelled: &dyn Fn() -> bool) -> Result<AudioAsset, String> {
            let job = service
                .import_audio(self.video_bytes(value, true, "connect a sound to the audio input first")?)
                .map_err(|(error, _)| message(closed(error)))?;
            wait(job, cancelled).map_err(message)
        }

        pub(in crate::action_exec) fn video_edit(
            &self,
            action: &str,
            params: &ParamValues,
            inputs: &[Value],
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            let edit = edit(action, params, inputs.len())?;
            let quality = quality(params)?;
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to edit videos")?;
            let service = service(tools, cancelled)?;
            let unconnected = if matches!(edit, Edit::Stitch { .. }) { "input 1 isn't a video" } else { "connect a video first" };
            let first = self.import_video(&service, inputs.first(), unconnected, cancelled)?;
            let whole =
                |video: &VideoAsset| ComposeClip::new(video.clone(), 0, video.metadata().duration_us).map_err(|_| "a connected video has no length".to_owned());
            let m = first.metadata();
            let (size, fps) = frame_for(m.width, m.height, m.sar, m.rotation, m.frame_rate);
            let (mut main, mut layers) = (Vec::new(), Vec::new());
            match edit {
                Edit::Trim { start, end } => {
                    let length = first.metadata().duration_us;
                    let end = if end == 0 { length } else { end };
                    if end > length {
                        return Err(format!("the end ({}) is after the video's ({})", seconds(end), seconds(length)));
                    }
                    if start >= end {
                        return Err(format!("the start ({}) is at or after the end ({})", seconds(start), seconds(end)));
                    }
                    main.push(ComposeClip::new(first.clone(), start, end).map_err(|_| "the trim can't be used".to_owned())?);
                }
                Edit::Speed { speed, mute } => {
                    let clip = whole(&first)?.with_speed(speed).and_then(|c| c.with_volume(1000, mute));
                    main.push(clip.map_err(|_| "the speed can't be used".to_owned())?);
                }
                Edit::Stitch { crossfade } => {
                    let mut videos = vec![first.clone()];
                    for (n, value) in inputs.iter().enumerate().skip(1) {
                        videos.push(self.import_video(&service, Some(value), &format!("input {} isn't a video", n.saturating_add(1)), cancelled)?);
                    }
                    let last = videos.len().saturating_sub(1);
                    for (n, video) in videos.iter().enumerate() {
                        let clip = whole(video)?;
                        let clip = if n < last { clip.with_transition(crossfade) } else { Ok(clip) };
                        main.push(clip.map_err(|_| format!("the crossfade is longer than video {}", n.saturating_add(1)))?);
                    }
                }
                Edit::ReplaceAudio { keep_own, volume, offset } => {
                    let sound = self.import_sound(&service, inputs.get(1), cancelled)?;
                    let length = first.metadata().duration_us;
                    // The sound plays until the video ends, counted in the frames the render shows,
                    // so it never makes the output longer; a shorter one leaves silence after it.
                    let room = layer_room_us(length, offset, fps)
                        .ok_or_else(|| format!("the new sound starts ({}) after the video ends ({})", seconds(offset), seconds(length)))?;
                    let out = sound.metadata().duration_us.min(room);
                    main.push(whole(&first)?.with_volume(1000, !keep_own).map_err(|_| "the video's sound can't be set".to_owned())?);
                    let under = ComposeClip::audio(sound, 0, out).and_then(|c| c.with_volume(volume, false));
                    layers.push((under.map_err(|_| "the new sound can't be used".to_owned())?, offset));
                }
            }
            let short = match edit {
                Edit::Trim { .. } => "the trimmed video is shorter than one frame",
                Edit::Speed { .. } => "at this speed the video is shorter than one frame",
                Edit::Stitch { .. } => "a crossfade is longer than the videos on either side of it allow",
                Edit::ReplaceAudio { .. } => "the video or the new sound is shorter than one frame",
            };
            let spec = ComposeSpec::new(size.0, size.1, fps, [0; 3], main)
                .and_then(|s| s.with_layers(layers))
                .map_err(|error| match error {
                    VideoError::Limit => "the output would be longer than an hour".to_owned(),
                    _ => short.to_owned(),
                })?
                .with_quality(quality);
            self.render(&service, spec, size, cancelled)
        }
    }
}

#[cfg(test)]
mod tests;
