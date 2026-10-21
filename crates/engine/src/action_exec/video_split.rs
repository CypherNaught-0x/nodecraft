//! Split video and extract sound's executor (#108). A split cuts the video at frame starts into
//! equal parts, parts of a set length or its scene changes, and renders each part on its own
//! through the same composition the video edits use, so every part keeps its sound; the run checks
//! for a cancel between parts. Extract sound writes the video's own sound out as Ogg, copied when it
//! is already Opus or Vorbis. The source is never changed. Natively only: browsers have no video
//! service.
use super::{LocalActions, number, whole, word};
use nodecraft_nodes::video_split::{EXTRACT_AUDIO, MAX_PARTS, SPLIT};
use nodecraft_types::ParamValues;

/// The fewest frames a part shows: a one-frame part can come out a microsecond short of the
/// frame the renderer needs, so two is the shortest that always renders.
const MIN_FRAMES: u64 = 2;
/// Most bytes all the parts of one split may stage together.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_SPLIT_BYTES: u64 = 4 * super::timeline::MAX_RENDER_BYTES;

/// Where a split cuts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Split {
    /// Into this many parts of (to the frame) the same length.
    Parts(u64),
    /// Into parts this many microseconds long; the last takes what is left.
    Length(u64),
    /// At changes past `threshold` thousandths of the scene score, no part shorter than `min_us`.
    Scenes { threshold: u16, min_us: u64 },
}

/// The settings an action runs with, checked before any video is imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Job {
    Split(Split),
    ExtractAudio,
}

/// A time setting in microseconds, from `min` to `max` seconds.
fn micros(params: &ParamValues, name: &str, what: &str, min: f64, max: f64) -> Result<u64, String> {
    let seconds = number(params, name)?;
    if !(min..=max).contains(&seconds) {
        return Err(format!("the {what} is from {min} to {max} seconds"));
    }
    // In range, so the product fits.
    Ok((seconds * 1_000_000.0).round() as u64)
}

fn job(action: &str, params: &ParamValues, inputs: usize) -> Result<Job, String> {
    if inputs == 0 {
        return Err("connect a video first".into());
    }
    Ok(match action {
        SPLIT => Job::Split(match word(params, "by")? {
            "parts" => {
                let parts = whole(params, "parts")?;
                if !(2..=MAX_PARTS).contains(&parts) {
                    return Err(format!("the number of parts is from 2 to {MAX_PARTS}"));
                }
                Split::Parts(parts)
            }
            "length" => Split::Length(micros(params, "length", "part length", 0.1, nodecraft_nodes::video_edit::MAX_SECONDS)?),
            "scenes" => Split::Scenes {
                threshold: super::video::thousandths(params, "threshold", 50, 1000)?,
                min_us: micros(params, "min_length", "shortest part", 0.0, 600.0)?,
            },
            _ => return Err("a video splits into equal parts, every few seconds or at scene changes".into()),
        }),
        EXTRACT_AUDIO => Job::ExtractAudio,
        _ => return Err(format!("{action} isn't a video action this host runs")),
    })
}

/// The frame nearest `time_us` at `fps`, counting from 0.
fn nearest_frame(time_us: u64, fps: (u32, u32)) -> u64 {
    let unit = u128::from(fps.1).saturating_mul(1_000_000).max(1);
    let index = (u128::from(time_us).saturating_mul(u128::from(fps.0)).saturating_add(unit / 2)) / unit;
    u64::try_from(index).unwrap_or(u64::MAX)
}

/// When frame `index` at `fps` starts, rounded up to the microsecond so the cut never takes a
/// sliver of the frame before.
fn frame_at(index: u64, fps: (u32, u32)) -> u64 {
    let rate = u128::from(fps.0).max(1);
    let time = (u128::from(index).saturating_mul(u128::from(fps.1)).saturating_mul(1_000_000)).div_ceil(rate);
    u64::try_from(time).unwrap_or(u64::MAX)
}

/// The parts a split of a video `length_us` long at `fps` makes, as `[start, end)` microseconds
/// in time order: every cut on a frame start, every part at least [`MIN_FRAMES`] long, the last
/// ending at the video's end. `changes` are the video's scene changes (for a scene split).
///
/// A split by length gives the last part what is left; a remainder shorter than [`MIN_FRAMES`]
/// joins the part before, so the last part can run that little longer than the rest. A scene
/// split skips a change that would leave a part shorter than the shortest part (counting from the
/// previous cut and to the end); with no change left the whole video is the one part.
fn spans(split: Split, length_us: u64, fps: (u32, u32), changes: &[u64]) -> Result<Vec<(u64, u64)>, String> {
    use super::timeline::seconds;
    let frames = super::video_frames::frame_count(length_us, fps);
    // Frame indices where a part starts, after the first.
    let mut cuts = Vec::new();
    match split {
        Split::Parts(parts) => {
            if frames < parts.saturating_mul(MIN_FRAMES) {
                return Err(format!("the video has {frames} frames; {parts} parts need at least {}", parts.saturating_mul(MIN_FRAMES)));
            }
            let (f, p) = (u128::from(frames), u128::from(parts.max(1)));
            cuts.extend((1..p).map(|i| u64::try_from(f * i / p).unwrap_or(u64::MAX)));
        }
        Split::Length(length) => {
            // At least MIN_FRAMES whole frames, not rounded: two cuts then round at least that far apart.
            if u128::from(length).saturating_mul(u128::from(fps.0)) < u128::from(MIN_FRAMES).saturating_mul(u128::from(fps.1)).saturating_mul(1_000_000) {
                return Err(format!("a part is at least {MIN_FRAMES} frames ({}) long", seconds(frame_at(MIN_FRAMES, fps))));
            }
            let count = length_us.div_ceil(length.max(1));
            // One more than the most, which a short remainder may join to the part before.
            if count > MAX_PARTS.saturating_add(1) {
                return Err(format!("that's {count} parts; Split makes at most {MAX_PARTS}, so make the parts longer"));
            }
            // Each cut from the start, not the one before, so the rounding to frames never adds up.
            for k in 1..count {
                let cut = nearest_frame(length.saturating_mul(k), fps);
                if cut < frames && cut.saturating_sub(cuts.last().copied().unwrap_or(0)) >= MIN_FRAMES {
                    cuts.push(cut);
                }
            }
            if cuts.last().is_some_and(|&last| frames.saturating_sub(last) < MIN_FRAMES) {
                cuts.pop();
            }
        }
        Split::Scenes { min_us, .. } => {
            let mut before = 0u64;
            for &change in changes {
                let cut = nearest_frame(change, fps);
                let at = frame_at(cut, fps);
                let long_enough =
                    |from: u64, to: u64, from_us: u64, to_us: u64| to.saturating_sub(from) >= MIN_FRAMES && to_us.saturating_sub(from_us) >= min_us;
                if cut > before && long_enough(before, cut, frame_at(before, fps), at) && long_enough(cut, frames, at, length_us) {
                    cuts.push(cut);
                    before = cut;
                }
            }
        }
    }
    let count = cuts.len().saturating_add(1) as u64;
    if count > MAX_PARTS {
        return Err(match split {
            Split::Scenes { .. } => {
                format!("the video has {count} scenes; Split makes at most {MAX_PARTS} parts, so raise the threshold or the shortest part")
            }
            _ => format!("that's {count} parts; Split makes at most {MAX_PARTS}, so make the parts longer"),
        });
    }
    let starts = std::iter::once(0).chain(cuts.iter().map(|&cut| frame_at(cut, fps)));
    let ends = cuts.iter().map(|&cut| frame_at(cut, fps)).chain(std::iter::once(length_us));
    Ok(starts.zip(ends).collect())
}

#[cfg(target_arch = "wasm32")]
impl LocalActions {
    pub(super) fn video_split(
        &self,
        action: &str,
        params: &ParamValues,
        inputs: &[nodecraft_types::Value],
        _: &dyn Fn() -> bool,
    ) -> Result<nodecraft_types::Value, String> {
        job(action, params, inputs.len())?;
        Err("editing videos needs the desktop app".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) use native::scenes;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::super::timeline::native::{closed, message, service, wait};
    use super::super::video::{frame_for, quality};
    use super::{Job, LocalActions, MAX_SPLIT_BYTES, Split, job, spans};
    use nodecraft_media::video::{ComposeClip, ComposeSpec, Quality, VideoAsset, VideoError, VideoService};
    use nodecraft_types::{AssetRef, MediaMeta, ParamValues, Value};
    use std::sync::PoisonError;

    /// The scene changes of `video` past `threshold` thousandths, in microseconds from its start.
    pub(in crate::action_exec) fn scenes(service: &VideoService, video: &VideoAsset, threshold: u16, cancelled: &dyn Fn() -> bool) -> Result<Vec<u64>, String> {
        let job = service.scenes(video, threshold).map_err(|error| message(closed(error)))?;
        wait(job, cancelled).map_err(|error| match error {
            VideoError::Unsupported => "this ffmpeg can't find scene changes (it needs the select and metadata filters)".to_owned(),
            VideoError::Limit => "the video changes scene too often to count; raise the threshold".to_owned(),
            VideoError::Timeout => "finding the scene changes took too long".to_owned(),
            VideoError::Cancelled => "finding the scene changes was cancelled".to_owned(),
            other => message(other),
        })
    }

    impl LocalActions {
        pub(in crate::action_exec) fn video_split(
            &self,
            action: &str,
            params: &ParamValues,
            inputs: &[Value],
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            let job = job(action, params, inputs.len())?;
            let split = match job {
                Job::Split(split) => Some((split, quality(params)?)),
                Job::ExtractAudio => None,
            };
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to edit videos")?;
            let service = service(tools, cancelled)?;
            let video = self.import_video(&service, inputs.first(), "connect a video first", cancelled)?;
            let Some((split, quality)) = split else {
                return self.extract_audio(&service, &video, cancelled);
            };
            let m = video.metadata();
            let (size, fps) = frame_for(m.width, m.height, m.sar, m.rotation, m.frame_rate);
            let changes = match split {
                Split::Scenes { threshold, .. } => scenes(&service, &video, threshold, cancelled)?,
                _ => Vec::new(),
            };
            let spans = spans(split, m.duration_us, fps, &changes)?;
            // What was staged before, so a split that fails part way unstages only its own parts.
            let before: std::collections::BTreeSet<_> = self.videos.lock().unwrap_or_else(PoisonError::into_inner).keys().copied().collect();
            let mut parts = Vec::new();
            let result = self.split_parts(&service, &video, &spans, size, fps, quality, &mut parts, cancelled);
            if result.is_err() {
                let mut videos = self.videos.lock().unwrap_or_else(PoisonError::into_inner);
                for part in &parts {
                    if let Value::Media(asset) = part
                        && !before.contains(&asset.hash)
                    {
                        videos.remove(&asset.hash);
                    }
                }
            }
            result.map(|()| Value::List(parts))
        }

        /// Render each of `spans` of `video` into `parts`, stopping at the first that fails.
        #[allow(clippy::too_many_arguments)]
        fn split_parts(
            &self,
            service: &VideoService,
            video: &VideoAsset,
            spans: &[(u64, u64)],
            size: (u32, u32),
            fps: (u32, u32),
            quality: Quality,
            parts: &mut Vec<Value>,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<(), String> {
            let mut staged = 0u64;
            for (n, &(start, end)) in (1u64..).zip(spans) {
                if cancelled() {
                    return Err(message(VideoError::Cancelled));
                }
                let clip = ComposeClip::new(video.clone(), start, end).map_err(|_| format!("part {n} can't be cut from the video"))?;
                let spec = ComposeSpec::new(size.0, size.1, fps, [0; 3], vec![clip])
                    .map_err(|error| match error {
                        VideoError::Limit => "a part would be longer than an hour".to_owned(),
                        _ => format!("part {n} is shorter than a frame"),
                    })?
                    .with_quality(quality);
                let part = self.render(service, spec, size, cancelled)?;
                if let Value::Media(asset) = &part {
                    let bytes = self.videos.lock().unwrap_or_else(PoisonError::into_inner).get(&asset.hash).map_or(0, |b| b.len() as u64);
                    staged = staged.saturating_add(bytes);
                }
                parts.push(part);
                if staged > MAX_SPLIT_BYTES {
                    return Err(format!("the parts come to more than {} MiB; use a lower quality or split a shorter video", MAX_SPLIT_BYTES / (1024 * 1024)));
                }
            }
            Ok(())
        }

        /// `video`'s sound, written out as Ogg on `service` and staged as a sound.
        fn extract_audio(&self, service: &VideoService, video: &VideoAsset, cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
            if !video.metadata().has_audio() {
                return Err("the video has no sound".into());
            }
            let job = service.extract_audio(video).map_err(|error| message(closed(error)))?;
            let sound = wait(job, cancelled).map_err(|error| match error {
                VideoError::Unsupported => {
                    "this ffmpeg can't write the sound (it needs the Ogg muxer, and Opus for sound that isn't already Opus or Vorbis)".to_owned()
                }
                VideoError::Limit => "the sound is too large to write out".to_owned(),
                VideoError::Timeout => "writing the sound out took too long".to_owned(),
                VideoError::Cancelled => "writing the sound out was cancelled".to_owned(),
                _ => "the video's sound can't be written out".to_owned(),
            })?;
            let hash = self.read_back(sound.bytes(), &|offset, length| service.read_audio_chunk(sound.asset(), offset, length), cancelled)?;
            let m = sound.asset().metadata();
            let mime = "audio/ogg".parse().map_err(|_| "audio/ogg isn't a media type".to_owned())?;
            let meta = MediaMeta {
                duration_ms: Some(m.duration_us / 1000),
                sample_rate: Some(m.sample_rate),
                channels: u16::try_from(m.channels).ok(),
                ..MediaMeta::default()
            };
            Ok(Value::Media(AssetRef { hash, mime, meta }))
        }
    }
}

#[cfg(test)]
mod tests;
