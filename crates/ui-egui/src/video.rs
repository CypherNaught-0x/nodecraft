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
    /// Imports the service works on at once.
    const MAX_IMPORTS: usize = 2;
    /// Staged video bytes kept imported; the least recently shown go first past this. Below the
    /// service's 1 GiB scratch reservation, so an import isn't refused for space others hold.
    const MAX_STAGED_BYTES: u64 = 768 * 1024 * 1024;
    /// Frames (posters and players) being made at once; the service has two workers.
    const MAX_FRAME_JOBS: usize = 4;
    /// Posters kept as textures; the least recently shown go first past this.
    const MAX_POSTERS: usize = 64;
    /// How often to look for finished work while some is outstanding (jobs don't wake the UI).
    const POLL: std::time::Duration = std::time::Duration::from_millis(30);
    /// Frame spacing assumed when a video doesn't say its rate (24 fps).
    const DEFAULT_STEP_US: u64 = 41_667;

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
        /// Read, waiting for the service to take it.
        Read(Arc<[u8]>),
        Importing(VideoJob<VideoAsset>),
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
        Making(VideoJob<VideoFrame>),
        Ready { texture: TextureHandle, size: [u32; 2], used: u64 },
        Failed,
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
        error: Option<&'static str>,
    }

    /// Video sources, posters and players. One per app.
    pub struct Videos {
        service: Service,
        tools: Option<(std::path::PathBuf, std::path::PathBuf)>,
        sources: HashMap<Blake3, SourceEntry>,
        posters: HashMap<(Blake3, u32), Poster>,
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
            let posters = self.posters.values().filter(|p| matches!(p, Poster::Making(_))).count();
            posters.saturating_add(self.players.values().filter(|p| p.job.is_some()).count())
        }

        /// Takes finished work and moves playing cards along; call once per frame.
        pub fn poll(&mut self, ctx: &egui::Context) {
            self.clock = self.clock.wrapping_add(1);
            self.now = ctx.input(|i| i.time);
            let mut waiting = false;
            if let Service::Opening(opening) = &mut self.service {
                match opening.try_result() {
                    Ok(Some(service)) => self.service = Service::Ready(service),
                    Ok(None) => waiting = true,
                    Err(VideoError::Unavailable | VideoError::Unsupported | VideoError::Process) => {
                        self.service = Service::Failed("The ffmpeg or ffprobe in [media] settings can't play videos")
                    }
                    Err(error) => self.service = Service::Failed(message(error)),
                }
            }
            waiting |= self.poll_sources();
            waiting |= self.poll_posters(ctx);
            waiting |= self.poll_players(ctx);
            if waiting {
                ctx.request_repaint_after(POLL);
            }
        }

        fn poll_sources(&mut self) -> bool {
            let mut waiting = false;
            let mut refused = None;
            let mut imports = self.sources.values().filter(|s| matches!(s.state, Source::Importing(_))).count();
            for (hash, entry) in &mut self.sources {
                let next = match &mut entry.state {
                    Source::Reading { done, .. } => match done.try_recv() {
                        Ok(Ok(bytes)) => Some(Source::Read(bytes)),
                        Ok(Err(why)) => Some(Source::Failed(why)),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => Some(Source::Failed("This video can't be read")),
                    },
                    Source::Read(bytes) if imports < MAX_IMPORTS => {
                        let Service::Ready(service) = &self.service else {
                            waiting = true;
                            continue;
                        };
                        match service.import(Arc::clone(bytes), ColorPolicy::default()) {
                            Ok(job) => {
                                imports = imports.saturating_add(1);
                                Some(Source::Importing(job))
                            }
                            // Kept read; tried again on a later frame.
                            Err((VideoError::Busy, _)) => None,
                            Err((VideoError::Limit, _)) => {
                                refused = Some(*hash);
                                None
                            }
                            Err((error, _)) => Some(Source::Failed(message(error))),
                        }
                    }
                    Source::Importing(job) => match job.try_result() {
                        Ok(Some(asset)) => {
                            imports = imports.saturating_sub(1);
                            Some(Source::Ready(asset))
                        }
                        Ok(None) => None,
                        Err(error) => {
                            imports = imports.saturating_sub(1);
                            Some(Source::Failed(message(error)))
                        }
                    },
                    Source::Read(_) | Source::Ready(_) | Source::Failed(_) => None,
                };
                waiting |= matches!(entry.state, Source::Reading { .. } | Source::Read(_) | Source::Importing(_));
                if let Some(next) = next {
                    entry.state = next;
                }
            }
            // The service is full: let the least recently shown imported video go, or give up on
            // this one if nothing else can.
            if let Some(hash) = refused
                && !self.evict_source(Some(hash))
                && let Some(entry) = self.sources.get_mut(&hash)
            {
                entry.state = Source::Failed(message(VideoError::Limit));
            }
            waiting
        }

        /// Drops the least recently shown imported video other than `keep` and not shown this
        /// frame; `false` when there's none.
        fn evict_source(&mut self, keep: Option<Blake3>) -> bool {
            let oldest = self
                .sources
                .iter()
                .filter(|(hash, entry)| Some(**hash) != keep && entry.used != self.clock && matches!(entry.state, Source::Ready(_) | Source::Failed(_)))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(hash, _)| *hash);
            oldest.is_some_and(|hash| self.sources.remove(&hash).is_some())
        }

        fn poll_posters(&mut self, ctx: &egui::Context) -> bool {
            let mut waiting = false;
            for ((hash, edge), poster) in &mut self.posters {
                let Poster::Making(job) = poster else {
                    continue;
                };
                match job.try_result() {
                    Ok(Some(frame)) => {
                        *poster = match texture_image(&frame) {
                            Some(image) => {
                                let texture = ctx.load_texture(format!("video-poster-{hash:?}-{edge}"), image, TextureOptions::LINEAR);
                                Poster::Ready { texture, size: [frame.width, frame.height], used: self.clock }
                            }
                            None => Poster::Failed,
                        }
                    }
                    Ok(None) => waiting = true,
                    Err(_) => *poster = Poster::Failed,
                }
            }
            while self.posters.len() > MAX_POSTERS {
                let oldest = self
                    .posters
                    .iter()
                    .filter_map(|(key, poster)| match poster {
                        Poster::Ready { used, .. } if *used != self.clock => Some((*used, *key)),
                        _ => None,
                    })
                    .min();
                let Some((_, key)) = oldest else {
                    break;
                };
                self.posters.remove(&key);
            }
            waiting
        }

        fn poll_players(&mut self, ctx: &egui::Context) -> bool {
            let mut waiting = false;
            let mut jobs = self.frame_jobs();
            let now = self.now;
            let seen = self.clock.wrapping_sub(1);
            for (id, player) in &mut self.players {
                let asset = match self.sources.get(&player.hash).map(|entry| &entry.state) {
                    Some(Source::Ready(asset)) => Some(asset),
                    _ => None,
                };
                let duration = asset.map_or(0, |asset| asset.metadata().duration_us);
                if player.playing {
                    let elapsed = (now - player.anchor.0).max(0.0) * 1_000_000.0;
                    let elapsed = if elapsed.is_finite() { elapsed.min(u64::MAX as f64) as u64 } else { 0 };
                    player.position_us = player.anchor.1.saturating_add(elapsed).min(duration);
                    waiting = true;
                }
                if let Some((time, job)) = &mut player.job {
                    match job.try_result() {
                        Ok(Some(frame)) => {
                            if let Some(image) = texture_image(&frame) {
                                let size = [frame.width, frame.height];
                                match &mut player.shown {
                                    Some(shown) => {
                                        shown.texture.set(image, TextureOptions::LINEAR);
                                        shown.size = size;
                                        shown.time_us = *time;
                                    }
                                    None => {
                                        let texture = ctx.load_texture(format!("video-player-{id:?}"), image, TextureOptions::LINEAR);
                                        player.shown = Some(Shown { texture, size, time_us: *time });
                                    }
                                }
                            }
                            player.job = None;
                            jobs = jobs.saturating_sub(1);
                        }
                        Ok(None) => waiting = true,
                        // Past the last frame: playback ends there.
                        Err(VideoError::NoFrame) => {
                            if player.playing {
                                player.playing = false;
                                player.position_us = duration;
                            }
                            player.job = None;
                            jobs = jobs.saturating_sub(1);
                        }
                        Err(VideoError::Busy | VideoError::Cancelled) => {
                            player.job = None;
                            jobs = jobs.saturating_sub(1);
                        }
                        Err(error) => {
                            player.error = Some(message(error));
                            player.playing = false;
                            player.job = None;
                            jobs = jobs.saturating_sub(1);
                        }
                    }
                }
                if player.playing && player.position_us >= duration {
                    player.playing = false;
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
                if player.job.is_some() || player.seen != seen || player.error.is_some() || jobs >= MAX_FRAME_JOBS {
                    continue;
                }
                let step = step_us(asset);
                // The last frame starts a step before the end; a request at the end has none.
                let last = duration.saturating_sub(step);
                let wanted = (player.position_us.min(last) / step).saturating_mul(step);
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
                        waiting = true;
                    }
                    Err(VideoError::Busy) => waiting = true,
                    Err(error) => player.error = Some(message(error)),
                }
            }
            waiting
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
            let Some((ffmpeg, ffprobe)) = &self.tools else {
                return VideoView::Unavailable(UNCONFIGURED);
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
            if let Service::Failed(why) = self.service {
                return VideoView::Unavailable(why);
            }
            let hash = asset.hash;
            let edge = crate::preview::bucket(edge);
            let clock = self.clock;
            let player = self.players.entry(id).or_insert_with(|| Player {
                hash,
                playing: false,
                position_us: 0,
                anchor: (0.0, 0),
                job: None,
                shown: None,
                edge,
                seen: clock,
                error: None,
            });
            if player.hash != hash {
                // The card shows another video now (a new result): start again.
                *player = Player { hash, playing: false, position_us: 0, anchor: (0.0, 0), job: None, shown: None, edge, seen: clock, error: None };
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
                None if matches!(self.posters.get(&(hash, edge)), Some(Poster::Failed)) => VideoView::Unavailable("Can't play this video"),
                None => VideoView::Pending,
            }
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
            let clock = self.clock;
            if let Some(Poster::Ready { texture, size, used }) = self.posters.get_mut(&(hash, edge)) {
                *used = clock;
                return Some((texture.id(), *size));
            }
            if !self.posters.contains_key(&(hash, edge))
                && self.frame_jobs() < MAX_FRAME_JOBS
                && let (Some(service), Some(SourceEntry { state: Source::Ready(asset), .. })) = (self.service(), self.sources.get(&hash))
            {
                match FrameRequest::new(0, edge, edge).and_then(|request| service.frame(asset, request)) {
                    Ok(job) => {
                        self.posters.insert((hash, edge), Poster::Making(job));
                    }
                    Err(VideoError::Busy) => {}
                    Err(_) => {
                        self.posters.insert((hash, edge), Poster::Failed);
                    }
                }
            }
            // While a sharper one is made, any other size of the same video will do.
            self.posters.iter_mut().find_map(|((h, _), poster)| match poster {
                Poster::Ready { texture, size, used } if *h == hash => {
                    *used = clock;
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
