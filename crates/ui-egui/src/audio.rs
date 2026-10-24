//! Sound on cards (#106): a waveform per content hash, and play, pause and seek per card.
//!
//! Like video (#105), sound is decoded by the ffmpeg the settings name (`[media]`), never one a
//! project names, and everything here is view state: which card plays, and where; nothing is
//! written to the document. Natively, a sound's bytes are read on a thread of their own and
//! imported into a video service of its own; its waveform is made once per content hash, and a
//! playing card asks for a couple of seconds of decoded sound at a time, one request in flight,
//! into a bounded buffer the sound device's callback plays from. One card plays at a time.
//! Browsers have no video service, so cards say playback needs the desktop app.
//!
//! A speech card also plays its job's latest preview while the sound arrives (#875): the MP3 so
//! far, imported like any sound but never given a waveform, and dropped once a newer one or the
//! result is ready. When the sound changes from a preview to the next, or to the same job's
//! result, the card keeps its place and goes on playing from there, the old sound playing on
//! until the new one is ready.
use nodecraft_engine::run_jobs::Take;
use nodecraft_graph::Node;
use nodecraft_types::{AssetRef, Value};
use std::sync::Arc;

/// A source sound's asset, else the first sound of the node's active result (inside a list too).
pub fn audio_of(node: &Node) -> Option<&AssetRef> {
    if node.kind == "source.audio" {
        return node.params.get("asset").and_then(audio);
    }
    let result = node.history.iter().find(|g| Some(g.id) == node.active)?;
    result.outputs.iter().find_map(|value| match value {
        Value::List(items) => items.iter().find_map(|item| match item {
            Value::List(inner) => inner.iter().find_map(audio),
            single => audio(single),
        }),
        single => audio(single),
    })
}

fn audio(value: &Value) -> Option<&AssetRef> {
    match value {
        Value::Media(asset) if asset.mime.essence().starts_with("audio/") => Some(asset),
        _ => None,
    }
}

/// What a card can show for its sound right now.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioView {
    /// The sound, and where playback is. `peaks` is `None` while the waveform is made (or when
    /// it couldn't be); `notice` says why playback stopped, when it stopped by itself.
    Sound { peaks: Option<Arc<[u8]>>, position_us: u64, duration_us: u64, playing: bool, notice: Option<&'static str> },
    /// Opening the tools or the sound; nothing to show yet.
    Pending,
    /// Why there's no sound: not set up, not supported here, or it can't be read.
    Unavailable(&'static str),
}

/// What a card asks of its player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioIntent {
    /// Play, or pause; playing at the end starts over.
    Toggle,
    /// Go to this time (microseconds), playing on from there if playing.
    Seek(u64),
    /// Move the playhead during a drag, without decoding intermediate positions.
    PreviewSeek(u64),
    /// Commit the drag's latest playhead position, even if the pointer left the window.
    FinishSeek,
}

/// Shown when no tools are set.
pub const UNCONFIGURED: &str =
    "Install FFmpeg (ffmpeg and ffprobe), or set their absolute paths under [media] in settings or NODECRAFT_FFMPEG / NODECRAFT_FFPROBE";

/// Slices in a card's waveform: a few per pixel of the widest card.
pub const BUCKETS: usize = 512;

/// The time under `x` on a waveform drawn from `left` over `width`, for a sound of `duration_us`.
pub fn time_at(x: f32, left: f32, width: f32, duration_us: u64) -> u64 {
    if !width.is_finite() || width <= 0.0 || !x.is_finite() || !left.is_finite() {
        return 0;
    }
    let along = f64::from(((x - left) / width).clamp(0.0, 1.0));
    (along * duration_us as f64).min(duration_us as f64) as u64
}

/// What a card's player does when its sound changes (#875).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// The same sound.
    Same,
    /// The sound so far grew, or became its job's result: keep the place, and play on if playing.
    Continue,
    /// Another sound: start again from the beginning, paused.
    Restart,
}

/// How a player showing sound `old` (a preview made by take `old_take`, if one) goes on to sound
/// `new`, made by take `new_take` (a newer preview of it, or the result it finished with). A
/// retry is another take: its sound starts again.
pub fn change(old: nodecraft_types::Blake3, old_take: Option<Take>, new: nodecraft_types::Blake3, new_take: Option<Take>) -> Change {
    if old == new {
        Change::Same
    } else if old_take.is_some() && old_take == new_take {
        Change::Continue
    } else {
        Change::Restart
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::Audios;
#[cfg(target_arch = "wasm32")]
pub use web::Audios;

#[cfg(target_arch = "wasm32")]
mod web {
    use super::{AudioIntent, AudioView, Take};
    use nodecraft_types::NodeId;

    /// Browsers have no video service, so no sound decoding, yet.
    #[derive(Default)]
    pub struct Audios;

    impl Audios {
        pub fn configure(&mut self, _: Option<(&std::path::Path, &std::path::Path)>) {}
        pub fn poll(&mut self, _: &egui::Context) {}
        pub fn view(
            &mut self,
            _: &egui::Context,
            _: NodeId,
            _: &nodecraft_types::AssetRef,
            _: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
        ) -> AudioView {
            AudioView::Unavailable("Sound playback needs the desktop app")
        }
        pub fn view_result(
            &mut self,
            _: &egui::Context,
            _: NodeId,
            _: &nodecraft_types::AssetRef,
            _: Option<Take>,
            _: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
        ) -> AudioView {
            AudioView::Unavailable("Sound playback needs the desktop app")
        }
        pub fn view_preview(&mut self, _: &egui::Context, _: NodeId, _: &nodecraft_engine::run_jobs::AudioPreview) -> AudioView {
            AudioView::Unavailable("Sound playback needs the desktop app")
        }
        pub fn act(&mut self, _: &egui::Context, _: NodeId, _: AudioIntent) {}
        pub fn let_go(&mut self, _: NodeId) {}
        pub fn retain(&mut self, _: impl Fn(NodeId) -> bool) {}
        pub fn stop(&mut self) {}
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod output;
#[cfg(not(target_arch = "wasm32"))]
mod playback;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::playback::Feeder;
    use super::{AudioIntent, AudioView, BUCKETS, Change, UNCONFIGURED, change};
    use crate::video::{Retry, stale};
    use nodecraft_engine::run_jobs::{AudioPreview, Take};
    use nodecraft_format::{PreviewAssetRead, PreviewReadError, asset_source::TransferCancel};
    use nodecraft_media::video::{AudioAsset, ServiceConfig, ToolPaths, VideoError, VideoJob, VideoLimits, VideoOpening, VideoService, Waveform};
    use nodecraft_types::{AssetRef, Blake3, NodeId};
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            mpsc::{Receiver, TryRecvError, sync_channel},
        },
    };

    /// Sounds read from the project at once.
    const MAX_READS: usize = 1;
    /// `media`'s cap on one input's bytes; a bigger sound is refused for good.
    const MAX_INPUT: usize = 256 * 1024 * 1024;
    /// Staged sound bytes kept imported; the least recently shown go first past this.
    const MAX_STAGED_BYTES: u64 = 512 * 1024 * 1024;
    /// Waveforms kept; the least recently shown go first past this.
    const MAX_WAVES: usize = 64;
    const POLL: std::time::Duration = std::time::Duration::from_millis(30);
    const LATER: std::time::Duration = std::time::Duration::from_secs(1);
    /// A sound that failed is read again once it's been off screen this many frames.
    const OFFSCREEN_FRAMES: u64 = 600;
    /// A preview no card shows any more is dropped after this many frames (#875).
    const PREVIEW_FRAMES: u64 = 60;

    enum Service {
        Off,
        Opening(VideoOpening),
        Ready(Arc<VideoService>),
        Failed(&'static str),
    }

    enum Source {
        Reading { done: Receiver<Result<Arc<[u8]>, &'static str>>, cancel: TransferCancel },
        Read { bytes: Arc<[u8]>, retry: Option<Retry> },
        Importing { job: VideoJob<AudioAsset>, bytes: Arc<[u8]>, retry: Option<Retry> },
        Ready(AudioAsset),
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
        /// A job's preview (#875): dropped as soon as nothing shows it.
        preview: bool,
    }

    enum Wave {
        Making { job: VideoJob<Waveform>, retry: Option<Retry> },
        Waiting(Retry),
        Ready(Arc<[u8]>),
        Failed,
    }

    struct WaveEntry {
        state: Wave,
        used: u64,
    }

    struct Player {
        hash: Blake3,
        /// Where it's paused, or where playing started.
        position_us: u64,
        seen: u64,
        notice: Option<&'static str>,
        duration_us: Option<u64>,
        /// The take whose preview this is (#875), so its next preview or result plays on.
        preview: Option<Take>,
        /// Play from `position_us` once this sound is ready: it took over from a playing one.
        resume: bool,
        /// A preview played to its end while more is on the way: play on when it comes.
        waiting: bool,
    }

    impl Player {
        fn new(hash: Blake3, seen: u64, preview: Option<Take>) -> Self {
            Self { hash, position_us: 0, seen, notice: None, duration_us: None, preview, resume: false, waiting: false }
        }
    }

    /// The one card playing.
    struct Playback {
        node: NodeId,
    }

    #[derive(Default)]
    struct Wake {
        soon: bool,
        later: bool,
    }

    /// Sound sources, waveforms and players. One per app.
    pub struct Audios {
        service: Service,
        tools: Option<(std::path::PathBuf, std::path::PathBuf)>,
        sources: HashMap<Blake3, SourceEntry>,
        waves: HashMap<Blake3, WaveEntry>,
        players: HashMap<NodeId, Player>,
        playback: Option<Playback>,
        feeder: Option<Feeder>,
        clock: u64,
        now: f64,
    }

    impl Default for Audios {
        fn default() -> Self {
            Self {
                service: Service::Off,
                tools: None,
                sources: HashMap::new(),
                waves: HashMap::new(),
                players: HashMap::new(),
                playback: None,
                feeder: None,
                clock: 0,
                now: 0.0,
            }
        }
    }

    pub(super) fn message(error: VideoError) -> &'static str {
        match error {
            VideoError::Unsupported | VideoError::InvalidMetadata => "This sound's format isn't supported",
            VideoError::Limit => "This sound is too large to play here",
            VideoError::Unavailable => "Can't run ffmpeg or ffprobe; check [media] in settings",
            VideoError::Timeout => "This sound took too long to load",
            _ => "Can't play this sound",
        }
    }

    fn read_message(error: PreviewReadError) -> &'static str {
        match error {
            PreviewReadError::Missing => "This sound's data is missing",
            PreviewReadError::TooLarge => "This sound is too large to play here",
            PreviewReadError::Corrupt => "Can't play this sound",
            _ => "This sound can't be read",
        }
    }

    impl Audios {
        /// Uses these tools from now on: the service is opened when a sound is first shown.
        /// `None` (either unset) leaves sounds unplayable, and says how to set them.
        pub fn configure(&mut self, tools: Option<(&std::path::Path, &std::path::Path)>) {
            let tools = tools.map(|(a, b)| (a.to_path_buf(), b.to_path_buf()));
            if tools != self.tools {
                let feeder = self.feeder.take();
                if let Some(feeder) = &feeder {
                    feeder.stop();
                }
                *self = Self { tools, feeder, ..Self::default() };
            }
        }

        fn asset(&self, hash: Blake3) -> Option<&AudioAsset> {
            match self.sources.get(&hash).map(|entry| &entry.state) {
                Some(Source::Ready(asset)) => Some(asset),
                _ => None,
            }
        }

        /// Takes finished source/waveform work and reads playback progress; call once per frame.
        pub fn poll(&mut self, ctx: &egui::Context) {
            self.clock = self.clock.wrapping_add(1);
            self.now = ctx.input(|i| i.time);
            let mut wake = Wake::default();
            if let Service::Opening(opening) = &mut self.service {
                let failed = match opening.try_result() {
                    Ok(Some(service)) => {
                        self.service = Service::Ready(Arc::new(service));
                        None
                    }
                    Ok(None) => {
                        wake.soon = true;
                        None
                    }
                    Err(VideoError::Unavailable | VideoError::Unsupported | VideoError::Process) => {
                        Some("The ffmpeg or ffprobe in [media] settings can't play sounds")
                    }
                    Err(error) => Some(message(error)),
                };
                if let Some(why) = failed {
                    self.service = Service::Failed(why);
                    self.sources.clear();
                }
            }
            self.poll_sources(&mut wake);
            self.poll_waves(&mut wake);
            if !ctx.input(|input| input.pointer.any_down())
                && let Some(feeder) = &self.feeder
            {
                feeder.finish_seek();
            }
            self.poll_playback(&mut wake);
            if wake.soon {
                ctx.request_repaint_after(POLL);
            } else if wake.later {
                ctx.request_repaint_after(LATER);
            }
        }

        fn poll_sources(&mut self, wake: &mut Wake) {
            let now = self.now;
            let mut importing = self.sources.values().any(|s| matches!(s.state, Source::Importing { .. }));
            for entry in self.sources.values_mut() {
                let next = match &mut entry.state {
                    Source::Reading { done, .. } => {
                        wake.soon = true;
                        match done.try_recv() {
                            Ok(Ok(bytes)) => Some(Source::Read { bytes, retry: None }),
                            Ok(Err(why)) => Some(Source::Failed(why)),
                            Err(TryRecvError::Empty) => None,
                            Err(TryRecvError::Disconnected) => Some(Source::Failed("This sound can't be read")),
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
                        if importing {
                            continue;
                        }
                        match service.import_audio(Arc::clone(bytes)) {
                            Ok(job) => {
                                importing = true;
                                Some(Source::Importing { job, bytes: Arc::clone(bytes), retry: *retry })
                            }
                            Err((VideoError::Busy, _)) => None,
                            Err((error, _)) => Some(match Retry::after(*retry, error, now) {
                                Some(retry) => Source::Read { bytes: Arc::clone(bytes), retry: Some(retry) },
                                None => Source::Failed(message(error)),
                            }),
                        }
                    }
                    Source::Importing { job, bytes, retry } => match job.try_result() {
                        Ok(Some(asset)) => Some(Source::Ready(asset)),
                        Ok(None) => {
                            wake.soon = true;
                            None
                        }
                        Err(error) => Some(match Retry::after(*retry, error, now) {
                            Some(retry) => Source::Read { bytes: Arc::clone(bytes), retry: Some(retry) },
                            None => Source::Failed(message(error)),
                        }),
                    },
                    Source::Ready(_) | Source::Failed(_) => None,
                };
                if let Some(next) = next {
                    if matches!(entry.state, Source::Importing { .. }) {
                        importing = false;
                    }
                    entry.state = next;
                }
            }
            let clock = self.clock;
            let shown: std::collections::HashSet<Blake3> = self.players.values().map(|player| player.hash).collect();
            self.sources.retain(|hash, entry| {
                let failed = matches!(entry.state, Source::Failed(_)) && clock.wrapping_sub(entry.used) > OFFSCREEN_FRAMES;
                let unshown = entry.preview && !shown.contains(hash) && clock.wrapping_sub(entry.used) > PREVIEW_FRAMES;
                !failed && !unshown
            });
        }

        fn poll_waves(&mut self, wake: &mut Wake) {
            let now = self.now;
            for entry in self.waves.values_mut() {
                match &mut entry.state {
                    Wave::Making { job, retry } => match job.try_result() {
                        Ok(Some(wave)) => entry.state = Wave::Ready(Arc::from(wave.peaks)),
                        Ok(None) => wake.soon = true,
                        Err(error) => entry.state = Retry::after(*retry, error, now).map_or(Wave::Failed, Wave::Waiting),
                    },
                    Wave::Waiting(_) => wake.later = true,
                    Wave::Ready(_) | Wave::Failed => {}
                }
            }
            let clock = self.clock;
            while self.waves.len() > MAX_WAVES {
                let oldest = self
                    .waves
                    .iter()
                    .filter(|(_, entry)| stale(entry.used, clock) && !matches!(entry.state, Wave::Making { .. }))
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(key, _)| *key);
                let Some(key) = oldest else {
                    break;
                };
                self.waves.remove(&key);
            }
        }

        fn poll_playback(&mut self, wake: &mut Wake) {
            let Some(playback) = &self.playback else { return };
            let Some(status) = self.feeder.as_ref().and_then(Feeder::status) else { return };
            if let Some(player) = self.players.get_mut(&playback.node) {
                player.position_us = status.position_us;
                player.duration_us = Some(status.duration_us);
                player.notice = status.notice;
            }
            if status.playing {
                wake.soon = true;
            } else {
                // A preview that ran out by itself waits for more rather than stopping.
                if let Some(player) = self.players.get_mut(&playback.node)
                    && player.preview.is_some()
                    && status.notice.is_none()
                {
                    player.waiting = true;
                }
                self.playback = None;
                if let Some(feeder) = &self.feeder {
                    feeder.stop();
                }
            }
        }

        /// The waveform and latest worker progress; requesting it never opens a device.
        pub fn view(
            &mut self,
            ctx: &egui::Context,
            id: NodeId,
            asset: &AssetRef,
            read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
        ) -> AudioView {
            self.view_result(ctx, id, asset, None, read)
        }

        /// [`Audios::view`] for a result that take `finished` made: a card playing that take's
        /// preview plays on into it (#875). Pass it only when this sound is what it made.
        pub fn view_result(
            &mut self,
            ctx: &egui::Context,
            id: NodeId,
            asset: &AssetRef,
            finished: Option<Take>,
            read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
        ) -> AudioView {
            if let Err(view) = self.open(ctx) {
                return view;
            }
            let hash = asset.hash;
            let sound = self.source(ctx, hash, read);
            // The preview plays on while the result is read and imported.
            if matches!(sound, Err(AudioView::Pending))
                && let Some(view) = self.hold(ctx, id, hash, finished)
            {
                return view;
            }
            self.switch(id, hash, finished, false);
            let sound = match sound {
                Ok(sound) => sound,
                Err(view) => return view,
            };
            let peaks = self.wave(hash, &sound);
            self.show(id, &sound, peaks)
        }

        /// The latest preview of take `preview.take`'s sound on node `id` (#875): played like a
        /// sound, without a waveform, and replaced by the next preview once that is ready.
        pub fn view_preview(&mut self, ctx: &egui::Context, id: NodeId, preview: &AudioPreview) -> AudioView {
            if let Err(view) = self.open(ctx) {
                return view;
            }
            let hash = preview.hash;
            let sound = self.preview_source(preview);
            // One that can't be played yet, or at all, leaves the one before playing.
            if sound.is_err()
                && let Some(view) = self.hold(ctx, id, hash, Some(preview.take))
            {
                return view;
            }
            self.switch(id, hash, Some(preview.take), true);
            match sound {
                Ok(sound) => self.show(id, &sound, None),
                Err(view) => view,
            }
        }

        /// What node `id` shows while its next sound `hash` (from `job`) isn't ready: the
        /// preview it shows now, if `hash` continues it and it's ready.
        fn hold(&mut self, ctx: &egui::Context, id: NodeId, hash: Blake3, job: Option<Take>) -> Option<AudioView> {
            let player = self.players.get(&id)?;
            if change(player.hash, player.preview, hash, job) != Change::Continue {
                return None;
            }
            let old = player.hash;
            let sound = self.asset(old)?.clone();
            if let Some(entry) = self.sources.get_mut(&old) {
                entry.used = self.clock;
            }
            ctx.request_repaint_after(POLL);
            Some(self.show(id, &sound, None))
        }

        /// Points node `id`'s player at sound `hash`, made by `job` (a preview of it if
        /// `preview`): from the start, or on from where it was when the sound continues.
        fn switch(&mut self, id: NodeId, hash: Blake3, job: Option<Take>, preview: bool) {
            let clock = self.clock;
            let here = self.playback.as_ref().is_some_and(|p| p.node == id);
            let status = if here { self.feeder.as_ref().and_then(Feeder::status) } else { None };
            let job_if_preview = if preview { job } else { None };
            let player = self.players.entry(id).or_insert_with(|| Player::new(hash, clock, job_if_preview));
            let old = player.hash;
            match change(old, player.preview, hash, job) {
                Change::Same => {}
                Change::Continue => {
                    let position_us = status.map_or(player.position_us, |status| status.position_us);
                    let resume = status.is_some_and(|status| status.playing) || player.waiting || player.resume;
                    *player = Player { position_us, resume, ..Player::new(hash, clock, job_if_preview) };
                }
                // The card has another sound now (a new result): start again.
                Change::Restart => *player = Player::new(hash, clock, job_if_preview),
            }
            player.seen = clock;
            if old != hash {
                if here {
                    self.stop();
                }
                self.drop_unshown_preview(old);
            }
        }

        /// Lets go of the preview node `id` plays (#875), if it plays one: its job ended, or
        /// tried again, and the card has no sound of its own to show instead. It stops, and its
        /// bytes go.
        pub fn let_go(&mut self, id: NodeId) {
            let Some(old) = self.players.get(&id).filter(|player| player.preview.is_some()).map(|player| player.hash) else { return };
            self.players.remove(&id);
            if self.playback.as_ref().is_some_and(|p| p.node == id) {
                self.stop();
            }
            self.drop_unshown_preview(old);
        }

        /// A preview nothing shows any more is no use: drops it now.
        fn drop_unshown_preview(&mut self, hash: Blake3) {
            let shown = self.players.values().any(|player| player.hash == hash);
            if !shown && self.sources.get(&hash).is_some_and(|entry| entry.preview) {
                self.sources.remove(&hash);
            }
        }

        /// Node `id`'s view of `sound`, playing on from its place first if it took over.
        fn show(&mut self, id: NodeId, sound: &AudioAsset, peaks: Option<Arc<[u8]>>) -> AudioView {
            let Some(player) = self.players.get_mut(&id) else { return AudioView::Pending };
            let (resume, from) = (std::mem::take(&mut player.resume), player.position_us);
            let duration = sound.metadata().duration_us;
            if resume && from < duration {
                self.play(id, sound.clone(), from, duration);
            } else if resume && let Some(player) = self.players.get_mut(&id) {
                if player.preview.is_some() {
                    // Not past where it was yet: wait for more.
                    player.waiting = true;
                } else {
                    // A result no longer than what played of it has nothing more: it's over.
                    player.position_us = duration;
                }
            }
            let Some(player) = self.players.get(&id) else { return AudioView::Pending };
            let (paused_at, notice, observed_duration, waiting) = (player.position_us, player.notice, player.duration_us, player.waiting);
            let status = self.playback.as_ref().filter(|p| p.node == id).and_then(|_| self.feeder.as_ref()?.status());
            let position_us = status.map_or(paused_at, |status| status.position_us);
            let duration_us = status.map_or_else(|| observed_duration.unwrap_or(sound.metadata().duration_us), |status| status.duration_us);
            let notice = status.and_then(|status| status.notice).or(notice);
            let playing = status.is_some_and(|status| status.playing) || waiting;
            AudioView::Sound { peaks, position_us, duration_us, playing, notice }
        }

        /// Starts the service the first time a sound is wanted; what to show while it can't.
        fn open(&mut self, ctx: &egui::Context) -> Result<(), AudioView> {
            let Some((ffmpeg, ffprobe)) = &self.tools else {
                return Err(AudioView::Unavailable(UNCONFIGURED));
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
                Service::Failed(why) => Err(AudioView::Unavailable(why)),
                _ => Ok(()),
            }
        }

        /// The imported sound `hash`, or what to show instead; starts reading it when it's new.
        fn source(
            &mut self,
            ctx: &egui::Context,
            hash: Blake3,
            read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
        ) -> Result<AudioAsset, AudioView> {
            let clock = self.clock;
            if !self.sources.contains_key(&hash) {
                let reading = self.sources.values().filter(|s| matches!(s.state, Source::Reading { .. })).count();
                if reading >= MAX_READS {
                    return Err(AudioView::Pending);
                }
                let (state, bytes) = match read() {
                    Ok(input) if input.hash() == hash => {
                        let bytes = input.byte_len();
                        (start_read(ctx, input), bytes)
                    }
                    Ok(_) => (Source::Failed("Can't play this sound"), 0),
                    Err(why) => (Source::Failed(read_message(why)), 0),
                };
                // Room for it among the imported sounds.
                let staged = |sources: &HashMap<Blake3, SourceEntry>| sources.values().map(|s| s.bytes).fold(0u64, u64::saturating_add);
                while staged(&self.sources).saturating_add(bytes) > MAX_STAGED_BYTES && self.evict_source() {}
                self.sources.insert(hash, SourceEntry { state, bytes, used: clock, preview: false });
            }
            self.imported(hash)
        }

        /// The imported preview `preview`, or what to show instead; imports it when it's new.
        fn preview_source(&mut self, preview: &AudioPreview) -> Result<AudioAsset, AudioView> {
            if !self.sources.contains_key(&preview.hash) {
                let bytes = u64::try_from(preview.bytes.len()).unwrap_or(u64::MAX);
                let staged = |sources: &HashMap<Blake3, SourceEntry>| sources.values().map(|s| s.bytes).fold(0u64, u64::saturating_add);
                while staged(&self.sources).saturating_add(bytes) > MAX_STAGED_BYTES && self.evict_source() {}
                let state = Source::Read { bytes: Arc::clone(&preview.bytes), retry: None };
                self.sources.insert(preview.hash, SourceEntry { state, bytes, used: self.clock, preview: true });
            }
            self.imported(preview.hash)
        }

        /// Sound `hash` once imported, else what to show; marks it shown.
        fn imported(&mut self, hash: Blake3) -> Result<AudioAsset, AudioView> {
            let clock = self.clock;
            let entry = self.sources.get_mut(&hash).ok_or(AudioView::Pending)?;
            entry.used = clock;
            match &entry.state {
                Source::Ready(asset) => Ok(asset.clone()),
                Source::Failed(why) => Err(AudioView::Unavailable(why)),
                _ => Err(AudioView::Pending),
            }
        }

        /// Drops the least recently shown imported sound that isn't on screen or playing.
        fn evict_source(&mut self) -> bool {
            let clock = self.clock;
            let playing = self.playback.as_ref().and_then(|p| self.players.get(&p.node)).map(|p| p.hash);
            let oldest = self
                .sources
                .iter()
                .filter(|(hash, entry)| Some(**hash) != playing && stale(entry.used, clock) && matches!(entry.state, Source::Ready(_) | Source::Failed(_)))
                .min_by_key(|(_, entry)| entry.used)
                .map(|(hash, _)| *hash);
            oldest.is_some_and(|hash| self.sources.remove(&hash).is_some())
        }

        /// The waveform of `sound`, starting it when it's new or due to be tried again.
        fn wave(&mut self, hash: Blake3, sound: &AudioAsset) -> Option<Arc<[u8]>> {
            let (clock, now) = (self.clock, self.now);
            let start = match self.waves.get_mut(&hash) {
                Some(entry) => {
                    entry.used = clock;
                    match &entry.state {
                        Wave::Ready(peaks) => return Some(Arc::clone(peaks)),
                        Wave::Waiting(retry) if retry.due(now) => Some(Some(*retry)),
                        _ => None,
                    }
                }
                None => Some(None),
            };
            // One waveform at a time, so the service's other worker is always free for sound.
            let making = self.waves.values().any(|entry| matches!(entry.state, Wave::Making { .. }));
            if let (Some(retry), Service::Ready(service), false) = (start, &self.service, making) {
                let state = match service.waveform(sound, BUCKETS) {
                    Ok(job) => Some(Wave::Making { job, retry }),
                    Err(VideoError::Busy) => None,
                    Err(error) => Some(Retry::after(retry, error, now).map_or(Wave::Failed, Wave::Waiting)),
                };
                if let Some(state) = state {
                    self.waves.insert(hash, WaveEntry { state, used: clock });
                }
            }
            None
        }

        /// Plays, pauses or seeks node `id`'s sound.
        pub fn act(&mut self, ctx: &egui::Context, id: NodeId, intent: AudioIntent) {
            // A preview waiting for more, or about to play on, shows as playing: pause it.
            if intent == AudioIntent::Toggle
                && let Some(player) = self.players.get_mut(&id)
                && (player.waiting || player.resume)
            {
                player.waiting = false;
                player.resume = false;
                ctx.request_repaint();
                return;
            }
            let Some(hash) = self.players.get(&id).map(|player| player.hash) else { return };
            let Some(asset) = self.asset(hash).cloned() else { return };
            let duration = self.players.get(&id).and_then(|player| player.duration_us).unwrap_or(asset.metadata().duration_us);
            let playing = self.playback.as_ref().filter(|p| p.node == id).and_then(|_| self.feeder.as_ref()?.status()).filter(|status| status.playing);
            match (intent, playing) {
                (AudioIntent::Toggle, Some(status)) => {
                    if let Some(player) = self.players.get_mut(&id) {
                        player.position_us = status.position_us;
                        player.duration_us = Some(status.duration_us);
                    }
                    self.stop();
                }
                (AudioIntent::Toggle, None) => {
                    let from = match self.players.get_mut(&id) {
                        Some(player) => {
                            if player.position_us >= duration {
                                player.position_us = 0;
                            }
                            player.notice = None;
                            player.position_us
                        }
                        None => return,
                    };
                    self.play(id, asset, from, duration);
                }
                (AudioIntent::Seek(us) | AudioIntent::PreviewSeek(us), _) => {
                    let us = us.min(duration);
                    if let Some(player) = self.players.get_mut(&id) {
                        player.position_us = us;
                    }
                    if self.playback.as_ref().is_some_and(|playback| playback.node == id)
                        && let Some(feeder) = &self.feeder
                    {
                        if matches!(intent, AudioIntent::PreviewSeek(_)) {
                            feeder.scrub(us);
                        } else {
                            feeder.seek(us);
                        }
                    }
                }
                (AudioIntent::FinishSeek, _) => {
                    if self.playback.as_ref().is_some_and(|playback| playback.node == id)
                        && let Some(feeder) = &self.feeder
                    {
                        feeder.finish_seek();
                    }
                }
            }
            ctx.request_repaint();
        }

        /// Plays node `id`'s `asset` from `from`, stopping whichever card played before.
        fn play(&mut self, id: NodeId, asset: AudioAsset, from: u64, duration: u64) {
            if let Some(previous) = self.playback.take() {
                let status = self.feeder.as_ref().and_then(Feeder::status);
                if let (Some(other), Some(status)) = (self.players.get_mut(&previous.node), status) {
                    other.position_us = status.position_us;
                    other.duration_us = Some(status.duration_us);
                }
            }
            let result = (|| {
                let feeder = Feeder::ready(&mut self.feeder)?;
                let Service::Ready(service) = &self.service else { return Err("This sound is not ready to play") };
                feeder.play(Arc::clone(service), asset, from, duration)
            })();
            match result {
                Ok(()) => {
                    self.playback = Some(Playback { node: id });
                    if let Some(player) = self.players.get_mut(&id) {
                        player.waiting = false;
                    }
                }
                Err(why) => {
                    if let Some(feeder) = &self.feeder {
                        feeder.stop();
                    }
                    if let Some(player) = self.players.get_mut(&id) {
                        player.notice = Some(why);
                        player.waiting = false;
                    }
                }
            }
        }

        /// Forgets the players of nodes `keep` says are gone, stopping one that was playing.
        pub fn retain(&mut self, keep: impl Fn(NodeId) -> bool) {
            self.players.retain(|id, _| keep(*id));
            if self.playback.as_ref().is_some_and(|p| !keep(p.node)) {
                self.stop();
            }
        }

        /// Stops whatever plays, releasing the sound device.
        pub fn stop(&mut self) {
            self.playback = None;
            if let Some(feeder) = &self.feeder {
                feeder.stop();
            }
        }

        #[cfg(test)]
        /// Points node `id`'s player at preview `hash` of take `take`, as a shown preview would.
        pub(crate) fn test_preview_player(&mut self, id: NodeId, hash: Blake3, take: Take) {
            self.players.insert(id, Player::new(hash, self.clock, Some(take)));
        }

        #[cfg(test)]
        /// Sources, finished waveforms, players, and whether one plays.
        pub(crate) fn test_counts(&self) -> (usize, usize, usize, bool) {
            (self.sources.len(), self.waves.values().filter(|w| matches!(w.state, Wave::Ready(_))).count(), self.players.len(), self.playback.is_some())
        }
    }

    /// Reads `input` on a thread of its own; the source says when it's done.
    fn start_read(ctx: &egui::Context, input: PreviewAssetRead) -> Source {
        let (send, done) = sync_channel(1);
        let cancel = TransferCancel::default();
        let active = cancel.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("nodecraft-audio-read".into()).spawn(move || {
            let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.read_verified(&active, &Default::default())));
            let result = match read {
                Ok(Ok(bytes)) => Ok(Arc::<[u8]>::from(bytes)),
                Ok(Err(why)) => Err(read_message(why)),
                Err(_) => Err("This sound can't be read"),
            };
            let _ = send.send(result);
            ctx.request_repaint();
        });
        match spawned {
            Ok(_) => Source::Reading { done, cancel },
            Err(_) => Source::Failed("This sound can't be read"),
        }
    }
}

#[cfg(test)]
mod tests;
