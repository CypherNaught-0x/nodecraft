//! The Layer Editor's fonts (#548): the bytes of each font the node holds, read once per font
//! so text layers render with them as a run does, and (natively) the picker that adds a font
//! file to the node through `layers.font_add`.
use nodecraft_actions::layers::{BUILTIN_FONT, Fonts, MAX_FONT_BYTES, same_name};
use nodecraft_types::{AssetRef, Blake3};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

/// A font the node holds, as `layers.list` names it.
#[derive(Clone, Deserialize)]
pub(super) struct HeldFont {
    pub(super) name: String,
    pub(super) font: AssetRef,
}

/// Each held font's bytes, by asset; None for one that can't be read, which draws as Lato.
#[derive(Clone, Default)]
pub(super) struct FontFiles {
    pub(super) bytes: HashMap<Blake3, Option<Arc<[u8]>>>,
}

impl FontFiles {
    /// Reads any of `held` not read yet, and forgets fonts the node no longer holds.
    pub(super) fn load(&mut self, assets: &nodecraft_format::AssetStore, held: &[HeldFont]) {
        self.bytes.retain(|hash, _| held.iter().any(|f| &f.font.hash == hash));
        for font in held {
            self.bytes.entry(font.font.hash).or_insert_with(|| read(assets, &font.font));
        }
    }

    /// Whether the held font `name` was read and draws with its own outlines; one that wasn't
    /// draws as Lato, in a run too.
    pub(super) fn readable(&self, held: &[HeldFont], name: &str) -> bool {
        // `read` validates every cached Some; listing a font must not rebuild all sixteen
        // face directories for each text layer and each font-panel row.
        same_name(name, BUILTIN_FONT) || held.iter().any(|font| same_name(&font.name, name) && self.bytes.get(&font.font.hash).is_some_and(Option::is_some))
    }

    /// A successful explicit re-import may make a previously missing asset available.
    pub(super) fn forget_missing(&mut self) {
        self.bytes.retain(|_, bytes| bytes.is_some());
    }

    /// Availability participates too: retrying a missing file must redraw its fallback texture.
    pub(super) fn render_key(&self, held: &[HeldFont]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key(held).hash(&mut hasher);
        for font in held {
            self.bytes.get(&font.font.hash).is_some_and(Option::is_some).hash(&mut hasher);
        }
        hasher.finish()
    }

    /// The fonts a render draws with.
    pub(super) fn fonts<'a>(&'a self, held: &[HeldFont]) -> Fonts<'a> {
        let mut fonts = Fonts::default();
        for font in held {
            if let Some(Some(bytes)) = self.bytes.get(&font.font.hash) {
                // One that can't be added draws as Lato, as a run draws it.
                let _ = fonts.add(&font.name, bytes);
            }
        }
        fonts
    }
}

fn read(assets: &nodecraft_format::AssetStore, font: &AssetRef) -> Option<Arc<[u8]>> {
    let bytes = match assets.get(&font.hash) {
        Some(resident) if resident.len() <= MAX_FONT_BYTES => resident.to_vec(),
        Some(_) => return None,
        None => {
            let read = assets.preview_read(font).ok()?;
            if read.byte_len() > u64::try_from(MAX_FONT_BYTES).ok()? {
                return None;
            }
            read.read_verified(&Default::default(), &Default::default()).ok()?
        }
    };
    // Read as a run reads it: a file that isn't a font draws as Lato.
    (bytes.len() <= MAX_FONT_BYTES && nodecraft_actions::layers::font_info(&bytes).is_ok()).then(|| Arc::from(bytes))
}

/// A hash of the held fonts, so a layer renders again when they change.
pub(super) fn key(held: &[HeldFont]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for font in held {
        font.name.hash(&mut hasher);
        font.font.hash.as_bytes().hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) use pick::Picking;

#[cfg(not(target_arch = "wasm32"))]
mod pick {
    use super::MAX_FONT_BYTES;
    use std::{
        future::Future,
        io::Read,
        path::{Path, PathBuf},
        pin::Pin,
        sync::{Arc, mpsc},
        task::{Context, Poll, Wake, Waker},
    };

    type Pick = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

    /// A font file being chosen, then read on its own thread, so neither blocks the frame.
    pub(in crate::app) struct Picking {
        /// The text layer to set the font on once it is added.
        pub(in crate::app) layer: Option<String>,
        phase: Phase,
    }

    enum Phase {
        Choosing(Pick),
        Reading(mpsc::Receiver<Result<Vec<u8>, String>>),
    }

    /// What a poll found.
    pub(in crate::app) enum Polled {
        Waiting(Picking),
        Cancelled,
        Read(Option<String>, Vec<u8>),
        Failed(String),
    }

    #[cfg(not(test))]
    fn choose() -> Pick {
        // Built on the UI thread: AppKit finds its window here.
        let future = rfd::AsyncFileDialog::new().set_title("Add a font").add_filter("Fonts", &["ttf", "otf"]).pick_file();
        Box::pin(async move { future.await.map(|file| file.path().to_path_buf()) })
    }

    /// Tests never open a real dialog: the pick is cancelled.
    #[cfg(test)]
    fn choose() -> Pick {
        Box::pin(std::future::ready(None))
    }

    struct Repaint(egui::Context);
    impl Wake for Repaint {
        fn wake(self: Arc<Self>) {
            self.0.request_repaint();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.request_repaint();
        }
    }

    /// Reads `path` whole: a regular file no larger than a font may be.
    fn read(path: &Path) -> Result<Vec<u8>, String> {
        let too_large = || format!("That font is larger than the {} MiB a font can be.", MAX_FONT_BYTES / (1024 * 1024));
        let unreadable = |_| "That font file can't be read.".to_owned();
        // A stat never blocks on a special file; only a regular file is opened.
        let meta = std::fs::metadata(path).map_err(unreadable)?;
        if !meta.is_file() {
            return Err("That isn't a file.".into());
        }
        let limit = u64::try_from(MAX_FONT_BYTES).unwrap_or(u64::MAX);
        if meta.len() > limit {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        // Bounded even if the file grows while it is read.
        std::fs::File::open(path).map_err(unreadable)?.take(limit.saturating_add(1)).read_to_end(&mut bytes).map_err(unreadable)?;
        if bytes.len() > MAX_FONT_BYTES {
            return Err(too_large());
        }
        Ok(bytes)
    }

    impl Picking {
        #[cfg(test)]
        pub(in crate::app) fn ready(layer: Option<String>, bytes: Vec<u8>) -> Self {
            let (send, receive) = mpsc::channel();
            let _ = send.send(Ok(bytes));
            Self { layer, phase: Phase::Reading(receive) }
        }

        pub(in crate::app) fn start(layer: Option<String>) -> Self {
            Self { layer, phase: Phase::Choosing(choose()) }
        }

        /// Moves the pick or read along.
        pub(in crate::app) fn poll(mut self, ctx: &egui::Context) -> Polled {
            match self.phase {
                Phase::Choosing(ref mut future) => {
                    let waker = Waker::from(Arc::new(Repaint(ctx.clone())));
                    let mut cx = Context::from_waker(&waker);
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(&mut cx))) {
                        Ok(Poll::Pending) => Polled::Waiting(self),
                        Ok(Poll::Ready(None)) => Polled::Cancelled,
                        Ok(Poll::Ready(Some(path))) => {
                            let (send, receive) = mpsc::channel();
                            let ctx = ctx.clone();
                            let spawned = std::thread::Builder::new().name("nodecraft-font-file".into()).spawn(move || {
                                // The editor may have closed; then nobody is waiting.
                                let _ = send.send(read(&path));
                                ctx.request_repaint();
                            });
                            match spawned {
                                Ok(_) => Polled::Waiting(Self { phase: Phase::Reading(receive), ..self }),
                                Err(_) => Polled::Failed("The font couldn't be read; try again.".into()),
                            }
                        }
                        Err(_) => Polled::Failed("The file picker failed; nothing was changed.".into()),
                    }
                }
                Phase::Reading(ref receive) => match receive.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => Polled::Waiting(self),
                    Err(mpsc::TryRecvError::Disconnected) => Polled::Failed("The font couldn't be read; try again.".into()),
                    Ok(Err(message)) => Polled::Failed(message),
                    Ok(Ok(bytes)) => Polled::Read(self.layer, bytes),
                },
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) use pick::Polled;
