//! The Timeline's executor (#109, #576): imports each connected video, sound and picture into
//! the video service, checks the clips against their media's lengths, renders the main track and
//! the tracks over it into one WebM (VP9 picture, Opus sound), with its captions drawn over
//! them all (#574), and stages it. The tools are the ones the settings name (`[media]`), never a project's.
//! Natively only: browsers have no video service.
use super::{LocalActions, whole, word};
use nodecraft_nodes::timeline::{Input, Wire};
use nodecraft_types::{ParamValues, Value};

/// Most bytes a rendered timeline may have: it is held in memory until the host imports it.
pub(crate) const MAX_RENDER_BYTES: u64 = 256 * 1024 * 1024;
/// Most bytes one connected video may have, as the video service reads it.
pub(crate) const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;

/// The output picture a Timeline node's parameters describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub background: [u8; 3],
}

pub(crate) fn frame(params: &ParamValues) -> Result<Frame, String> {
    use nodecraft_nodes::timeline::{MAX_FRAME, MIN_FRAME, RATES};
    let edge = |name| {
        whole(params, name).and_then(|v| {
            u32::try_from(v)
                .ok()
                .filter(|v| (MIN_FRAME..=MAX_FRAME).contains(&u64::from(*v)) && v.is_multiple_of(2))
                .ok_or(format!("the {name} is an even number of pixels from 16 to 4096"))
        })
    };
    let rate = word(params, "fps")?;
    let fps = RATES.iter().find(|r| **r == rate).and_then(|r| r.parse::<u32>().ok()).ok_or("the frame rate isn't one the timeline offers")?;
    let [r, g, b, _] = nodecraft_actions::layers::parse_color(word(params, "background")?).map_err(|_| "the background is #rrggbb")?;
    Ok(Frame { width: edge("width")?, height: edge("height")?, fps: (fps, 1), background: [r, g, b] })
}

fn texts(params: &ParamValues, name: &str) -> Vec<String> {
    match params.get(name) {
        Some(Value::List(items)) => items.iter().map(|v| if let Value::Text(t) = v { t.clone() } else { String::new() }).collect(),
        _ => Vec::new(),
    }
}

/// Each of `count` inputs' port and origin node, as planned: an origin that isn't known is
/// empty, and a port that isn't (a job planned before #576) is `clips`.
pub(crate) fn wires(params: &ParamValues, count: usize) -> Vec<Wire> {
    let (origins, ports) = (texts(params, nodecraft_nodes::timeline::SOURCES_PARAM), texts(params, nodecraft_nodes::timeline::PORTS_PARAM));
    (0..count)
        .map(|i| {
            let input = ports.get(i).and_then(|p| Input::from_port(p)).unwrap_or_default();
            Wire::new(input, origins.get(i).cloned().unwrap_or_default())
        })
        .collect()
}

/// The clips a planned job carries, checked.
pub(crate) fn clips(params: &ParamValues) -> Result<nodecraft_nodes::timeline::Timeline, String> {
    let json = match params.get(nodecraft_nodes::timeline::SPEC_PARAM) {
        None => nodecraft_nodes::timeline::EMPTY,
        Some(Value::Text(json)) => json,
        Some(_) => return Err("the timeline can't be read".into()),
    };
    nodecraft_nodes::timeline::Timeline::parse(json).map_err(|e| e.message)
}

/// `S.s` seconds, for messages.
pub(super) fn seconds(us: u64) -> String {
    format!("{:.1} s", us as f64 / 1_000_000.0)
}

#[cfg(target_arch = "wasm32")]
impl LocalActions {
    pub(super) fn timeline(&self, params: &ParamValues, _: &[Value], _: &dyn Fn() -> bool) -> Result<Value, String> {
        frame(params)?;
        clips(params)?;
        Err("rendering a timeline needs the desktop app".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) mod native {
    use super::{Input, LocalActions, MAX_INPUT_BYTES, MAX_RENDER_BYTES, clips, frame, seconds, wires};
    use nodecraft_format::asset_source::TransferCancel;
    use nodecraft_media::video::{
        ColorPolicy, ComposeCaption, ComposeClip, ComposeSpec, MAX_CLIPS, Placement, Quality, RenderedVideo, ServiceConfig, Source, ToolPaths, VideoChunk,
        VideoError, VideoJob, VideoLimits, VideoService,
    };
    use nodecraft_nodes::timeline::{Timeline, caption};
    use nodecraft_types::{AssetRef, Blake3, MediaMeta, ParamValues, Value};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    type Tools = (PathBuf, PathBuf);

    /// The service renders share, for the tools it was opened with.
    static SERVICE: Mutex<Option<(Tools, Arc<VideoService>)>> = Mutex::new(None);
    /// How often a wait looks for a result or a cancel.
    pub(in crate::action_exec) const POLL: Duration = Duration::from_millis(10);
    /// Longest wait for another render to free the service's one render slot.
    pub(in crate::action_exec) const BUSY_WAIT: Duration = Duration::from_secs(3600);

    /// The render quality a Timeline node's parameters ask for; a job planned before there was
    /// a choice renders as it did then.
    fn quality(params: &ParamValues) -> Result<Quality, String> {
        match params.get("quality") {
            None => Ok(Quality::Standard),
            Some(Value::Text(q)) if q == "draft" => Ok(Quality::Draft),
            Some(Value::Text(q)) if q == "standard" => Ok(Quality::Standard),
            Some(Value::Text(q)) if q == "high" => Ok(Quality::High),
            Some(_) => Err("the quality is draft, standard or high".into()),
        }
    }

    /// Where `clip` sits in the frame, as the video service takes it.
    fn placement(clip: &nodecraft_nodes::timeline::Clip) -> Placement {
        let p = clip.placement();
        Placement { pos_x: p.pos_x, pos_y: p.pos_y, scale: p.scale, crop: p.crop, rotation: p.rotation, opacity: p.opacity }
    }

    pub(in crate::action_exec) fn message(error: VideoError) -> String {
        match error {
            VideoError::Unavailable => "can't run ffmpeg or ffprobe; check [media] in settings",
            VideoError::Unsupported => {
                "this ffmpeg can't render the video (it needs VP9, Opus and the xfade filter, and rotate, lutyuv, color and atempo for placed or sped-up clips), or a connected video's format isn't supported"
            }
            VideoError::InvalidMetadata => "a connected video's or sound's format isn't supported",
            VideoError::Limit => "the videos or the render are too large; make the output shorter or smaller",
            VideoError::Timeout => "the render took too long",
            VideoError::Cancelled => "the render was cancelled",
            VideoError::Busy => "another video is rendering; try again when it's done",
            VideoError::Invalid => "the video can't be rendered as set",
            _ => "the render failed",
        }
        .to_owned()
    }

    /// The shared service for `tools`, opened on first use (or when the tools change).
    pub(in crate::action_exec) fn service(tools: &Tools, cancelled: &dyn Fn() -> bool) -> Result<Arc<VideoService>, String> {
        let mut slot = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((open, service)) = slot.as_ref()
            && open == tools
        {
            return Ok(Arc::clone(service));
        }
        *slot = None;
        let config = ToolPaths::new(tools.0.clone(), tools.1.clone())
            .and_then(|paths| VideoLimits::default().lower_output(MAX_RENDER_BYTES).map(|limits| (paths, limits)))
            .and_then(|(paths, limits)| ServiceConfig::new(paths, std::env::temp_dir(), limits))
            .map_err(message)?;
        let mut opening = VideoService::begin_open(config).map_err(message)?;
        let service = loop {
            if let Some(service) = opening.try_result().map_err(message)? {
                break Arc::new(service);
            }
            if cancelled() {
                return Err(message(VideoError::Cancelled));
            }
            std::thread::sleep(POLL);
        };
        *slot = Some((tools.clone(), Arc::clone(&service)));
        Ok(service)
    }

    /// Forgets the shared service when `error` says it is gone, so the next render opens another.
    pub(in crate::action_exec) fn closed(error: VideoError) -> VideoError {
        if matches!(error, VideoError::Closed) {
            *SERVICE.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        error
    }

    /// The real fixture may share this process-wide slot with another ignored test. Detach
    /// only this exact service; never clear one another test opened with different tools.
    #[cfg(test)]
    pub(in crate::action_exec) fn detach_test_service(service: &Arc<VideoService>) {
        let mut slot = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|(_, current)| Arc::ptr_eq(current, service)) {
            *slot = None;
        }
    }

    /// `job`'s result; cancels the job once `cancelled` says so. The service bounds every job by
    /// a deadline of its own, so this ends.
    pub(in crate::action_exec) fn wait<T>(mut job: VideoJob<T>, cancelled: &dyn Fn() -> bool) -> Result<T, VideoError> {
        loop {
            if let Some(value) = job.try_result().map_err(closed)? {
                return Ok(value);
            }
            if cancelled() {
                job.cancel();
                return Err(VideoError::Cancelled);
            }
            std::thread::sleep(POLL);
        }
    }

    fn compose(service: &VideoService, spec: ComposeSpec, cancelled: &dyn Fn() -> bool) -> Result<RenderedVideo, String> {
        let started = Instant::now();
        let job = loop {
            match service.compose(spec.clone()) {
                Err(VideoError::Busy) if started.elapsed() < BUSY_WAIT && !cancelled() => std::thread::sleep(POLL.saturating_mul(10)),
                other => break other.map_err(|error| message(closed(error)))?,
            }
        };
        wait(job, cancelled).map_err(message)
    }

    fn video_value(hash: Blake3, size: (u32, u32), duration_us: u64, sound: bool) -> Result<Value, String> {
        let mime = "video/webm".parse().map_err(|_| "video/webm isn't a media type".to_owned())?;
        let meta = MediaMeta {
            width: Some(size.0),
            height: Some(size.1),
            duration_ms: Some(duration_us / 1000),
            sample_rate: sound.then_some(48_000),
            channels: sound.then_some(2),
            ..MediaMeta::default()
        };
        Ok(Value::Media(AssetRef { hash, mime, meta }))
    }

    impl LocalActions {
        /// The bytes of the video (or, for `audio`, the sound) `value` names: one an earlier
        /// action in this run made, or the project's. `unconnected` says what to do without one.
        pub(in crate::action_exec) fn video_bytes(&self, value: Option<&Value>, audio: bool, unconnected: &str) -> Result<Arc<[u8]>, String> {
            let (kind, what) = if audio { ("audio/", "sound") } else { ("video/", "video") };
            let Some(Value::Media(source)) = value.filter(|v| matches!(v, Value::Media(a) if a.mime.essence().starts_with(kind))) else {
                return Err(unconnected.to_owned());
            };
            if let Some(bytes) = self.videos.lock().unwrap_or_else(PoisonError::into_inner).get(&source.hash) {
                return Ok(Arc::clone(bytes));
            }
            if let Some(bytes) = self.assets.get(&source.hash) {
                return Ok(Arc::from(bytes));
            }
            let input = self.assets.preview_read(source).map_err(|error| match error {
                nodecraft_format::PreviewReadError::Missing => format!("a connected {what} isn't in this project"),
                _ => format!("a connected {what} can't be read"),
            })?;
            if input.byte_len() > MAX_INPUT_BYTES {
                return Err(format!("a connected {what} is larger than {} MiB", MAX_INPUT_BYTES / (1024 * 1024)));
            }
            let bytes = input.read_verified(&TransferCancel::default(), &Default::default()).map_err(|_| format!("a connected {what} can't be read"))?;
            Ok(Arc::from(bytes))
        }

        /// `value` staged in the video service as the port `input` says.
        fn import(&self, service: &VideoService, input: Input, value: Option<&Value>, cancelled: &dyn Fn() -> bool) -> Result<Source, String> {
            Ok(match input {
                Input::Clips => {
                    let job = service
                        .import(self.video_bytes(value, false, "connect a video to the timeline's clips input first")?, ColorPolicy::default())
                        .map_err(|(error, _)| message(closed(error)))?;
                    Source::Video(wait(job, cancelled).map_err(message)?)
                }
                Input::Audio => {
                    let job = service
                        .import_audio(self.video_bytes(value, true, "connect a sound to the timeline's audio input first")?)
                        .map_err(|(error, _)| message(closed(error)))?;
                    Source::Audio(wait(job, cancelled).map_err(message)?)
                }
                Input::Stills => {
                    let picture = self.raster(value).map_err(|e| {
                        e.replace("connect an image first", "connect a picture to the timeline's stills input first")
                            .replace("the input image", "a connected picture")
                            .replace("the image", "a connected picture")
                    })?;
                    let job = service.stage_still(Arc::from(picture.pixels()), picture.width(), picture.height()).map_err(|(error, _)| match error {
                        VideoError::Invalid => "a connected picture is larger than 8192 pixels a side".to_owned(),
                        other => message(closed(other)),
                    })?;
                    Source::Still(wait(job, cancelled).map_err(message)?)
                }
            })
        }

        /// Each enabled caption that starts before `duration_us`, drawn for a `frame` and staged.
        fn captions(
            &self,
            service: &VideoService,
            timeline: &Timeline,
            frame: super::Frame,
            duration_us: u64,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Vec<ComposeCaption>, String> {
            use nodecraft_actions::layers::{Align, caption as draw, parse_color};
            let mut out = Vec::new();
            for (n, c) in timeline.captions.iter().enumerate().filter(|(_, c)| c.enabled && c.start_us < duration_us) {
                let number = n.saturating_add(1);
                let color = |text: &str| parse_color(text).map_err(|_| format!("caption {number}'s colours are #rrggbb or #rrggbbaa"));
                let style = draw::CaptionStyle {
                    text: &c.text,
                    font: &c.font,
                    size: f64::from(c.size),
                    align: match c.align {
                        caption::Align::Left => Align::Left,
                        caption::Align::Center => Align::Center,
                        caption::Align::Right => Align::Right,
                    },
                    vertical: match c.position {
                        caption::Position::Top => draw::Vertical::Top,
                        caption::Position::Middle => draw::Vertical::Middle,
                        caption::Position::Bottom => draw::Vertical::Bottom,
                    },
                    color: color(&c.color)?,
                    stroke: if c.stroke_width > 0 { Some((color(&c.stroke)?, f64::from(c.stroke_width))) } else { None },
                };
                let placed = draw::caption(&style, frame.width, frame.height, nodecraft_actions::image::ImageLimits::default())
                    .map_err(|e| format!("caption {number} can't be drawn: {e}"))?;
                let Some(placed) = placed else { continue };
                let job = service
                    .stage_still(Arc::from(placed.raster.pixels()), placed.raster.width(), placed.raster.height())
                    .map_err(|(error, _)| message(closed(error)))?;
                let still = wait(job, cancelled).map_err(message)?;
                out.push(ComposeCaption::new(still, placed.x, placed.y, c.start_us, c.end_us).map_err(|_| format!("caption {number} can't be placed"))?);
            }
            Ok(out)
        }

        pub(in crate::action_exec) fn timeline(&self, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
            let frame = frame(params)?;
            let quality = quality(params)?;
            let wires = wires(params, inputs.len());
            let timeline = clips(params)?;
            if timeline.unplaced_sound(&wires) {
                return Err(format!(
                    "a connected sound has no sound track to play on: remove one of the {} picture tracks",
                    nodecraft_nodes::timeline::MAX_TRACKS
                ));
            }
            let listed = timeline.with_sources(&wires);
            let resolved = timeline.resolve(&wires);
            if resolved.is_empty() {
                return Err("the timeline has no clips to render: connect a video, sound or picture, or enable a clip".into());
            }
            if !resolved.iter().any(|(_, c)| c.track == 0) {
                return Err("the main track has no clips to play: connect a video or picture, or enable one of its clips".into());
            }
            // Before any media is imported: each can be hundreds of megabytes.
            if resolved.len() > MAX_CLIPS {
                return Err(format!("the timeline has {} enabled clips; it renders at most {MAX_CLIPS}: disable or remove some", resolved.len()));
            }
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to render timelines")?;
            let service = service(tools, cancelled)?;
            // One input at a time: its bytes are dropped once the service holds its own copy.
            let mut imported: Vec<Option<Source>> = vec![None; inputs.len()];
            for (input, _) in &resolved {
                let port = wires.get(*input).map(|w| w.input);
                let (Some(slot), Some(port)) = (imported.get_mut(*input), port) else { return Err("a clip names an input that isn't connected".into()) };
                if slot.is_none() {
                    *slot = Some(self.import(&service, port, inputs.get(*input), cancelled)?);
                }
            }
            let (mut main, mut layers) = (Vec::new(), Vec::new());
            for (n, (input, clip)) in resolved.iter().enumerate() {
                // Numbered as the editor lists them, disabled clips included.
                let number = listed.clips.iter().position(|c| c.id == clip.id).unwrap_or(n).saturating_add(1);
                let source = imported.get(*input).and_then(Option::as_ref).ok_or("a clip names an input that isn't connected")?;
                // The end of a video or sound, checked against its length.
                let end = |duration: u64, what: &str| {
                    let out = clip.out_us.unwrap_or(duration);
                    if out > duration {
                        return Err(format!("clip {number} ends after its {what} does ({})", seconds(duration)));
                    }
                    if clip.in_us >= out {
                        return Err(format!("clip {number} starts after its {what} ends ({})", seconds(duration)));
                    }
                    Ok(out)
                };
                let made = match source {
                    Source::Video(video) => ComposeClip::new(video.clone(), clip.in_us, end(video.metadata().duration_us, "video")?),
                    Source::Audio(sound) => ComposeClip::audio(sound.clone(), clip.in_us, end(sound.metadata().duration_us, "sound")?),
                    Source::Still(still) => {
                        ComposeClip::still(still.clone(), clip.out_us.unwrap_or(nodecraft_nodes::timeline::DEFAULT_STILL_US).saturating_sub(clip.in_us))
                    }
                }
                .map_err(|_| format!("clip {number} is longer than an hour"))?;
                let made = made
                    .with_speed(clip.speed)
                    .and_then(|c| c.with_placement(placement(clip)))
                    .map_err(|_| format!("clip {number}'s speed or place in the frame can't be used"))?;
                if !made.fits(frame.width, frame.height) {
                    return Err(format!("clip {number} is placed too large for the frame: make it smaller"));
                }
                let made = made
                    .with_fades(clip.fade_in_us, clip.fade_out_us)
                    .and_then(|c| c.with_volume(clip.volume, clip.muted))
                    .and_then(|c| c.with_transition(clip.transition_us))
                    .map_err(|_| format!("clip {number}'s fades or transition are longer than the clip"))?;
                if clip.track == 0 {
                    main.push(made);
                } else {
                    layers.push((made, clip.at_us));
                }
            }
            let spec = ComposeSpec::new(frame.width, frame.height, frame.fps, frame.background, main)
                .map_err(|error| match error {
                    VideoError::Limit => "the timeline is longer than an hour".to_owned(),
                    _ => "a transition is longer than the clips on either side of it allow".to_owned(),
                })?
                .with_layers(layers)
                .map_err(|_| "the timeline is longer than an hour".to_owned())?
                .with_quality(quality);
            let duration_us = spec.duration_us();
            let captions = self.captions(&service, &timeline, frame, duration_us, cancelled)?;
            let spec = spec.with_captions(captions).map_err(|_| "the captions can't be drawn over the picture".to_owned())?;
            self.render(&service, spec, (frame.width, frame.height), cancelled)
        }

        /// `spec` rendered on `service` (waiting for its one render slot), read back and staged
        /// as a `size` WebM: what the Timeline and every video action output.
        pub(in crate::action_exec) fn render(
            &self,
            service: &VideoService,
            spec: ComposeSpec,
            size: (u32, u32),
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            let duration_us = spec.duration_us();
            let rendered = compose(service, spec, cancelled)?;
            self.keep(service, &rendered, size, duration_us, true, cancelled)
        }

        /// A Split part stays unpublished until the whole split succeeds. The receipt is
        /// explicit: concurrent actions on this same executor keep their normal publication.
        pub(in crate::action_exec) fn render_pending(
            &self,
            service: &VideoService,
            spec: ComposeSpec,
            size: (u32, u32),
            cancelled: &dyn Fn() -> bool,
        ) -> Result<(Value, crate::action_exec::video_staging::Provisional<'_>), String> {
            let duration_us = spec.duration_us();
            let rendered = compose(service, spec, cancelled)?;
            let held = self.retain_video_pending(
                rendered.bytes(),
                |offset, length| {
                    wait(service.read_chunk(rendered.asset(), offset, length).map_err(|error| message(closed(error)))?, cancelled).map_err(message)
                },
                VideoChunk::bytes,
                cancelled,
            )?;
            Ok((video_value(held.hash(), size, duration_us, true)?, held))
        }

        /// A rendered file `bytes_len` long (at most [`MAX_RENDER_BYTES`]), read back a chunk at a
        /// time with `read` and staged under its hash. The shared run budget is reserved before
        /// allocating its buffer, including other read-backs still in flight.
        pub(in crate::action_exec) fn read_back(
            &self,
            bytes_len: u64,
            read: &dyn Fn(u64, usize) -> Result<VideoJob<VideoChunk>, VideoError>,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Blake3, String> {
            self.retain_video(
                bytes_len,
                |offset, length| wait(read(offset, length).map_err(|error| message(closed(error)))?, cancelled).map_err(message),
                VideoChunk::bytes,
                cancelled,
            )
        }

        /// `rendered` read back from `service` and staged as a `size` WebM `duration_us` long,
        /// with stereo sound when `sound`.
        pub(in crate::action_exec) fn keep(
            &self,
            service: &VideoService,
            rendered: &RenderedVideo,
            size: (u32, u32),
            duration_us: u64,
            sound: bool,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<Value, String> {
            let bytes_len = rendered.bytes();
            if bytes_len > MAX_RENDER_BYTES {
                return Err(format!("the rendered video is larger than {} MiB; make it shorter or smaller", MAX_RENDER_BYTES / (1024 * 1024)));
            }
            let hash = self.read_back(bytes_len, &|offset, length| service.read_chunk(rendered.asset(), offset, length), cancelled)?;
            video_value(hash, size, duration_us, sound)
        }
    }
}

#[cfg(test)]
mod tests;
