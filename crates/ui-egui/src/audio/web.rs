//! Sound playback in the browser (#1070). The browser has no FFmpeg, but it decodes sound itself:
//! each playing card gets an `<audio>` element of its own, never added to the page, whose source
//! is the card's sound as an object URL. There is no waveform (the card draws its bar without
//! one).
//!
//! A speech card plays its job's latest preview while the sound arrives (#875), as on the
//! desktop: a newer preview of the same take, or the result it finishes with, takes the old one's
//! place and goes on from where it was ([`super::change`]); another sound starts again, paused.
//! A preview that plays up to the end of what has arrived waits for more, still playing, and the
//! next one goes on from there. The old preview stays until the next one is ready.
//!
//! One card plays at a time. Everything here is view state. Bounded: at most [`MAX_PLAYERS`]
//! players (the least recently shown go first, freeing their URL; cards shown in the same frame
//! never push each other out), a player whose card isn't shown is paused, and a sound of at most
//! [`MAX_INPUT`] bytes. A sound the browser can't decode says so on its card and isn't tried
//! again until it changes; a store that is only busy is asked again shortly.
use super::{AudioIntent, AudioView, Change, change};
use nodecraft_engine::run_jobs::{AudioPreview, Take};
use nodecraft_format::{PreviewAssetRead, PreviewReadError};
use nodecraft_types::{AssetRef, Blake3, NodeId};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

/// Players kept at once; the least recently shown go first past this.
const MAX_PLAYERS: usize = 8;
/// The most bytes of one sound handed to the browser: each player keeps a copy as a blob, and an
/// asset not in memory is read on the frame that opens it.
const MAX_INPUT: u64 = 64 * 1024 * 1024;
/// How soon a card whose sound the store was too busy to read asks again.
const RETRY: std::time::Duration = std::time::Duration::from_millis(250);

const UNREADABLE: &str = "The sound can't be read";
const TOO_LARGE: &str = "The sound is too large to play here";
const UNSUPPORTED: &str = "This browser can't play this sound; the desktop app can";
const NO_PAGE: &str = "The page can't make a sound player";
const TOO_MANY: &str = "Too many sounds are shown at once to play them all here; zoom in on fewer";
const REFUSED: &str = "The browser kept it from playing; press play";

/// One card's player: its element and the copy of the sound it plays from.
struct Player {
    hash: Blake3,
    /// The take a preview (or the result it became) came from, so the next one continues it.
    take: Option<Take>,
    /// Whether it plays a preview, which is never the finished sound.
    preview: bool,
    /// Whether the person asked it to play (and hasn't paused it): a preview that ran out of
    /// sound so far is still playing, waiting for more.
    wants_play: bool,
    /// Set when the browser refused to play it (no click on the page yet, say).
    refused: Rc<Cell<bool>>,
    audio: web_sys::HtmlAudioElement,
    url: String,
    /// The frame (egui's pass number) it was last shown in.
    used: u64,
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.audio.pause();
        let _ = self.audio.remove_attribute("src");
        self.audio.load();
        let _ = web_sys::Url::revoke_object_url(&self.url);
    }
}

impl Player {
    /// Whether the card shows it playing: sounding now, or a preview waiting for more.
    fn playing(&self) -> bool {
        self.wants_play && (self.preview || !self.audio.ended()) && !self.refused.get()
    }

    fn play(&mut self) {
        self.wants_play = true;
        self.refused.set(false);
        if self.audio.ended() {
            if self.preview {
                // A preview at its end waits for the next one.
                return;
            }
            self.audio.set_current_time(0.0);
        }
        play(&self.audio, &self.refused);
    }

    fn pause(&mut self) {
        self.wants_play = false;
        let _ = self.audio.pause();
    }

    fn view(&self) -> AudioView {
        AudioView::Sound {
            peaks: None,
            position_us: micros(self.audio.current_time()),
            duration_us: micros(self.audio.duration()),
            playing: self.playing(),
            notice: self.refused.get().then_some(REFUSED),
        }
    }
}

/// Where a player that is replaced by the next sound of its take was, to go on from there.
#[derive(Clone, Copy)]
struct Place {
    seconds: f64,
    playing: bool,
}

/// A sound to show: an asset to read, or a preview's bytes.
enum Source<'a, R> {
    Asset(&'a AssetRef, R),
    Preview(&'a AudioPreview),
}

/// Why a player couldn't be made.
enum Fail {
    /// For good, until the sound changes.
    Lasting(&'static str),
    /// The store is busy: ask again shortly.
    Passing,
}

#[derive(Default)]
pub struct Audios {
    players: HashMap<NodeId, Player>,
    /// Cards whose sound couldn't be played, with which sound and why: not tried again until the
    /// card shows another sound.
    failed: HashMap<NodeId, (Blake3, &'static str)>,
}

impl Audios {
    /// The browser decodes sound itself; there are no tools to set.
    pub fn configure(&mut self, _: Option<(&std::path::Path, &std::path::Path)>) {}

    /// Keeps the playhead moving while a card plays, and pauses a player whose card wasn't shown
    /// last frame: its sound would go on with no way to stop it.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let now = ctx.cumulative_pass_nr();
        for player in self.players.values_mut() {
            if player.used.saturating_add(1) < now && player.wants_play {
                player.pause();
            }
        }
        if self.players.values().any(Player::playing) {
            ctx.request_repaint();
        }
    }

    /// What `node`'s card shows of a source sound `asset`.
    pub fn view(
        &mut self,
        ctx: &egui::Context,
        node: NodeId,
        asset: &AssetRef,
        read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
    ) -> AudioView {
        self.show(ctx, node, asset.hash, None, Source::Asset(asset, read))
    }

    /// What `node`'s card shows of its result `asset`, made by `take` when its job is known: a
    /// preview of that take playing on the card continues into it.
    pub fn view_result(
        &mut self,
        ctx: &egui::Context,
        node: NodeId,
        asset: &AssetRef,
        take: Option<Take>,
        read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
    ) -> AudioView {
        self.show(ctx, node, asset.hash, take, Source::Asset(asset, read))
    }

    /// What `node`'s card shows of the sound its job has sent so far.
    pub fn view_preview(&mut self, ctx: &egui::Context, node: NodeId, preview: &AudioPreview) -> AudioView {
        self.show::<fn() -> Result<PreviewAssetRead, PreviewReadError>>(ctx, node, preview.hash, Some(preview.take), Source::Preview(preview))
    }

    fn show<R: FnOnce() -> Result<PreviewAssetRead, PreviewReadError>>(
        &mut self,
        ctx: &egui::Context,
        node: NodeId,
        hash: Blake3,
        take: Option<Take>,
        source: Source<'_, R>,
    ) -> AudioView {
        let frame = ctx.cumulative_pass_nr();
        // What the card's player does with this sound: keep it, go on into it, or start again.
        let (kept, place) = match self.players.get_mut(&node) {
            Some(player) => {
                player.used = frame;
                match change(player.hash, player.preview.then_some(player.take).flatten(), hash, take) {
                    Change::Same => (true, None),
                    Change::Continue => (false, Some(Place { seconds: player.audio.current_time(), playing: player.wants_play })),
                    Change::Restart => {
                        self.players.remove(&node);
                        (false, None)
                    }
                }
            }
            None => (false, None),
        };
        if !kept {
            match self.failed.get(&node) {
                Some((failed, why)) if *failed == hash => {
                    // A preview that couldn't be opened leaves the one before it playing.
                    return self.players.get(&node).map_or(AudioView::Unavailable(why), Player::view);
                }
                Some(_) => {
                    self.failed.remove(&node);
                }
                None => {}
            }
            // Room first: only a player not shown this frame makes way, so cards on screen never
            // take turns evicting each other.
            if place.is_none() && !self.make_room(frame) {
                return AudioView::Unavailable(TOO_MANY);
            }
            let preview = matches!(source, Source::Preview(_));
            match open(hash, take, preview, source, place) {
                // The new sound is ready: only now does it take the old one's place.
                Ok(mut player) => {
                    player.used = frame;
                    if place.is_some_and(|place| place.playing) {
                        self.pause_others(node);
                    }
                    self.players.insert(node, player);
                }
                Err(Fail::Passing) => {
                    ctx.request_repaint_after(RETRY);
                    return self.players.get(&node).map_or(AudioView::Pending, Player::view);
                }
                Err(Fail::Lasting(why)) => {
                    self.failed.insert(node, (hash, why));
                    return self.players.get(&node).map_or(AudioView::Unavailable(why), Player::view);
                }
            }
        }
        let Some(player) = self.players.get(&node) else { return AudioView::Pending };
        if player.audio.error().is_some() {
            self.players.remove(&node);
            self.failed.insert(node, (hash, UNSUPPORTED));
            return AudioView::Unavailable(UNSUPPORTED);
        }
        player.view()
    }

    /// Plays, pauses or seeks `node`'s card. One card plays at a time, as on the desktop:
    /// playing one pauses the others where they are.
    pub fn act(&mut self, ctx: &egui::Context, node: NodeId, intent: AudioIntent) {
        let playing = match self.players.get(&node) {
            Some(player) => player.playing(),
            None => return,
        };
        if intent == AudioIntent::Toggle && !playing {
            self.pause_others(node);
        }
        let Some(player) = self.players.get_mut(&node) else { return };
        match intent {
            AudioIntent::Toggle if playing => player.pause(),
            AudioIntent::Toggle => player.play(),
            // The browser finds a place cheaply, so a drag moves the sound with it.
            AudioIntent::Seek(us) | AudioIntent::PreviewSeek(us) => {
                let seconds = us as f64 / 1_000_000.0;
                let duration = player.audio.duration();
                player.audio.set_current_time(if duration.is_finite() { seconds.clamp(0.0, duration) } else { seconds.max(0.0) });
            }
            AudioIntent::FinishSeek => {}
        }
        ctx.request_repaint();
    }

    fn pause_others(&mut self, node: NodeId) {
        for (other, player) in &mut self.players {
            if *other != node && player.wants_play {
                player.pause();
            }
        }
    }

    /// Lets go of the preview `node` plays, if it plays one: its job ended or tried again, and
    /// the card has no sound of its own to show instead. A card's own sound is kept.
    pub fn let_go(&mut self, node: NodeId) {
        if self.players.get(&node).is_some_and(|player| player.preview) {
            self.players.remove(&node);
        }
    }

    /// The page's video element plays a video card's own sound (#1069), so there is nothing to
    /// follow here.
    pub fn follow(
        &mut self,
        _: &egui::Context,
        _: NodeId,
        _: &nodecraft_types::AssetRef,
        _: bool,
        _: u64,
        _: impl FnOnce() -> Result<nodecraft_format::PreviewAssetRead, nodecraft_format::PreviewReadError>,
    ) {
    }

    /// Lets go of the players of cards no longer shown on the canvas.
    pub fn retain(&mut self, keep: impl Fn(NodeId) -> bool) {
        self.players.retain(|id, _| keep(*id));
        self.failed.retain(|id, _| keep(*id));
    }

    /// Pauses every card.
    pub fn stop(&mut self) {
        for player in self.players.values_mut() {
            player.pause();
        }
    }

    /// Lets go of the least recently shown players past [`MAX_PLAYERS`], making space for one
    /// more; never one shown in `frame`. Whether there is space.
    fn make_room(&mut self, frame: u64) -> bool {
        while self.players.len() >= MAX_PLAYERS {
            let oldest = self.players.iter().filter(|(_, player)| player.used < frame).min_by_key(|(_, player)| player.used).map(|(id, _)| *id);
            match oldest {
                Some(oldest) => {
                    self.players.remove(&oldest);
                }
                None => return false,
            }
        }
        true
    }
}

/// Plays `audio`. A page that hasn't had the person's click yet may refuse sound: then the card
/// says so (`refused`), rather than playing it muted.
fn play(audio: &web_sys::HtmlAudioElement, refused: &Rc<Cell<bool>>) {
    if let Ok(promise) = audio.play() {
        let refused = Rc::clone(refused);
        wasm_bindgen_futures::spawn_local(async move {
            if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                refused.set(true);
            }
        });
    }
}

/// Seconds from the element as microseconds; nothing for an unknown or endless length.
fn micros(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 { (seconds * 1_000_000.0) as u64 } else { 0 }
}

/// A player for `source`, going on from `place` when it continues a sound of the same take.
fn open<R: FnOnce() -> Result<PreviewAssetRead, PreviewReadError>>(
    hash: Blake3,
    take: Option<Take>,
    preview: bool,
    source: Source<'_, R>,
    place: Option<Place>,
) -> Result<Player, Fail> {
    let url = match source {
        Source::Preview(preview) => blob_url(&preview.bytes, preview.mime)?,
        Source::Asset(asset, read) => {
            let read = read().map_err(read_failed)?;
            if read.byte_len() > MAX_INPUT {
                return Err(Fail::Lasting(TOO_LARGE));
            }
            match read.resident() {
                Some(bytes) => blob_url(bytes, asset.mime.essence())?,
                None => blob_url(&read.read_verified(&Default::default(), &Default::default()).map_err(read_failed)?, asset.mime.essence())?,
            }
        }
    };
    let Ok(audio) = web_sys::HtmlAudioElement::new() else {
        let _ = web_sys::Url::revoke_object_url(&url);
        return Err(Fail::Lasting(NO_PAGE));
    };
    audio.set_preload("auto");
    match place {
        // A media fragment starts the element where the last one was, once it has loaded.
        Some(place) if place.seconds.is_finite() && place.seconds > 0.0 => audio.set_src(&format!("{url}#t={:.3}", place.seconds)),
        _ => audio.set_src(&url),
    }
    let mut player = Player { hash, take, preview, wants_play: false, refused: Rc::default(), audio, url, used: 0 };
    if place.is_some_and(|place| place.playing) {
        player.play();
    }
    Ok(player)
}

/// A store that is busy, full or was asked to stop will answer later; anything else won't.
fn read_failed(error: PreviewReadError) -> Fail {
    match error {
        PreviewReadError::Busy | PreviewReadError::Capacity | PreviewReadError::Cancelled => Fail::Passing,
        PreviewReadError::TooLarge => Fail::Lasting(TOO_LARGE),
        PreviewReadError::Missing | PreviewReadError::Unavailable | PreviewReadError::Corrupt | PreviewReadError::Unreadable => Fail::Lasting(UNREADABLE),
    }
}

/// An object URL for a copy of `bytes` as `mime`, at most [`MAX_INPUT`] of them.
fn blob_url(bytes: &[u8], mime: &str) -> Result<String, Fail> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_INPUT {
        return Err(Fail::Lasting(TOO_LARGE));
    }
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(mime);
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag).map_err(|_| Fail::Lasting(NO_PAGE))?;
    web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| Fail::Lasting(NO_PAGE))
}
