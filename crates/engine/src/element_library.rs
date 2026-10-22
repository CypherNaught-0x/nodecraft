//! The Elements library (#552): Elements saved outside any project, so one can be placed in every
//! project (`docs/decisions/2026-10-22-elements-library.md`).
//!
//! An entry is an Element's settings and the images it shows ([`Saved`]): nothing else, so a
//! shared entry never carries keys, account data or a project's history. The entries are one file
//! ([`Elements`]) and the images are kept by content hash beside it ([`FileElements`]); the
//! browser keeps both in memory ([`MemoryElements`]). The session answers `element.save` and the
//! library commands through [`handle`], with the project's media at hand, and copies an entry's
//! images into the project when `element.place` or `element.update` adds them.
use nodecraft_format::MAX_ASSET_BYTES;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_nodes::element::{CATEGORIES, MAX_GUIDELINES, MAX_IMAGES, valid_name};
use nodecraft_types::{AssetRef, Blake3, Error, ErrorKind, Modality};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

/// The library file's format version.
pub const ELEMENTS_VERSION: u32 = 1;
/// Largest library file read back or written.
pub const MAX_ELEMENTS_BYTES: usize = 16 * 1024 * 1024;
/// Most entries the library holds.
pub const MAX_ELEMENTS: usize = 1024;

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the Elements library is busy; try again")
}

/// One saved Element: what placing it needs, and nothing else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Saved {
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub guidelines: String,
    /// The reference images, in order.
    pub images: Vec<AssetRef>,
    /// The default reference: the reference sheet the Element showed, if it had one.
    #[serde(default)]
    pub sheet: Option<AssetRef>,
    /// Meant for others too: listed as shared. Private entries are the default.
    #[serde(default)]
    pub shared: bool,
}

impl Saved {
    /// `self`, checked as an entry may be: a valid name and category, bounded guidelines, and up
    /// to [`MAX_IMAGES`] images (at least one image or a sheet).
    pub fn checked(self) -> Result<Self, Error> {
        if !valid_name(&self.name) {
            return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
        }
        if !CATEGORIES.iter().any(|(id, _)| *id == self.category) {
            return Err(invalid("unknown Element category"));
        }
        if self.guidelines.len() > MAX_GUIDELINES {
            return Err(invalid("an Element's guidelines are too long"));
        }
        if self.images.len() > MAX_IMAGES {
            return Err(invalid("an Element holds at most 8 reference images"));
        }
        if self.images.is_empty() && self.sheet.is_none() {
            return Err(invalid("an Element needs an image to be saved"));
        }
        if self.media().any(|asset| !Modality::Image.accepts_mime(asset.mime.essence())) {
            return Err(invalid("an Element is made of images"));
        }
        Ok(self)
    }

    /// Every image the entry shows: its images, then its sheet.
    pub fn media(&self) -> impl Iterator<Item = &AssetRef> {
        self.images.iter().chain(&self.sheet)
    }

    /// What lists show.
    pub fn summary(&self) -> Json {
        json!({"name": self.name, "category": self.category, "guidelines": self.guidelines, "images": self.images.len(),
            "sheet": self.sheet.is_some(), "shared": self.shared})
    }
}

/// The key a name is kept under: names are compared ignoring case, as on a canvas.
fn key(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ElementsFile {
    version: u32,
    #[serde(default)]
    elements: Vec<Saved>,
}

/// The entries in the library, one per name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Elements {
    entries: BTreeMap<String, Saved>,
}

impl Elements {
    /// A library file, checked: every entry valid, each name once.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_ELEMENTS_BYTES {
            return Err(invalid("the Elements library file is too large"));
        }
        let file: ElementsFile = serde_json::from_slice(bytes).map_err(|_| invalid("the Elements library file is damaged"))?;
        if file.version != ELEMENTS_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, "the Elements library was written by a newer NodeCraft"));
        }
        if file.elements.len() > MAX_ELEMENTS {
            return Err(invalid("the Elements library holds too many Elements"));
        }
        let mut elements = Self::default();
        for saved in file.elements {
            let saved = saved.checked()?;
            if elements.entries.insert(key(&saved.name), saved).is_some() {
                return Err(invalid("the Elements library names one Element twice"));
            }
        }
        Ok(elements)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let file = ElementsFile { version: ELEMENTS_VERSION, elements: self.entries.values().cloned().collect() };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|_| Error::new(ErrorKind::Internal, "the Elements library couldn't be written"))?;
        if bytes.len() > MAX_ELEMENTS_BYTES {
            return Err(invalid("the Elements library would be too large"));
        }
        Ok(bytes)
    }

    /// The entry called `name` (ignoring case).
    pub fn get(&self, name: &str) -> Option<&Saved> {
        self.entries.get(&key(name))
    }

    pub fn entries(&self) -> impl Iterator<Item = &Saved> {
        self.entries.values()
    }

    /// Every image some entry shows.
    fn media(&self) -> BTreeSet<Blake3> {
        self.entries.values().flat_map(Saved::media).map(|asset| asset.hash).collect()
    }
}

/// Where a host keeps its Elements library.
pub trait ElementStore {
    /// The library file's bytes, or `None` when there is none yet.
    fn load(&self) -> Result<Option<Vec<u8>>, Error>;
    fn save(&self, bytes: &[u8]) -> Result<(), Error>;
    /// An image's bytes as stored, or `None` when the library doesn't have it.
    fn media(&self, hash: &Blake3) -> Result<Option<Vec<u8>>, Error>;
    /// Keeps `bytes` (which hash to `hash`).
    fn put_media(&self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error>;
    /// Every image kept, to drop those no entry shows.
    fn media_kept(&self) -> Result<Vec<Blake3>, Error>;
    fn drop_media(&self, hash: &Blake3) -> Result<(), Error>;
    /// Held while a change reads, edits and writes the library, so another process's change
    /// can't be lost between them. Busy while another holds it.
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        Ok(Box::new(()))
    }
}

#[derive(Default)]
struct Memory {
    file: Option<Vec<u8>>,
    media: BTreeMap<Blake3, Vec<u8>>,
}

/// A library in memory: tests, and the browser, where it lasts for the session.
#[derive(Clone, Default)]
pub struct MemoryElements(Rc<RefCell<Memory>>);

impl MemoryElements {
    /// The library as last saved.
    pub fn saved(&self) -> Option<Elements> {
        self.0.try_borrow().ok()?.file.as_deref().and_then(|bytes| Elements::from_bytes(bytes).ok())
    }
    /// How many images it keeps.
    pub fn media_count(&self) -> usize {
        self.0.try_borrow().map(|memory| memory.media.len()).unwrap_or_default()
    }
}

impl ElementStore for MemoryElements {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.0.try_borrow().map_err(|_| busy())?.file.clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.file = Some(bytes.to_vec());
        Ok(())
    }
    fn media(&self, hash: &Blake3) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.0.try_borrow().map_err(|_| busy())?.media.get(hash).cloned())
    }
    fn put_media(&self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.media.insert(*hash, bytes.to_vec());
        Ok(())
    }
    fn media_kept(&self) -> Result<Vec<Blake3>, Error> {
        Ok(self.0.try_borrow().map_err(|_| busy())?.media.keys().copied().collect())
    }
    fn drop_media(&self, hash: &Blake3) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.media.remove(hash);
        Ok(())
    }
}

/// The library as `elements.json` in a folder, replaced atomically so a crash or a full disk
/// keeps the last whole file, with its images in `element-media/`, one file per content hash.
/// `elements.lock` beside them orders changes between processes.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileElements {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileElements {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    fn media_dir(&self) -> std::path::PathBuf {
        self.dir.join("element-media")
    }

    fn read_capped(path: &std::path::Path, cap: usize) -> Result<Option<Vec<u8>>, Error> {
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::new(ErrorKind::Internal, "the Elements library can't be read")),
        };
        let mut bytes = Vec::new();
        let cap = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
        file.take(cap).read_to_end(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "the Elements library can't be read"))?;
        Ok(Some(bytes))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn unwritable() -> Error {
    Error::new(ErrorKind::Internal, "the Elements library couldn't be saved (is the disk full?)")
}

#[cfg(not(target_arch = "wasm32"))]
impl ElementStore for FileElements {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        Self::read_capped(&self.dir.join("elements.json"), MAX_ELEMENTS_BYTES)
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&self.dir.join("elements.json"), bytes).map_err(|_| unwritable())
    }
    fn media(&self, hash: &Blake3) -> Result<Option<Vec<u8>>, Error> {
        Self::read_capped(&self.media_dir().join(hash.to_string()), MAX_ASSET_BYTES)
    }
    fn put_media(&self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
        let dir = self.media_dir();
        std::fs::create_dir_all(&dir).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&dir.join(hash.to_string()), bytes).map_err(|_| unwritable())
    }
    fn media_kept(&self) -> Result<Vec<Blake3>, Error> {
        let entries = match std::fs::read_dir(self.media_dir()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(Error::new(ErrorKind::Internal, "the Elements library's images can't be listed")),
        };
        // Anything not named by a hash isn't the library's: leave it alone.
        Ok(entries.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok()).collect())
    }
    fn drop_media(&self, hash: &Blake3) -> Result<(), Error> {
        match std::fs::remove_file(self.media_dir().join(hash.to_string())) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::new(ErrorKind::Internal, "an unused library image couldn't be removed")),
        }
    }
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| Error::new(ErrorKind::Internal, "the Elements library's folder can't be made"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join("elements.lock"))
            .map_err(|_| Error::new(ErrorKind::Internal, "the Elements library's lock can't be opened"))?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => busy(),
            std::fs::TryLockError::Error(_) => Error::new(ErrorKind::Internal, "the Elements library's lock failed"),
        })?;
        Ok(Box::new(file))
    }
}

/// The session's Elements library: its store, and the entries as last read, which
/// `element.place` and `element.update` read without touching storage.
#[derive(Clone)]
pub struct ElementShelf {
    store: Rc<dyn ElementStore>,
    last: Rc<RefCell<Result<Elements, Error>>>,
}

impl ElementShelf {
    pub fn new(store: Rc<dyn ElementStore>) -> Self {
        let last = Rc::new(RefCell::new(read(store.as_ref())));
        Self { store, last }
    }

    /// Calls `f` with the entries last read.
    pub fn with<T>(&self, f: impl FnOnce(&Elements) -> Result<T, Error>) -> Result<T, Error> {
        let shelved = self.last.try_borrow().map_err(|_| busy())?;
        match &*shelved {
            Ok(elements) => f(elements),
            Err(error) => Err(Error::new(error.kind, format!("the Elements library isn't available: {}", error.message))),
        }
    }

    /// The library's copy of `hash`, checked against it.
    pub fn media(&self, hash: &Blake3) -> Result<Vec<u8>, Error> {
        let bytes = self.store.media(hash)?.ok_or_else(|| Error::new(ErrorKind::NotFound, "the Elements library lost an image; save the Element again"))?;
        if Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()) != *hash {
            return Err(invalid("an image in the Elements library is damaged; save the Element again"));
        }
        Ok(bytes)
    }

    fn put(&self, read: Result<Elements, Error>) {
        if let Ok(mut shelved) = self.last.try_borrow_mut() {
            *shelved = read;
        }
    }
}

fn read(store: &dyn ElementStore) -> Result<Elements, Error> {
    store.load()?.map_or_else(|| Ok(Elements::default()), |bytes| Elements::from_bytes(&bytes))
}

/// What a library command asks of the session's library.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    List,
    /// Keep `entry`, its images read from the project; over an entry of that name only when
    /// `replace`.
    Save {
        entry: Saved,
        replace: bool,
    },
    Remove {
        name: String,
    },
    Rename {
        name: String,
        to: String,
    },
}

/// Answers `request` against the shelf's store, reading images `Save` needs from `assets`, and
/// shelves what it read.
pub fn handle(shelf: &ElementShelf, assets: &nodecraft_format::AssetStore, request: Request) -> Result<Json, Error> {
    let store = shelf.store.as_ref();
    if request == Request::List {
        let read = read(store);
        shelf.put(read.clone());
        return Ok(json!({"elements": read?.entries().map(Saved::summary).collect::<Vec<_>>()}));
    }
    let _lock = store.lock()?;
    let mut elements = read(store);
    shelf.put(elements.clone());
    let elements = elements.as_mut().map_err(|e| e.clone())?;
    let result = match request {
        Request::List => return Err(Error::new(ErrorKind::Internal, "listing doesn't change the library")),
        Request::Save { entry, replace } => {
            let entry = entry.checked()?;
            let replaced = elements.entries.contains_key(&key(&entry.name));
            if replaced && !replace {
                return Err(Error::new(ErrorKind::Conflict, "the library already has an Element with that name: replace it, or rename one"));
            }
            if !replaced && elements.entries.len() >= MAX_ELEMENTS {
                return Err(invalid("the Elements library is full; remove Elements you no longer use"));
            }
            // The images first: a crash between leaves unused images, never an entry without them.
            let kept: BTreeSet<Blake3> = store.media_kept()?.into_iter().collect();
            for asset in entry.media() {
                // A kept image is checked, so saving again repairs one that went bad.
                if kept.contains(&asset.hash) && shelf.media(&asset.hash).is_ok() {
                    continue;
                }
                let bytes = assets
                    .open_verified(asset, &TransferCancel::default(), &StreamMetrics::default())?
                    .read_small(MAX_ASSET_BYTES, &StreamMetrics::default())?;
                store.put_media(&asset.hash, &bytes)?;
            }
            let summary = entry.summary();
            elements.entries.insert(key(&entry.name), entry);
            json!({"saved": summary, "replaced": replaced})
        }
        Request::Remove { name } => {
            let removed = elements.entries.remove(&key(&name)).ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no Element with that name"))?;
            json!({"removed": removed.summary()})
        }
        Request::Rename { name, to } => {
            if !valid_name(&to) {
                return Err(invalid("an Element's name takes 1 to 40 letters, digits, - or _"));
            }
            let mut entry = elements.entries.remove(&key(&name)).ok_or_else(|| Error::new(ErrorKind::NotFound, "the library has no Element with that name"))?;
            if elements.entries.contains_key(&key(&to)) {
                return Err(Error::new(ErrorKind::Conflict, "the library already has an Element with that name"));
            }
            entry.name = to;
            let summary = entry.summary();
            elements.entries.insert(key(&entry.name), entry);
            json!({"renamed": summary})
        }
    };
    store.save(&elements.to_bytes()?)?;
    shelf.put(Ok(elements.clone()));
    // Images no entry shows any more go; failing to tidy up loses nothing.
    let shown = elements.media();
    for hash in store.media_kept().unwrap_or_default() {
        if !shown.contains(&hash) {
            let _ = store.drop_media(&hash);
        }
    }
    Ok(result)
}

/// Gives `session` the Elements library in `store`.
pub fn install(session: &mut crate::Session, store: Rc<dyn ElementStore>) {
    session.swap_resource(Some(ElementShelf::new(store)));
}

#[cfg(test)]
mod tests;
