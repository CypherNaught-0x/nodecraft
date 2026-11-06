//! Video playback in the browser (#1069). The browser has no FFmpeg, but it decodes video itself:
//! each playing card gets a `<video>` element of its own, never added to the page, whose source
//! is the card's video as an object URL. Each frame its current picture is drawn into a canvas no
//! larger than the card needs and taken into the card's texture, and the element plays the
//! video's sound through the page, so a card with a sound track is heard while it plays.
//!
//! Everything here is view state, as on the desktop. Bounded: at most [`MAX_PLAYERS`] players
//! (the least recently shown go first, freeing their URL; cards shown in the same frame never
//! push each other out), a player whose card isn't shown is paused, frames are taken about 30
//! times a second, a video of at most `media`'s 256 MiB
//! input cap, and textures at most [`MAX_EDGE`] on their longer side. A video the browser can't
//! decode says so on its card and isn't tried again until it changes.
use super::{VideoIntent, VideoView};
use eframe::wasm_bindgen::JsCast as _;
use egui::{ColorImage, TextureHandle, TextureOptions};
use nodecraft_format::{PreviewAssetRead, PreviewReadError};
use nodecraft_types::{AssetRef, Blake3, NodeId};
use std::collections::HashMap;

/// Players kept at once; the least recently shown go first past this.
const MAX_PLAYERS: usize = 8;
/// `media`'s cap on one video's bytes.
const MAX_INPUT: u64 = 256 * 1024 * 1024;
/// The longest side of a card's frame texture, whatever the card's size.
const MAX_EDGE: u32 = 1024;
/// `HTMLMediaElement.HAVE_CURRENT_DATA`: the element has a frame to draw.
const HAVE_CURRENT_DATA: u16 = 2;
/// Least time between two frames taken from a playing element: about 30 a second. A display
/// repaints faster than most videos change, and each take reads the canvas back.
const DRAW_EVERY: std::time::Duration = std::time::Duration::from_millis(33);

const UNREADABLE: &str = "The video can't be read";
const TOO_LARGE: &str = "The video is too large to play here";
const UNSUPPORTED: &str = "This browser can't play this video; the desktop app can";
const NO_PAGE: &str = "The page can't make a video player";
const TOO_MANY: &str = "Too many videos are shown at once to play them all here; zoom in on fewer";

/// One card's player: its element and the copy of the video it plays from.
struct Player {
    hash: Blake3,
    video: web_sys::HtmlVideoElement,
    url: String,
    canvas: web_sys::HtmlCanvasElement,
    context: web_sys::CanvasRenderingContext2d,
    texture: Option<TextureHandle>,
    size: [u32; 2],
    /// The element's time when its frame was last drawn, and at which size, and when.
    drawn: Option<(f64, [u32; 2])>,
    drawn_at: Option<web_time::Instant>,
    /// The frame (egui's pass number) it was last shown in.
    used: u64,
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.video.pause();
        let _ = self.video.remove_attribute("src");
        self.video.load();
        let _ = web_sys::Url::revoke_object_url(&self.url);
    }
}

#[derive(Default)]
pub struct Videos {
    players: HashMap<NodeId, Player>,
    /// Cards whose video couldn't be played, with which video and why: not tried again until the
    /// card shows another video.
    failed: HashMap<NodeId, (Blake3, &'static str)>,
}

impl Videos {
    /// The browser decodes video itself; there are no tools to set.
    pub fn configure(&mut self, _: Option<(&std::path::Path, &std::path::Path)>) {}

    /// Keeps drawing while a card plays, and pauses a player whose card wasn't shown last frame
    /// (scrolled away, zoomed out, its output changed): its sound would go on with no way to stop it.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let now = ctx.cumulative_pass_nr();
        for player in self.players.values() {
            if player.used.saturating_add(1) < now && !player.video.paused() {
                let _ = player.video.pause();
            }
        }
        if self.players.values().any(|player| !player.video.paused() && !player.video.ended()) {
            ctx.request_repaint();
        }
    }

    /// What `node`'s card shows of `asset`, at most `edge` points on its longer side.
    pub fn view(
        &mut self,
        ctx: &egui::Context,
        node: NodeId,
        asset: &AssetRef,
        edge: f32,
        read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
    ) -> VideoView {
        let frame = ctx.cumulative_pass_nr();
        if let Some((hash, why)) = self.failed.get(&node) {
            if *hash == asset.hash {
                return VideoView::Unavailable(why);
            }
            self.failed.remove(&node);
        }
        if self.players.get(&node).is_some_and(|player| player.hash != asset.hash) {
            self.players.remove(&node);
        }
        if !self.players.contains_key(&node) {
            // Room first: only a player not shown this frame makes way, so cards on screen never
            // take turns evicting each other.
            if !self.make_room(frame) {
                return VideoView::Unavailable(TOO_MANY);
            }
            match open(asset, read) {
                Ok(player) => {
                    self.players.insert(node, player);
                }
                Err(why) => {
                    self.failed.insert(node, (asset.hash, why));
                    return VideoView::Unavailable(why);
                }
            }
        }
        let Some(player) = self.players.get_mut(&node) else { return VideoView::Pending };
        player.used = frame;
        if player.video.error().is_some() {
            self.players.remove(&node);
            self.failed.insert(node, (asset.hash, UNSUPPORTED));
            return VideoView::Unavailable(UNSUPPORTED);
        }
        if player.video.ready_state() < HAVE_CURRENT_DATA || player.video.seeking() {
            // The element loads on the page's own time; look again shortly.
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
            return VideoView::Pending;
        }
        if let Err(why) = player.draw(ctx, edge) {
            self.players.remove(&node);
            self.failed.insert(node, (asset.hash, why));
            return VideoView::Unavailable(why);
        }
        let Some(texture) = &player.texture else { return VideoView::Pending };
        VideoView::Frame {
            texture: texture.id(),
            size: player.size,
            position_us: micros(player.video.current_time()),
            duration_us: micros(player.video.duration()),
            playing: !player.video.paused() && !player.video.ended(),
        }
    }

    /// A video's length and poster for the Timeline editor. The browser's player is per card, so
    /// the editor shows its clips without posters here.
    pub fn still(
        &mut self,
        _: &egui::Context,
        _: &AssetRef,
        _: f32,
        _: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>,
    ) -> (Option<u64>, Option<(egui::TextureId, [u32; 2])>) {
        (None, None)
    }

    /// Plays, pauses or seeks `node`'s card.
    pub fn act(&mut self, ctx: &egui::Context, node: NodeId, intent: VideoIntent) {
        let Some(player) = self.players.get(&node) else { return };
        let video = &player.video;
        match intent {
            VideoIntent::Toggle if video.paused() || video.ended() => {
                if video.ended() {
                    video.set_current_time(0.0);
                }
                // A page that hasn't had the person's click yet may refuse sound: then it plays muted.
                if let Ok(promise) = video.play() {
                    let video = video.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        if wasm_bindgen_futures::JsFuture::from(promise).await.is_err() {
                            video.set_muted(true);
                            let _ = video.play();
                        }
                    });
                }
            }
            VideoIntent::Toggle => {
                let _ = video.pause();
            }
            VideoIntent::Seek(us) => {
                let seconds = us as f64 / 1_000_000.0;
                let duration = video.duration();
                video.set_current_time(if duration.is_finite() { seconds.clamp(0.0, duration) } else { seconds.max(0.0) });
            }
        }
        ctx.request_repaint();
    }

    /// Lets go of the players of cards no longer shown on the canvas.
    pub fn retain(&mut self, keep: impl Fn(NodeId) -> bool) {
        self.players.retain(|id, _| keep(*id));
        self.failed.retain(|id, _| keep(*id));
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

impl Player {
    /// Takes the element's current frame into the texture, when it moved or the card's size did.
    fn draw(&mut self, ctx: &egui::Context, edge: f32) -> Result<(), &'static str> {
        let (width, height) = (self.video.video_width(), self.video.video_height());
        if width == 0 || height == 0 {
            return Ok(());
        }
        let size = fit(width, height, edge);
        let now = self.video.current_time();
        if self.texture.is_some() && self.drawn == Some((now, size)) {
            return Ok(());
        }
        // A playing video is taken about 30 times a second, however fast the display repaints.
        let playing = !self.video.paused() && !self.video.ended();
        let resized = self.drawn.is_none_or(|(_, drawn)| drawn != size);
        if playing && !resized && self.texture.is_some() && self.drawn_at.is_some_and(|at| at.elapsed() < DRAW_EVERY) {
            return Ok(());
        }
        let [w, h] = size;
        if resized {
            self.canvas.set_width(w);
            self.canvas.set_height(h);
        }
        self.context.draw_image_with_html_video_element_and_dw_and_dh(&self.video, 0.0, 0.0, f64::from(w), f64::from(h)).map_err(|_| UNSUPPORTED)?;
        let data = self.context.get_image_data(0.0, 0.0, f64::from(w), f64::from(h)).map_err(|_| UNSUPPORTED)?.data();
        let expected = usize::try_from(u64::from(w) * u64::from(h) * 4).map_err(|_| TOO_LARGE)?;
        if data.0.len() != expected {
            return Err(UNSUPPORTED);
        }
        let pixels = [usize::try_from(w).map_err(|_| TOO_LARGE)?, usize::try_from(h).map_err(|_| TOO_LARGE)?];
        let image = ColorImage::from_rgba_unmultiplied(pixels, &data.0);
        match &mut self.texture {
            Some(texture) => texture.set(image, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("card video", image, TextureOptions::LINEAR)),
        }
        self.size = size;
        self.drawn = Some((now, size));
        self.drawn_at = Some(web_time::Instant::now());
        Ok(())
    }
}

/// A frame of `width` × `height` scaled down to at most `edge` (and [`MAX_EDGE`]) on its longer
/// side, never up, and never to nothing.
fn fit(width: u32, height: u32, edge: f32) -> [u32; 2] {
    let longest = width.max(height).max(1);
    let edge = if edge.is_finite() && edge >= 1.0 { (edge as u32).min(MAX_EDGE) } else { MAX_EDGE };
    if longest <= edge {
        return [width.max(1), height.max(1)];
    }
    let scale = |side: u32| u32::try_from(u64::from(side) * u64::from(edge) / u64::from(longest)).unwrap_or(edge).max(1);
    [scale(width), scale(height)]
}

/// Seconds from the element as microseconds; nothing for an unknown or endless length.
fn micros(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 { (seconds * 1_000_000.0) as u64 } else { 0 }
}

/// A player for `asset`, from the bytes `read` gives.
fn open(asset: &AssetRef, read: impl FnOnce() -> Result<PreviewAssetRead, PreviewReadError>) -> Result<Player, &'static str> {
    let read = read().map_err(|error| if matches!(error, PreviewReadError::TooLarge) { TOO_LARGE } else { UNREADABLE })?;
    if read.byte_len() > MAX_INPUT {
        return Err(TOO_LARGE);
    }
    let owned;
    let bytes = match read.resident() {
        Some(bytes) => bytes,
        None => {
            owned = read.read_verified(&Default::default(), &Default::default()).map_err(|_| UNREADABLE)?;
            &owned[..]
        }
    };
    let document = web_sys::window().and_then(|window| window.document()).ok_or(NO_PAGE)?;
    let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(asset.mime.essence());
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag).map_err(|_| NO_PAGE)?;
    let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(|_| NO_PAGE)?;
    let made = (|| {
        let video: web_sys::HtmlVideoElement = document.create_element("video").ok()?.dyn_into().ok()?;
        let _ = video.set_attribute("playsinline", "");
        video.set_preload("auto");
        video.set_src(&url);
        let canvas: web_sys::HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
        // Its pixels are read back every drawn frame: ask for a canvas kept where that is cheap.
        let options = js_sys::Object::new();
        js_sys::Reflect::set(&options, &"willReadFrequently".into(), &true.into()).ok()?;
        let context: web_sys::CanvasRenderingContext2d = canvas.get_context_with_context_options("2d", &options).ok()??.dyn_into().ok()?;
        Some((video, canvas, context))
    })();
    let Some((video, canvas, context)) = made else {
        let _ = web_sys::Url::revoke_object_url(&url);
        return Err(NO_PAGE);
    };
    Ok(Player { hash: asset.hash, video, url, canvas, context, texture: None, size: [0, 0], drawn: None, drawn_at: None, used: 0 })
}
