//! The Timeline's executor (#109): imports each connected video into the video service, checks
//! the clips against the videos' lengths, renders them into one WebM (VP9 picture, Opus sound)
//! and stages it. The tools are the ones the settings name (`[media]`), never a project's.
//! Natively only: browsers have no video service.
use super::{LocalActions, whole, word};
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

/// Each input's origin node, as planned (empty where it isn't known).
pub(crate) fn sources(params: &ParamValues) -> Vec<String> {
    match params.get(nodecraft_nodes::timeline::SOURCES_PARAM) {
        Some(Value::List(items)) => items.iter().map(|v| if let Value::Text(t) = v { t.clone() } else { String::new() }).collect(),
        _ => Vec::new(),
    }
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
fn seconds(us: u64) -> String {
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
mod native {
    use super::{LocalActions, MAX_INPUT_BYTES, MAX_RENDER_BYTES, clips, frame, seconds, sources};
    use nodecraft_format::asset_source::TransferCancel;
    use nodecraft_media::video::{
        ColorPolicy, ComposeClip, ComposeSpec, ServiceConfig, ToolPaths, VideoAsset, VideoError, VideoJob, VideoLimits, VideoService,
    };
    use nodecraft_types::{AssetRef, Blake3, MediaMeta, ParamValues, Value};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    type Tools = (PathBuf, PathBuf);

    /// The service renders share, for the tools it was opened with.
    static SERVICE: Mutex<Option<(Tools, Arc<VideoService>)>> = Mutex::new(None);
    /// How often a wait looks for a result or a cancel.
    const POLL: Duration = Duration::from_millis(10);
    /// Longest wait for another render to free the service's one render slot.
    const BUSY_WAIT: Duration = Duration::from_secs(3600);
    /// Bytes read back from the rendered file at once (the service's most).
    const CHUNK: u64 = 1024 * 1024;

    fn message(error: VideoError) -> String {
        match error {
            VideoError::Unavailable => "can't run ffmpeg or ffprobe; check [media] in settings",
            VideoError::Unsupported => "this ffmpeg can't render the timeline (it needs VP9, Opus and the xfade filter), or a video's format isn't supported",
            VideoError::InvalidMetadata => "a connected video's format isn't supported",
            VideoError::Limit => "the videos are too large to render together",
            VideoError::Timeout => "the render took too long",
            VideoError::Cancelled => "the render was cancelled",
            VideoError::Busy => "another video is rendering; try again when it's done",
            VideoError::Invalid => "the timeline can't be rendered as set",
            _ => "the render failed",
        }
        .to_owned()
    }

    /// The shared service for `tools`, opened on first use (or when the tools change).
    fn service(tools: &Tools, cancelled: &dyn Fn() -> bool) -> Result<Arc<VideoService>, String> {
        let mut slot = SERVICE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((open, service)) = slot.as_ref()
            && open == tools
        {
            return Ok(Arc::clone(service));
        }
        *slot = None;
        let config = ToolPaths::new(tools.0.clone(), tools.1.clone())
            .and_then(|paths| ServiceConfig::new(paths, std::env::temp_dir(), VideoLimits::default()))
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
    fn closed(error: VideoError) -> VideoError {
        if matches!(error, VideoError::Closed) {
            *SERVICE.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        error
    }

    /// `job`'s result; cancels the job once `cancelled` says so. The service bounds every job by
    /// a deadline of its own, so this ends.
    fn wait<T>(mut job: VideoJob<T>, cancelled: &dyn Fn() -> bool) -> Result<T, VideoError> {
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

    impl LocalActions {
        /// The bytes of the video `value` names: one an earlier action in this run made, or the
        /// project's.
        fn video_bytes(&self, value: Option<&Value>) -> Result<Arc<[u8]>, String> {
            let Some(Value::Media(source)) = value.filter(|v| matches!(v, Value::Media(a) if a.mime.essence().starts_with("video/"))) else {
                return Err("connect a video to the timeline first".into());
            };
            if let Some(bytes) = self.videos.lock().unwrap_or_else(PoisonError::into_inner).get(&source.hash) {
                return Ok(Arc::clone(bytes));
            }
            if let Some(bytes) = self.assets.get(&source.hash) {
                return Ok(Arc::from(bytes));
            }
            let input = self.assets.preview_read(source).map_err(|error| match error {
                nodecraft_format::PreviewReadError::Missing => "a connected video isn't in this project",
                _ => "a connected video can't be read",
            })?;
            if input.byte_len() > MAX_INPUT_BYTES {
                return Err(format!("a connected video is larger than {} MiB", MAX_INPUT_BYTES / (1024 * 1024)));
            }
            let bytes = input.read_verified(&TransferCancel::default(), &Default::default()).map_err(|_| "a connected video can't be read")?;
            Ok(Arc::from(bytes))
        }

        pub(in crate::action_exec) fn timeline(&self, params: &ParamValues, inputs: &[Value], cancelled: &dyn Fn() -> bool) -> Result<Value, String> {
            let frame = frame(params)?;
            let mut sources = sources(params);
            sources.resize(inputs.len(), String::new());
            let timeline = clips(params)?;
            let listed = timeline.with_sources(&sources);
            let resolved = timeline.resolve(&sources);
            if resolved.is_empty() {
                return Err("the timeline has no clips to render: connect a video, or enable a clip".into());
            }
            let tools = self.video.as_ref().ok_or("set ffmpeg and ffprobe under [media] in settings to render timelines")?;
            let service = service(tools, cancelled)?;
            // One video at a time: its bytes are dropped once the service holds its own copy.
            let mut imported: Vec<Option<VideoAsset>> = vec![None; inputs.len()];
            for (input, _) in &resolved {
                let Some(slot) = imported.get_mut(*input) else { return Err("a clip names a video that isn't connected".into()) };
                if slot.is_some() {
                    continue;
                }
                let bytes = self.video_bytes(inputs.get(*input))?;
                let job = service.import(bytes, ColorPolicy::default()).map_err(|(error, _)| message(closed(error)))?;
                *slot = Some(wait(job, cancelled).map_err(message)?);
            }
            let mut composed = Vec::with_capacity(resolved.len());
            for (n, (input, clip)) in resolved.iter().enumerate() {
                // Numbered as the editor lists them, disabled clips included.
                let number = listed.clips.iter().position(|c| c.id == clip.id).unwrap_or(n).saturating_add(1);
                let asset = imported.get(*input).and_then(Option::as_ref).ok_or("a clip names a video that isn't connected")?;
                let duration = asset.metadata().duration_us;
                let out = clip.out_us.unwrap_or(duration);
                if out > duration {
                    return Err(format!("clip {number} ends after its video does ({})", seconds(duration)));
                }
                if clip.in_us >= out {
                    return Err(format!("clip {number} starts after its video ends ({})", seconds(duration)));
                }
                let made = ComposeClip::new(asset.clone(), clip.in_us, out)
                    .and_then(|c| c.with_fades(clip.fade_in_us, clip.fade_out_us))
                    .and_then(|c| c.with_volume(clip.volume, clip.muted))
                    .and_then(|c| c.with_transition(clip.transition_us))
                    .map_err(|_| format!("clip {number}'s fades or transition are longer than the clip"))?;
                composed.push(made);
            }
            let spec = ComposeSpec::new(frame.width, frame.height, frame.fps, frame.background, composed).map_err(|error| match error {
                VideoError::Limit => "the timeline is longer than an hour".to_owned(),
                _ => "a transition is longer than the clips on either side of it allow".to_owned(),
            })?;
            let duration_us = spec.duration_us();
            let started = Instant::now();
            let job = loop {
                match service.compose(spec.clone()) {
                    Err(VideoError::Busy) if started.elapsed() < BUSY_WAIT && !cancelled() => std::thread::sleep(POLL.saturating_mul(10)),
                    other => break other.map_err(|error| message(closed(error)))?,
                }
            };
            let rendered = wait(job, cancelled).map_err(message)?;
            let size = rendered.bytes();
            if size > MAX_RENDER_BYTES {
                return Err(format!("the rendered video is larger than {} MiB; make the timeline shorter or smaller", MAX_RENDER_BYTES / (1024 * 1024)));
            }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(usize::try_from(size).map_err(|_| "the rendered video is too large")?).map_err(|_| "the rendered video is too large")?;
            while (bytes.len() as u64) < size {
                let length = usize::try_from(CHUNK.min(size.saturating_sub(bytes.len() as u64))).map_err(|_| "the rendered video is too large")?;
                let chunk = wait(service.read_chunk(rendered.asset(), bytes.len() as u64, length).map_err(message)?, cancelled).map_err(message)?;
                bytes.extend_from_slice(chunk.bytes());
            }
            let hash = Blake3::from_bytes(*blake3::hash(&bytes).as_bytes());
            self.videos.lock().unwrap_or_else(PoisonError::into_inner).insert(hash, Arc::from(bytes));
            let mime = "video/webm".parse().map_err(|_| "video/webm isn't a media type".to_owned())?;
            let meta = MediaMeta {
                width: Some(frame.width),
                height: Some(frame.height),
                duration_ms: Some(duration_us / 1000),
                sample_rate: Some(48_000),
                channels: Some(2),
                ..MediaMeta::default()
            };
            Ok(Value::Media(AssetRef { hash, mime, meta }))
        }
    }
}

#[cfg(test)]
mod tests;
