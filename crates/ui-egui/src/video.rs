//! Video playback on cards (#105): a poster frame per content hash, and play, pause and scrub
//! per card.
//!
//! Frames come from `media`'s video service, which runs the ffmpeg and ffprobe the settings name
//! (`[media]`), never ones a project names. Everything here is view state: which card plays, and
//! where; nothing is written to the document. Natively, a video's bytes are read on a thread of
//! their own, imported into the service, and frames are asked for one at a time per card, so a
//! playing card shows the newest frame the service could make, not every frame. Browsers have no
//! video service, so cards say playback needs the desktop app.
use nodecraft_graph::Node;
use nodecraft_types::{AssetRef, Value};

/// A source video's asset, else the first video of the node's active result (inside a list too).
pub fn video_of(node: &Node) -> Option<&AssetRef> {
    if node.kind == "source.video" {
        return node.params.get("asset").and_then(video);
    }
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    result.outputs.iter().find_map(|value| match value {
        Value::List(items) => items.iter().find_map(|item| match item {
            Value::List(inner) => inner.iter().find_map(video),
            single => video(single),
        }),
        single => video(single),
    })
}

fn video(value: &Value) -> Option<&AssetRef> {
    match value {
        Value::Media(asset) if asset.mime.essence().starts_with("video/") => Some(asset),
        _ => None,
    }
}

/// What a card can show for its video right now.
#[derive(Clone, Debug, PartialEq)]
pub enum VideoView {
    /// A frame to show, and where playback is. `position_us` moves while playing.
    Frame { texture: egui::TextureId, size: [u32; 2], position_us: u64, duration_us: u64, playing: bool },
    /// Opening the tools or the video; nothing to show yet.
    Pending,
    /// Why there's no video: not set up, not supported here, or it can't be read.
    Unavailable(&'static str),
}

/// What a card asks of its player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoIntent {
    /// Play, or pause; playing at the end starts over.
    Toggle,
    /// Show the frame at this time (microseconds), playing on from there if playing.
    Seek(u64),
}

/// Shown when no tools are set.
pub const UNCONFIGURED: &str = "Set ffmpeg and ffprobe under [media] in settings to play videos";

/// `m:ss` (or `h:mm:ss`) for a time in microseconds.
pub fn clock(us: u64) -> String {
    let seconds = us / 1_000_000;
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Videos;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{Retry, stale};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod import;
#[cfg(target_arch = "wasm32")]
pub use web::Videos;

#[cfg(target_arch = "wasm32")]
mod web {
    use super::{VideoIntent, VideoView};
    use nodecraft_types::NodeId;

    /// Browsers have no video service yet.
    #[derive(Default)]
    pub struct Videos;

    impl Videos {
        pub fn configure(&mut self, _: Option<(&std::path::Path, &std::path::Path)>) {}
        pub fn poll(&mut self, _: &egui::Context) {}
        pub fn view(
            &mut self,
            _: &egui::Context,
            _: NodeId,
            _: &nodecraft_types::AssetRef,
            _: f32,
            _: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
        ) -> VideoView {
            VideoView::Unavailable("Video playback needs the desktop app")
        }
        pub fn still(
            &mut self,
            _: &egui::Context,
            _: &nodecraft_types::AssetRef,
            _: f32,
            _: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
        ) -> (Option<u64>, Option<(egui::TextureId, [u32; 2])>) {
            (None, None)
        }
        pub fn act(&mut self, _: &egui::Context, _: NodeId, _: VideoIntent) {}
        pub fn retain(&mut self, _: impl Fn(NodeId) -> bool) {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{UNCONFIGURED, VideoIntent, VideoView};
    use egui::{ColorImage, TextureHandle, TextureOptions};
    use nodecraft_format::{PreviewAssetRead, PreviewReadError, asset_source::TransferCancel};
    use nodecraft_media::video::{
        ColorPolicy, FrameRequest, ServiceConfig, ToolPaths, VideoAsset, VideoError, VideoFrame, VideoJob, VideoLimits, VideoOpening, VideoService,
    };
    use nodecraft_types::NodeId;
    use nodecraft_types::{AssetRef, Blake3};
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            mpsc::{Receiver, TryRecvError, sync_channel},
        },
    };

    /// Videos read from the project at once; each may hold up to `media`'s 256 MiB input cap.
    const MAX_READS: usize = 1;
    /// `media`'s cap on one video's bytes; a bigger one is refused for good.
    const MAX_INPUT: usize = 256 * 1024 * 1024;
    /// Imports the service works on at once.
    const MAX_IMPORTS: usize = 2;
    /// Staged video bytes kept imported; the least recently shown go first past this. Below the
    /// service's 1 GiB scratch reservation, so an import isn't refused for space others hold.
    const MAX_STAGED_BYTES: u64 = 768 * 1024 * 1024;
    /// Frames (posters and players) being made at once; the service has two workers.
    const MAX_FRAME_JOBS: usize = 4;
    /// Posters kept; the least recently shown go first past this.
    const MAX_POSTERS: usize = 64;
    /// How often to look for finished work while some is outstanding (jobs don't wake the UI).
    const POLL: std::time::Duration = std::time::Duration::from_millis(30);
    /// How often to look again while only waiting to retry.
    const LATER: std::time::Duration = std::time::Duration::from_secs(1);
    /// Frame spacing assumed when a video doesn't say its rate (24 fps).
    const DEFAULT_STEP_US: u64 = 41_667;
    /// Tries for work that failed for now (the service was full, busy or slow), and seconds
    /// before the first retry; each later one waits a second longer.
    const MAX_TRIES: u8 = 5;
    const RETRY_S: f64 = 1.0;
    /// Times a player finds no frame where the video says there is one before it settles on
    /// the last it showed.
    const MAX_MISSES: u8 = 5;
    /// Frames a card can be off screen before its player lets its texture go.
    const OFFSCREEN_FRAMES: u64 = 600;

    enum Service {
        Off,
        Opening(VideoOpening),
        Ready(VideoService),
        Failed(&'static str),
    }

    enum Source {
        Reading {
            done: Receiver<Result<Arc<[u8]>, &'static str>>,
            cancel: TransferCancel,
        },
        /// Read, waiting for the service to take it (again, after `retry`).
        Read {
            bytes: Arc<[u8]>,
            retry: Option<Retry>,
        },
        Importing {
            job: VideoJob<VideoAsset>,
            bytes: Arc<[u8]>,
            retry: Option<Retry>,
        },
        Ready(VideoAsset),
        Failed(&'static str),
    }

    impl Drop for Source {
        fn drop(&mut self) {
            if let Source::Reading { cancel, .. } = self {
                cancel.cancel();
            }
        }
    }

    struct SourceEntry {
        state: Source,
        bytes: u64,
        used: u64,
    }

    enum Poster {
        Making {
            job: VideoJob<VideoFrame>,
            retry: Option<Retry>,
        },
        Ready {
            texture: TextureHandle,
            size: [u32; 2],
        },
        /// Failed for now; tried again when due.
        Waiting(Retry),
        Failed,
    }

    struct PosterEntry {
        state: Poster,
        used: u64,
    }

    struct Shown {
        texture: TextureHandle,
        size: [u32; 2],
        time_us: u64,
    }

    struct Player {
        hash: Blake3,
        playing: bool,
        position_us: u64,
        /// While playing: the clock time playback (re)started at, and the position then.
        anchor: (f64, u64),
        /// The frame asked for, and when.
        job: Option<(u64, VideoJob<VideoFrame>)>,
        /// The frame on screen, once the player has made one; the poster until then.
        shown: Option<Shown>,
        /// The bounding box frames are asked for at.
        edge: u32,
        seen: u64,
        /// Where the video turned out to have no more frames, and how often that happened.
        end: Option<u64>,
        misses: u8,
        retry: Option<Retry>,
        error: Option<&'static str>,
    }

    impl Player {
        fn new(hash: Blake3, edge: u32, seen: u64) -> Self {
            Self { hash, playing: false, position_us: 0, anchor: (0.0, 0), job: None, shown: None, edge, seen, end: None, misses: 0, retry: None, error: None }
        }
    }

    /// When to try failed work again.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub(crate) struct Retry {
        at: f64,
        tries: u8,
    }

    impl Retry {
        /// After `error` at `now`, following `previous` tries: when to try again, or `None` to
        /// give up (the error won't pass, or it has been tried enough).
        pub(crate) fn after(previous: Option<Retry>, error: VideoError, now: f64) -> Option<Retry> {
            let tries = previous.map_or(0, |r| r.tries).saturating_add(1);
            (transient(error) && tries < MAX_TRIES).then(|| Retry { at: now + RETRY_S * f64::from(tries), tries })
        }

        pub(crate) fn due(self, now: f64) -> bool {
            now >= self.at
        }
    }

    /// Whether `error` may pass when tried again later.
    pub(super) fn transient(error: VideoError) -> bool {
        matches!(error, VideoError::Limit | VideoError::Busy | VideoError::Timeout | VideoError::Io | VideoError::Cancelled)
    }

    /// Neither shown this frame nor the last, so it can go.
    pub(crate) fn stale(used: u64, clock: u64) -> bool {
        clock.wrapping_sub(used) > 1
    }

    /// The frame time to ask for at `position_us`: the start of its frame, and no later than
    /// the last frame, which starts a step before the end (or before `end`, where the video
    /// turned out to have none). `None` when there's no frame at all.
    pub(super) fn wanted_us(position_us: u64, duration_us: u64, step_us: u64, end: Option<u64>) -> Option<u64> {
        let step = step_us.max(1);
        let limit = end.map_or(duration_us, |end| end.min(duration_us));
        if end.is_some_and(|end| end == 0) {
            return None;
        }
        let last = limit.saturating_sub(step);
        Some(position_us.min(last) / step * step)
    }

    /// What's still outstanding after a poll.
    #[derive(Default)]
    struct Wake {
        /// Work that finishes on its own: look again shortly.
        soon: bool,
        /// Only retries: look again in a while.
        later: bool,
    }

    /// Video sources, posters and players. One per app.
    pub struct Videos {
        service: Service,
        tools: Option<(std::path::PathBuf, std::path::PathBuf)>,
        sources: HashMap<Blake3, SourceEntry>,
        posters: HashMap<(Blake3, u32), PosterEntry>,
        players: HashMap<NodeId, Player>,
        clock: u64,
        now: f64,
    }

    impl Default for Videos {
        fn default() -> Self {
            Self { service: Service::Off, tools: None, sources: HashMap::new(), posters: HashMap::new(), players: HashMap::new(), clock: 0, now: 0.0 }
        }
    }

    fn message(error: VideoError) -> &'static str {
        match error {
            VideoError::Unsupported | VideoError::InvalidMetadata => "This video's format isn't supported",
            VideoError::Limit => "This video is too large to play here",
            VideoError::Unavailable => "Can't run ffmpeg or ffprobe; check [media] in settings",
            VideoError::Timeout => "This video took too long to load",
            _ => "Can't play this video",
        }
    }

    fn read_message(error: PreviewReadError) -> &'static str {
        match error {
            PreviewReadError::Missing => "This video's data is missing",
            PreviewReadError::TooLarge => "This video is too large to play here",
            PreviewReadError::Corrupt => "Can't play this video",
            _ => "This video can't be read",
        }
    }

    fn texture_image(frame: &VideoFrame) -> Option<ColorImage> {
        let size = [usize::try_from(frame.width).ok()?, usize::try_from(frame.height).ok()?];
        (size[0].checked_mul(size[1])?.checked_mul(4)? == frame.pixels().len()).then(|| ColorImage::from_rgba_unmultiplied(size, frame.pixels()))
    }

    /// Microseconds between frames: the video's own rate, within reason.
    fn step_us(asset: &VideoAsset) -> u64 {
        asset.metadata().frame_rate.and_then(|(num, den)| den.checked_mul(1_000_000)?.checked_div(num)).unwrap_or(DEFAULT_STEP_US).clamp(1_000, 1_000_000)
    }

    impl Videos {
        /// Reuses the configured playback service for best-effort import metadata. Never waits.
        pub(crate) fn probe_import(
            &mut self,
            ctx: &egui::Context,
            bytes: std::sync::Arc<[u8]>,
        ) -> Option<Result<nodecraft_media::video::VideoJob<VideoAsset>, ()>> {
            if self.open(ctx).is_err() {
                return Some(Err(()));
            }
            self.service().map(|service| service.import(bytes, ColorPolicy::default()).map_err(|_| ()))
        }

        /// Uses these tools from now on: the service is opened when a video is first shown.
        /// `None` (either unset) leaves videos unplayable, and says how to set them.
        pub fn configure(&mut self, tools: Option<(&std::path::Path, &std::path::Path)>) {
            let tools = tools.map(|(a, b)| (a.to_path_buf(), b.to_path_buf()));
            if tools != self.tools {
                *self = Self { tools, ..Self::default() };
            }
        }

        fn service(&self) -> Option<&VideoService> {
            match &self.service {
                Service::Ready(service) => Some(service),
                _ => None,
            }
        }

        fn frame_jobs(&self) -> usize {
            let posters = self.posters.values().filter(|p| matches!(p.state, Poster::Making { .. })).count();
            posters.saturating_add(self.players.values().filter(|p| p.job.is_some()).count())
        }

        /// Takes finished work and moves playing cards along; call once per frame.
        pub fn poll(&mut self, ctx: &egui::Context) {
            self.clock = self.clock.wrapping_add(1);
            self.now = ctx.input(|i| i.time);
            let mut wake = Wake::default();
            if let Service::Opening(opening) = &mut self.service {
                let failed = match opening.try_result() {
                    Ok(Some(service)) => {
                        self.service = Service::Ready(service);
                        None
                    }
                    Ok(None) => {
                        wake.soon = true;
                        None
                    }
                    Err(VideoError::Unavailable | VideoError::Unsupported | VideoError::Process) => {
                        Some("The ffmpeg or ffprobe in [media] settings can't play videos")
                    }
                    Err(error) => Some(message(error)),
                };
                if let Some(why) = failed {
                    // Nothing can be imported now: let the read videos go.
                    self.service = Service::Failed(why);
                    self.sources.clear();
                }
            }
            self.poll_sources(&mut wake);
            self.poll_posters(ctx, &mut wake);
            self.poll_players(ctx, &mut wake);
            if wake.soon {
                ctx.request_repaint_after(POLL);
            } else if wake.later {
                ctx.request_repaint_after(LATER);
            }
        }

        fn poll_sources(&mut self, wake: &mut Wake) {
            let now = self.now;
            let mut refused = None;
            let mut imports = self.sources.values().filter(|s| matches!(s.state, Source::Importing { .. })).count();
            for (hash, entry) in &mut self.sources {
                let next = match &mut entry.state {
                    Source::Reading { done, .. } => {
                        wake.soon = true;
                        match done.try_recv() {
                            Ok(Ok(bytes)) => Some(Source::Read { bytes, retry: None }),
                            Ok(Err(why)) => Some(Source::Failed(why)),
                            Err(TryRecvError::Empty) => None,
                            Err(TryRecvError::Disconnected) => Some(Source::Failed("This video can't be read")),
                        }
                    }
                    Source::Read { retry: Some(retry), .. } if !retry.due(now) => {
                        wake.later = true;
                        None
                    }
                    Source::Read { bytes, .. } if bytes.len() > MAX_INPUT => Some(Source::Failed(message(VideoError::Limit))),
                    Source::Read { bytes, retry } => {
                        wake.soon = true;
                        let Service::Ready(service) = &self.service else {
                            continue;
                        };
                        if imports >= MAX_IMPORTS {
                            continue;
                        }
                        match service.import(Arc::clone(bytes), ColorPolicy::default()) {
                            Ok(job) => {
                                imports = imports.saturating_add(1);
                                Some(Source::Importing { job, bytes: Arc::clone(bytes), retry: *retry })
                            }
                            // Kept read; tried again on a later frame.
                            Err((VideoError::Busy, _)) => None,
                            Err((error, _)) => {
                                if error == VideoError::Limit {
                                    refused = Some(*hash);
                                }
                                Some(match Retry::after(*retry, error, now) {
                                    Some(retry) => Source::Read { bytes: Arc::clone(bytes), retry: Some(retry) },
                                    None => Source::Failed(message(error)),
                                })
                            }
                        }
                    }
                    Source::Importing { job, bytes, retry } => match job.try_result() {
                        Ok(Some(asset)) => {
                            imports = imports.saturating_sub(1);
                            Some(Source::Ready(asset))
                        }
                        Ok(None) => {
                            wake.soon = true;
                            None
                        }
                        Err(error) => {
                            imports = imports.saturating_sub(1);
                            if error == VideoError::Limit {
                                refused = Some(*hash);
                            }
                            Some(match Retry::after(*retry, error, now) {
                                Some(retry) => Source::Read { bytes: Arc::clone(bytes), retry: Some(retry) },
                                None => Source::Failed(message(error)),
                            })
                        }
                    },
                    Source::Ready(_) | Source::Failed(_) => None,
                };
                if let Some(next) = next {
                    entry.state = next;
                }
            }
            // The service is full: make room by letting the least recently shown imported video
            // go, if there's one that isn't on screen.
            if let Some(hash) = refused {
                self.evict_source(Some(hash));
            }
            // A video that failed is read again once it's back on screen after a while.
            let clock = self.clock;
            self.sources.retain(|_, entry| !matches!(entry.state, Source::Failed(_)) || clock.wrapping_sub(entry.used) <= OFFSCREEN_FRAMES);
        }

        /// Drops the least recently shown imported video other than `keep` and not on screen;
        /// `false` when there's none.
        fn evict_source(&mut self, keep: Option<Blake3>) -> bool {
            let clock = self.clock;
            let oldest = self
                .sources
                .iter()
                .filter(|(hash, entry)| Some(**hash) != keep && stale(entry.used, clock) && matches!(entry.state, Source::Ready(_) | Source::Failed(_)))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(hash, _)| *hash);
            oldest.is_some_and(|hash| self.sources.remove(&hash).is_some())
        }

        fn poll_posters(&mut self, ctx: &egui::Context, wake: &mut Wake) {
            let now = self.now;
            for ((hash, edge), entry) in &mut self.posters {
                match &mut entry.state {
                    Poster::Making { job, retry } => match job.try_result() {
                        Ok(Some(frame)) => {
                            entry.state = match texture_image(&frame) {
                                Some(image) => {
                                    let texture = ctx.load_texture(format!("video-poster-{hash:?}-{edge}"), image, TextureOptions::LINEAR);
                                    Poster::Ready { texture, size: [frame.width, frame.height] }
                                }
                                None => Poster::Failed,
                            }
                        }
                        Ok(None) => wake.soon = true,
                        Err(error) => entry.state = Retry::after(*retry, error, now).map_or(Poster::Failed, Poster::Waiting),
                    },
                    Poster::Waiting(_) => wake.later = true,
                    Poster::Ready { .. } | Poster::Failed => {}
                }
            }
            let clock = self.clock;
            while self.posters.len() > MAX_POSTERS {
                let oldest = self
                    .posters
                    .iter()
                    .filter(|(_, entry)| stale(entry.used, clock) && !matches!(entry.state, Poster::Making { .. }))
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(key, _)| *key);
                let Some(key) = oldest else {
                    break;
                };
                self.posters.remove(&key);
            }
        }

        fn poll_players(&mut self, ctx: &egui::Context, wake: &mut Wake) {
            let mut jobs = self.frame_jobs();
            let now = self.now;
            let clock = self.clock;
            let seen = clock.wrapping_sub(1);
            for (id, player) in &mut self.players {
                let asset = match self.sources.get(&player.hash).map(|entry| &entry.state) {
                    Some(Source::Ready(asset)) => Some(asset),
                    _ => None,
                };
                let duration = asset.map_or(0, |asset| asset.metadata().duration_us);
                let step = asset.map_or(DEFAULT_STEP_US, step_us);
                let on_screen = player.seen == seen;
                if player.playing {
                    let elapsed = (now - player.anchor.0).max(0.0) * 1_000_000.0;
                    let elapsed = if elapsed.is_finite() { elapsed.min(u64::MAX as f64) as u64 } else { 0 };
                    player.position_us = player.anchor.1.saturating_add(elapsed).min(duration);
                    // Off screen it plays on by the clock, with nothing to draw until it's back.
                    wake.soon |= on_screen;
                }
                if let Some((time, job)) = &mut player.job {
                    let time = *time;
                    let done = match job.try_result() {
                        Ok(Some(frame)) => {
                            match texture_image(&frame) {
                                Some(image) => {
                                    let size = [frame.width, frame.height];
                                    match &mut player.shown {
                                        Some(shown) => {
                                            shown.texture.set(image, TextureOptions::LINEAR);
                                            shown.size = size;
                                            shown.time_us = time;
                                        }
                                        None => {
                                            let texture = ctx.load_texture(format!("video-player-{id:?}"), image, TextureOptions::LINEAR);
                                            player.shown = Some(Shown { texture, size, time_us: time });
                                        }
                                    }
                                    player.retry = None;
                                }
                                None => player.error = Some(message(VideoError::Invalid)),
                            }
                            true
                        }
                        Ok(None) => {
                            wake.soon = true;
                            false
                        }
                        // No frame there, though the video's length says there should be (its
                        // video ends before its audio, say): its frames end there. After a few
                        // misses, at the last frame shown, rather than looking further back.
                        Err(VideoError::NoFrame) => {
                            player.misses = player.misses.saturating_add(1);
                            let end =
                                if player.misses >= MAX_MISSES { player.shown.as_ref().map_or(0, |s| s.time_us.saturating_add(step)).min(time) } else { time };
                            player.end = Some(player.end.map_or(end, |known| known.min(end)));
                            true
                        }
                        Err(error) => {
                            player.retry = Retry::after(player.retry, error, now);
                            if player.retry.is_none() {
                                player.error = Some(message(error));
                                player.playing = false;
                            }
                            true
                        }
                    };
                    if done {
                        player.job = None;
                        jobs = jobs.saturating_sub(1);
                    }
                }
                if player.playing && player.position_us >= duration {
                    player.playing = false;
                }
                if player.job.is_none() && clock.wrapping_sub(player.seen) > OFFSCREEN_FRAMES {
                    player.shown = None;
                }
                // Only cards on screen ask for frames.
                let (Some(asset), Some(service)) = (
                    asset,
                    match &self.service {
                        Service::Ready(service) => Some(service),
                        _ => None,
                    },
                ) else {
                    continue;
                };
                if player.job.is_some() || !on_screen || player.error.is_some() || jobs >= MAX_FRAME_JOBS {
                    continue;
                }
                if let Some(retry) = player.retry
                    && !retry.due(now)
                {
                    wake.later = true;
                    continue;
                }
                let Some(wanted) = wanted_us(player.position_us, duration, step, player.end) else {
                    continue;
                };
                let shown = player.shown.as_ref().map(|s| s.time_us);
                // Paused at the start with nothing of its own yet: the poster will do.
                if shown == Some(wanted) || (shown.is_none() && wanted == 0 && !player.playing) {
                    continue;
                }
                let job = FrameRequest::new(wanted, player.edge, player.edge).and_then(|request| service.frame(asset, request));
                match job {
                    Ok(job) => {
                        player.job = Some((wanted, job));
                        jobs = jobs.saturating_add(1);
                        wake.soon = true;
                    }
                    Err(VideoError::Busy) => wake.soon = true,
                    Err(error) => {
                        player.retry = Retry::after(player.retry, error, now);
                        if player.retry.is_none() {
                            player.error = Some(message(error));
                        }
                        wake.later = true;
                    }
                }
            }
        }

        /// What node `id`'s card shows for `asset`, at about `edge` pixels. `read` captures the
        /// asset for reading, and is only called when the video is first needed.
        pub fn view(
            &mut self,
            ctx: &egui::Context,
            id: NodeId,
            asset: &AssetRef,
            edge: f32,
            read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
        ) -> VideoView {
            if let Err(view) = self.open(ctx) {
                return view;
            }
            let hash = asset.hash;
            let edge = crate::preview::bucket(edge);
            let clock = self.clock;
            let player = self.players.entry(id).or_insert_with(|| Player::new(hash, edge, clock));
            if player.hash != hash {
                // The card shows another video now (a new result): start again.
                *player = Player::new(hash, edge, clock);
            }
            player.seen = clock;
            player.edge = edge;
            if let Some(why) = player.error {
                return VideoView::Unavailable(why);
            }
            let (playing, position_us) = (player.playing, player.position_us);
            let shown = player.shown.as_ref().map(|s| (s.texture.id(), s.size));
            let state = self.source(ctx, hash, read);
            let asset = match state {
                Ok(asset) => asset,
                Err(view) => return view,
            };
            let duration_us = asset.metadata().duration_us;
            let poster = self.poster(hash, edge);
            match shown.or(poster) {
                Some((texture, size)) => VideoView::Frame { texture, size, position_us, duration_us, playing },
                None if matches!(self.posters.get(&(hash, edge)).map(|p| &p.state), Some(Poster::Failed)) => VideoView::Unavailable("Can't play this video"),
                None => VideoView::Pending,
            }
        }

        /// Starts the service the first time a video is wanted; what to show while it can't.
        fn open(&mut self, ctx: &egui::Context) -> Result<(), VideoView> {
            let Some((ffmpeg, ffprobe)) = &self.tools else {
                return Err(VideoView::Unavailable(UNCONFIGURED));
            };
            if matches!(self.service, Service::Off) {
                let config =
                    ToolPaths::new(ffmpeg.clone(), ffprobe.clone()).and_then(|tools| ServiceConfig::new(tools, std::env::temp_dir(), VideoLimits::default()));
                self.service = match config.and_then(VideoService::begin_open) {
                    Ok(opening) => Service::Opening(opening),
                    Err(error) => Service::Failed(message(error)),
                };
                ctx.request_repaint_after(POLL);
            }
            match self.service {
                Service::Failed(why) => Err(VideoView::Unavailable(why)),
                _ => Ok(()),
            }
        }

        /// A video's length in microseconds and its first frame at about `edge` pixels, each once
        /// known, without a card playing it: for editors that lay videos out. `read` is as for
        /// `view`.
        pub fn still(
            &mut self,
            ctx: &egui::Context,
            asset: &AssetRef,
            edge: f32,
            read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
        ) -> (Option<u64>, Option<(egui::TextureId, [u32; 2])>) {
            if self.open(ctx).is_err() {
                return (None, None);
            }
            let Ok(video) = self.source(ctx, asset.hash, read) else {
                return (None, None);
            };
            (Some(video.metadata().duration_us), self.poster(asset.hash, crate::preview::bucket(edge)))
        }

        /// The imported video `hash`, or what to show instead; starts reading it when it's new.
        fn source(
            &mut self,
            ctx: &egui::Context,
            hash: Blake3,
            read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
        ) -> Result<VideoAsset, VideoView> {
            let clock = self.clock;
            if !self.sources.contains_key(&hash) {
                let reading = self.sources.values().filter(|s| matches!(s.state, Source::Reading { .. })).count();
                if reading >= MAX_READS {
                    return Err(VideoView::Pending);
                }
                let (state, bytes) = match read() {
                    Ok(input) if input.hash() == hash => {
                        let bytes = input.byte_len();
                        (start_read(ctx, input), bytes)
                    }
                    Ok(_) => (Source::Failed("Can't play this video"), 0),
                    Err(why) => (Source::Failed(read_message(why)), 0),
                };
                // Room for it among the imported videos.
                let staged = |sources: &HashMap<Blake3, SourceEntry>| sources.values().map(|s| s.bytes).fold(0u64, u64::saturating_add);
                while staged(&self.sources).saturating_add(bytes) > MAX_STAGED_BYTES && self.evict_source(None) {}
                self.sources.insert(hash, SourceEntry { state, bytes, used: clock });
            }
            let entry = self.sources.get_mut(&hash).ok_or(VideoView::Pending)?;
            entry.used = clock;
            match &entry.state {
                Source::Ready(asset) => Ok(asset.clone()),
                Source::Failed(why) => Err(VideoView::Unavailable(why)),
                _ => Err(VideoView::Pending),
            }
        }

        /// The first frame of video `hash` at `edge`, starting it when there's room.
        fn poster(&mut self, hash: Blake3, edge: u32) -> Option<(egui::TextureId, [u32; 2])> {
            let (clock, now) = (self.clock, self.now);
            if let Some(entry) = self.posters.get_mut(&(hash, edge)) {
                entry.used = clock;
                if let Poster::Ready { texture, size } = &entry.state {
                    return Some((texture.id(), *size));
                }
            }
            let start = match self.posters.get(&(hash, edge)).map(|p| &p.state) {
                None => Some(None),
                Some(Poster::Waiting(retry)) if retry.due(now) => Some(Some(*retry)),
                _ => None,
            };
            if let Some(retry) = start
                && self.frame_jobs() < MAX_FRAME_JOBS
                && let (Some(service), Some(SourceEntry { state: Source::Ready(asset), .. })) = (self.service(), self.sources.get(&hash))
            {
                let state = match FrameRequest::new(0, edge, edge).and_then(|request| service.frame(asset, request)) {
                    Ok(job) => Some(Poster::Making { job, retry }),
                    Err(VideoError::Busy) => None,
                    Err(error) => Some(Retry::after(retry, error, now).map_or(Poster::Failed, Poster::Waiting)),
                };
                if let Some(state) = state {
                    self.posters.insert((hash, edge), PosterEntry { state, used: clock });
                }
            }
            // While a sharper one is made, any other size of the same video will do.
            self.posters.iter_mut().find_map(|((h, _), entry)| match &entry.state {
                Poster::Ready { texture, size } if *h == hash => {
                    entry.used = clock;
                    Some((texture.id(), *size))
                }
                _ => None,
            })
        }

        /// Plays, pauses or seeks node `id`'s video.
        pub fn act(&mut self, ctx: &egui::Context, id: NodeId, intent: VideoIntent) {
            let Some(player) = self.players.get_mut(&id) else {
                return;
            };
            let duration = match self.sources.get(&player.hash).map(|entry| &entry.state) {
                Some(Source::Ready(asset)) => asset.metadata().duration_us,
                _ => return,
            };
            let now = ctx.input(|i| i.time);
            match intent {
                VideoIntent::Toggle if player.playing => player.playing = false,
                VideoIntent::Toggle => {
                    if player.position_us >= duration {
                        player.position_us = 0;
                    }
                    player.playing = true;
                    player.anchor = (now, player.position_us);
                }
                VideoIntent::Seek(us) => {
                    player.position_us = us.min(duration);
                    player.anchor = (now, player.position_us);
                }
            }
            ctx.request_repaint();
        }

        /// Forgets the players of nodes `keep` says are gone.
        pub fn retain(&mut self, keep: impl Fn(NodeId) -> bool) {
            self.players.retain(|id, _| keep(*id));
        }

        #[cfg(test)]
        pub(crate) fn test_counts(&self) -> (usize, usize, usize) {
            (self.sources.len(), self.posters.len(), self.players.len())
        }
    }

    /// Reads `input` on a thread of its own; the source says when it's done.
    fn start_read(ctx: &egui::Context, input: PreviewAssetRead) -> Source {
        let (send, done) = sync_channel(1);
        let cancel = TransferCancel::default();
        let active = cancel.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("nodecraft-video-read".into()).spawn(move || {
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.read_verified(&active, &Default::default())));
            let result = match read {
                Ok(Ok(bytes)) => Ok(Arc::<[u8]>::from(bytes)),
                Ok(Err(why)) => Err(read_message(why)),
                Err(_) => Err("This video can't be read"),
            };
            let _ = send.send(result);
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => Source::Reading { done, cancel },
            Err(_) => Source::Failed("This video can't be read"),
        }
    }
}

#[cfg(test)]
mod tests;
