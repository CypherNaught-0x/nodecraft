//! My library (#96): images, videos, sounds, 3D models and PDFs a person saved outside any project,
//! so they can be placed on any canvas of any project
//! (`docs/decisions/2026-11-03-library-url-import-and-global-library.md`).
//!
//! It has the Elements library's shape (`element_library`): one versioned index file
//! ([`Index`]) and the bytes kept by content hash beside it ([`FileAssetLibrary`]); the browser
//! and headless hosts keep both in memory for the session ([`MemoryAssetLibrary`]). An entry is a
//! media reference, a public name and the name of the project it was saved from: never a path, a
//! canvas, a card or a URL. Only what a person saves goes in; nothing is added on its own.
//!
//! * **Bounded.** At most [`MAX_ENTRIES`] entries and [`MAX_LIBRARY_BYTES`] of media, each at
//!   most one small asset. A save that would go past either is refused for that file (nothing
//!   is evicted: the library is the person's, and they choose what goes).
//! * **Corruption-tolerant.** Listing never writes: an index that can't be read is reported, and a
//!   person's Repair (`library.repair_saved`) keeps it aside (`library.damaged.json`, never over an
//!   earlier one) and rebuilds the index from the files kept, within a 1 GiB read budget; a file
//!   it can't use or didn't check is kept as an unusable entry, never deleted. Entries this build
//!   can't use are kept as they are and reported. A newer format is reported and left alone.
//! * **Safe across windows.** Every change takes `library.lock`, reads the index again, changes it
//!   and replaces it atomically, so two windows never lose each other's change. Listing reads the
//!   index again; placing uses the entries as last listed.
use crate::library_browse_cmds::{ASSET_KINDS, kind_of};
use nodecraft_format::MAX_ASSET_BYTES;
use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_types::{AssetRef, Blake3, Error, ErrorKind, Modality};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

/// The index file's format version.
pub const LIBRARY_VERSION: u32 = 1;
/// Largest index file read back or written.
pub const MAX_INDEX_BYTES: usize = 8 * 1024 * 1024;
/// Most entries the library holds.
pub const MAX_ENTRIES: usize = 4096;
/// Most media bytes the library holds, summed over its entries.
pub const MAX_LIBRARY_BYTES: u64 = 1024 * 1024 * 1024;
/// Longest entry name and origin, in characters.
pub const MAX_LABEL_CHARS: usize = 80;
/// Most entries one save or removal names.
pub const MAX_CHANGE: usize = 32;

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "My library is busy in another window; try again")
}

fn label(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_LABEL_CHARS).collect::<String>().trim().to_owned()
}

/// One saved file: what placing it needs, and nothing else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub asset: AssetRef,
    /// What it is called: a file or card name, never a path.
    pub name: String,
    /// The title of the project it was saved from, when known.
    #[serde(default)]
    pub origin: Option<String>,
    pub bytes: u64,
}

impl Entry {
    /// `self`, checked as an entry may be.
    pub fn checked(self) -> Result<Self, Error> {
        if self.name.trim().is_empty() || self.name.chars().count() > MAX_LABEL_CHARS || self.name.chars().any(char::is_control) {
            return Err(invalid("an entry's name takes 1 to 80 characters"));
        }
        if self.origin.as_ref().is_some_and(|origin| origin.chars().count() > MAX_LABEL_CHARS || origin.chars().any(char::is_control)) {
            return Err(invalid("an entry's origin takes at most 80 characters"));
        }
        if !self.kind().is_some_and(|kind| ASSET_KINDS.contains(&kind)) {
            return Err(invalid("My library holds images, videos, sounds, 3D models and PDFs"));
        }
        if self.bytes == 0 || self.bytes > MAX_ASSET_BYTES as u64 {
            return Err(invalid("an entry is between 1 byte and 16 MiB"));
        }
        Ok(self)
    }

    pub fn kind(&self) -> Option<Modality> {
        kind_of(&self.asset.mime)
    }

    /// What lists show.
    pub fn summary(&self) -> Json {
        json!({"hash": self.asset.hash, "mime": self.asset.mime, "kind": self.kind(), "meta": self.asset.meta, "name": self.name,
            "origin": self.origin, "bytes": self.bytes})
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexFile {
    version: u32,
    #[serde(default)]
    assets: Vec<Json>,
}

/// The entries, in the order they were saved, and those this build can't use (kept as written).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Index {
    entries: Vec<Entry>,
    damaged: Vec<(String, Json)>,
}

impl Index {
    /// An index file, checked. An entry that isn't valid (one a newer build wrote, one edited by
    /// hand, a second entry for the same bytes) is set aside with why, not the whole index.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_INDEX_BYTES {
            return Err(invalid("My library's index is too large"));
        }
        let file: IndexFile = serde_json::from_slice(bytes).map_err(|_| invalid("My library's index is damaged"))?;
        if file.version > LIBRARY_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, "My library was written by a newer NodeCraft; update to use it"));
        }
        if file.version != LIBRARY_VERSION {
            return Err(invalid("My library's index has an unknown version"));
        }
        if file.assets.len() > MAX_ENTRIES {
            return Err(invalid("My library's index holds too many entries"));
        }
        let mut index = Self::default();
        let mut seen = BTreeSet::new();
        for raw in file.assets {
            let checked = serde_json::from_value::<Entry>(raw.clone())
                .map_err(|_| invalid("it has a field this build doesn't know, or one is missing"))
                .and_then(Entry::checked);
            match checked {
                Ok(entry) if !seen.insert(entry.asset.hash) => index.damaged.push(("another entry holds the same file".into(), raw)),
                Ok(entry) => index.entries.push(entry),
                Err(error) => index.damaged.push((error.message, raw)),
            }
        }
        Ok(index)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let unwritable = || Error::new(ErrorKind::Internal, "My library's index couldn't be written");
        let mut assets = Vec::with_capacity(self.entries.len().saturating_add(self.damaged.len()));
        for entry in &self.entries {
            assets.push(serde_json::to_value(entry).map_err(|_| unwritable())?);
        }
        assets.extend(self.damaged.iter().map(|(_, raw)| raw.clone()));
        let bytes = serde_json::to_vec_pretty(&IndexFile { version: LIBRARY_VERSION, assets }).map_err(|_| unwritable())?;
        if bytes.len() > MAX_INDEX_BYTES {
            return Err(invalid("My library's index would be too large"));
        }
        Ok(bytes)
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn get(&self, hash: &Blake3) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.asset.hash == *hash)
    }

    /// The media bytes the entries hold.
    pub fn used_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).fold(0, u64::saturating_add)
    }

    fn len(&self) -> usize {
        self.entries.len().saturating_add(self.damaged.len())
    }

    /// Every hash an entry holds or a damaged entry mentions (a newer build may read it again).
    fn media(&self) -> BTreeSet<Blake3> {
        let mut media: BTreeSet<Blake3> = self.entries.iter().map(|entry| entry.asset.hash).collect();
        for (_, raw) in &self.damaged {
            let mut stack = vec![raw];
            while let Some(value) = stack.pop() {
                match value {
                    Json::String(text) => media.extend(text.parse::<Blake3>().ok()),
                    Json::Array(items) => stack.extend(items),
                    Json::Object(fields) => stack.extend(fields.values()),
                    _ => {}
                }
            }
        }
        media
    }
}

/// Where a host keeps My library.
pub trait AssetLibraryStore {
    /// The index's bytes, or `None` when there is none yet.
    fn load(&self) -> Result<Option<Vec<u8>>, Error>;
    fn save(&self, bytes: &[u8]) -> Result<(), Error>;
    /// Keeps an index that couldn't be read, for the person to look at, before it is rebuilt,
    /// never over one kept earlier; the name it is kept under.
    fn set_aside(&self, bytes: &[u8]) -> Result<String, Error>;
    /// A file's bytes as stored, or `None` when the library doesn't have it.
    fn media(&self, hash: &Blake3) -> Result<Option<Vec<u8>>, Error>;
    /// Keeps `bytes` (which hash to `hash`).
    fn put_media(&self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error>;
    /// Every file kept, by hash.
    fn media_kept(&self) -> Result<Vec<Blake3>, Error>;
    fn drop_media(&self, hash: &Blake3) -> Result<(), Error>;
    /// Held while a change reads, edits and writes the index. Busy while another holds it.
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        Ok(Box::new(()))
    }
}

/// Most damaged indexes kept aside.
const MAX_ASIDE: usize = 20;

/// The name the `n`th damaged index (from 1) is kept under.
fn aside_name(n: usize) -> String {
    if n <= 1 { "library.damaged.json".into() } else { format!("library.damaged-{n}.json") }
}

#[derive(Default)]
struct Memory {
    file: Option<Vec<u8>>,
    aside: Vec<Vec<u8>>,
    media: std::collections::BTreeMap<Blake3, Vec<u8>>,
}

/// My library in memory: tests, the browser and headless hosts, where it lasts for the session.
#[derive(Clone, Default)]
pub struct MemoryAssetLibrary(Rc<RefCell<Memory>>);

impl MemoryAssetLibrary {
    /// The index as last saved.
    pub fn index(&self) -> Option<Vec<u8>> {
        self.0.try_borrow().ok()?.file.clone()
    }
    /// Replaces the index's bytes, as another window (or damage) would.
    pub fn write_index(&self, bytes: &[u8]) {
        if let Ok(mut memory) = self.0.try_borrow_mut() {
            memory.file = Some(bytes.to_vec());
        }
    }
    /// The indexes set aside, oldest first.
    pub fn aside(&self) -> Vec<Vec<u8>> {
        self.0.try_borrow().map(|memory| memory.aside.clone()).unwrap_or_default()
    }
    pub fn media_count(&self) -> usize {
        self.0.try_borrow().map(|memory| memory.media.len()).unwrap_or_default()
    }
}

impl AssetLibraryStore for MemoryAssetLibrary {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.0.try_borrow().map_err(|_| busy())?.file.clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.file = Some(bytes.to_vec());
        Ok(())
    }
    fn set_aside(&self, bytes: &[u8]) -> Result<String, Error> {
        let mut memory = self.0.try_borrow_mut().map_err(|_| busy())?;
        memory.aside.push(bytes.to_vec());
        Ok(aside_name(memory.aside.len()))
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

/// My library as `library.json` in a folder, replaced atomically so a crash or a full disk keeps
/// the last whole index, with its files in `media/`, one per content hash. `library.lock`
/// beside them orders changes between windows and processes.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileAssetLibrary {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileAssetLibrary {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }

    fn media_dir(&self) -> std::path::PathBuf {
        self.dir.join("media")
    }

    fn read_capped(path: &std::path::Path, cap: usize) -> Result<Option<Vec<u8>>, Error> {
        use std::io::Read;
        let unreadable = || Error::new(ErrorKind::Internal, "My library can't be read");
        // A stat never blocks on a special file; only a regular file is opened.
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => return Err(unreadable()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(unreadable()),
        }
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(unreadable()),
        };
        let mut bytes = Vec::new();
        let cap = u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1);
        file.take(cap).read_to_end(&mut bytes).map_err(|_| unreadable())?;
        Ok(Some(bytes))
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn unwritable() -> Error {
    Error::new(ErrorKind::Internal, "My library couldn't be saved: the disk may be full, or another program has the file open; try again")
}

#[cfg(not(target_arch = "wasm32"))]
impl AssetLibraryStore for FileAssetLibrary {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        Self::read_capped(&self.dir.join(INDEX_FILE), MAX_INDEX_BYTES)
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&self.dir.join(INDEX_FILE), bytes).map_err(|_| unwritable())
    }
    fn set_aside(&self, bytes: &[u8]) -> Result<String, Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| unwritable())?;
        // The first free name: an earlier damaged index is never overwritten.
        let name = (1..=MAX_ASIDE)
            .map(aside_name)
            .find(|name| std::fs::symlink_metadata(self.dir.join(name)).is_err())
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "My library already kept 20 damaged indexes; delete the library.damaged files first"))?;
        craft_atomic::atomic_write(&self.dir.join(&name), bytes).map_err(|_| unwritable())?;
        Ok(name)
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
            Err(_) => return Err(Error::new(ErrorKind::Internal, "My library's files can't be listed")),
        };
        // Anything not named by a hash isn't the library's: leave it alone.
        let mut kept: Vec<Blake3> = entries.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok()).collect();
        kept.sort();
        Ok(kept)
    }
    fn drop_media(&self, hash: &Blake3) -> Result<(), Error> {
        match std::fs::remove_file(self.media_dir().join(hash.to_string())) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::new(ErrorKind::Internal, "a file My library no longer uses couldn't be removed")),
        }
    }
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| Error::new(ErrorKind::Internal, "My library's folder can't be made"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join("library.lock"))
            .map_err(|_| Error::new(ErrorKind::Internal, "My library's lock can't be opened"))?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => busy(),
            std::fs::TryLockError::Error(_) => Error::new(ErrorKind::Internal, "My library's lock failed"),
        })?;
        Ok(Box::new(file))
    }
}

/// The session's library: its store, and the entries as last read, which placing reads
/// without touching storage.
#[derive(Clone)]
pub struct AssetShelf {
    store: Rc<dyn AssetLibraryStore>,
    last: Rc<RefCell<Result<Index, Error>>>,
}

impl AssetShelf {
    /// The shelf over `store`. Nothing is written: a damaged index waits for a person to repair
    /// it (`library.repair_saved`).
    pub fn new(store: Rc<dyn AssetLibraryStore>) -> Self {
        let first = match read(store.as_ref()) {
            Ok(Read::Index(index)) => Ok(index),
            Ok(Read::Damaged { .. }) => Err(needs_repair()),
            Err(error) => Err(error),
        };
        Self { store, last: Rc::new(RefCell::new(first)) }
    }

    /// Calls `f` with the entries last read.
    pub fn with<T>(&self, f: impl FnOnce(&Index) -> Result<T, Error>) -> Result<T, Error> {
        let shelved = self.last.try_borrow().map_err(|_| busy())?;
        match &*shelved {
            Ok(index) => f(index),
            Err(error) => Err(Error::new(error.kind, format!("My library isn't available: {}", error.message))),
        }
    }

    /// The library's copy of `hash`, checked against it.
    pub fn media(&self, hash: &Blake3) -> Result<Vec<u8>, Error> {
        let bytes = self.store.media(hash)?.ok_or_else(|| Error::new(ErrorKind::NotFound, "My library lost that file; remove the entry and save it again"))?;
        if Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()) != *hash {
            return Err(invalid("a file in My library is damaged; remove the entry and save it again"));
        }
        Ok(bytes)
    }

    /// [`Self::media`], also recognised from its bytes as the type its entry (as last listed)
    /// says: an index edited to call a PDF an image is refused, never placed under a wrong type.
    pub fn media_as_listed(&self, hash: &Blake3) -> Result<Vec<u8>, Error> {
        let listed = self.with(|index| {
            index.get(hash).map(|entry| entry.asset.mime.clone()).ok_or_else(|| Error::new(ErrorKind::NotFound, "My library has no such entry; list it again"))
        })?;
        let bytes = self.media(hash)?;
        let (mime, _) = crate::import::describe_media(&bytes).map_err(|why| invalid(&format!("a file in My library can't be placed: {why}")))?;
        if mime.essence() != listed.essence() {
            return Err(invalid("a file in My library isn't the type its entry says; remove the entry and save it again"));
        }
        Ok(bytes)
    }

    fn put(&self, read: Result<Index, Error>) {
        if let Ok(mut shelved) = self.last.try_borrow_mut() {
            *shelved = read;
        }
    }
}

fn needs_repair() -> Error {
    invalid("My library's index is damaged; repair it in My library (Repair) first")
}

enum Read {
    Index(Index),
    /// The index exists but can't be read; its bytes, and why.
    Damaged {
        bytes: Vec<u8>,
        why: String,
    },
}

/// The index in `store`: empty when there is none, `Damaged` when it can't be read (a newer
/// format is an error instead, and is never repaired). Messages name the file, never its folder.
fn read(store: &dyn AssetLibraryStore) -> Result<Read, Error> {
    let Some(bytes) = store.load()? else { return Ok(Read::Index(Index::default())) };
    match Index::from_bytes(&bytes) {
        Ok(index) => Ok(Read::Index(index)),
        Err(error) if error.kind == ErrorKind::Unsupported => Err(Error::new(error.kind, format!("{} ({INDEX_FILE})", error.message))),
        Err(error) => Ok(Read::Damaged { bytes, why: error.message }),
    }
}

/// The index's file name, the only part of its location messages and listings give.
pub const INDEX_FILE: &str = "library.json";
/// Most bytes a repair reads to check files against their hashes; files past it are kept, unchecked.
pub const MAX_REPAIR_BYTES: u64 = MAX_LIBRARY_BYTES;

/// A new index made from the files kept. Each file whose bytes still match its hash and that is
/// recognised as media becomes an entry named after its kind (names and origins were in the
/// index). Any other file named by a hash that still matches, or that wasn't read because the
/// repair's byte budget ran out, is kept as an unusable entry, so no later tidying deletes it.
fn rebuild(store: &dyn AssetLibraryStore) -> Result<Index, Error> {
    let mut index = Index::default();
    let (mut used, mut read_bytes) = (0u64, 0u64);
    let keep = |index: &mut Index, hash: Blake3, why: &str| index.damaged.push((why.to_owned(), json!({"hash": hash, "kept": why})));
    for hash in store.media_kept()? {
        if index.len() >= MAX_ENTRIES {
            break;
        }
        if read_bytes >= MAX_REPAIR_BYTES {
            keep(&mut index, hash, "not checked: the repair reads at most 1 GiB");
            continue;
        }
        let bytes = match store.media(&hash) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(_) => {
                keep(&mut index, hash, "it couldn't be read during the repair");
                continue;
            }
        };
        read_bytes = read_bytes.saturating_add(bytes.len() as u64);
        if Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()) != hash {
            continue;
        }
        let len = bytes.len() as u64;
        let described = crate::import::describe_media(&bytes).ok().filter(|_| used.saturating_add(len) <= MAX_LIBRARY_BYTES);
        let entry = described.and_then(|(mime, meta)| {
            let asset = AssetRef { hash, mime, meta };
            let name = format!("Recovered {}", kind_of(&asset.mime).map_or("file", noun));
            (Entry { asset, name, origin: None, bytes: len }).checked().ok()
        });
        match entry {
            Some(entry) => {
                used = used.saturating_add(len);
                index.entries.push(entry);
            }
            None => keep(&mut index, hash, "this version of NodeCraft can't use it"),
        }
    }
    Ok(index)
}

fn noun(kind: Modality) -> &'static str {
    match kind {
        Modality::Image => "image",
        Modality::Video => "video",
        Modality::Audio => "sound",
        Modality::Mesh => "3D model",
        Modality::Document => "PDF",
        Modality::Text | Modality::Style => "file",
    }
}

/// One file to save: the project's reference to it and its public name.
#[derive(Clone, Debug, PartialEq)]
pub struct Saving {
    pub asset: AssetRef,
    pub name: String,
}

/// What a library command asks of the session's library.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    List {
        offset: usize,
        limit: usize,
        kind: Option<Modality>,
        query: Option<String>,
    },
    /// Copy these files from the project into the library, saved from the project `origin`.
    Save {
        items: Vec<Saving>,
        origin: String,
    },
    Remove {
        hashes: Vec<Blake3>,
    },
    /// Keep a damaged index aside and rebuild it from the files kept.
    Repair,
}

/// Answers `request` against the shelf's store, reading what `Save` needs from `assets`, and
/// shelves what it read. Listing never writes.
pub fn handle(shelf: &AssetShelf, assets: &nodecraft_format::AssetStore, request: Request) -> Result<Json, Error> {
    let store = shelf.store.as_ref();
    if let Request::List { offset, limit, kind, query } = &request {
        let index = match read(store) {
            Ok(Read::Index(index)) => index,
            Ok(Read::Damaged { why, .. }) => {
                shelf.put(Err(needs_repair()));
                return Ok(json!({"assets": [], "total": 0, "offset": offset, "limit": limit, "used_bytes": 0, "max_bytes": MAX_LIBRARY_BYTES,
                    "entries": 0, "max_entries": MAX_ENTRIES, "index_damaged": why, "file": INDEX_FILE}));
            }
            Err(error) => {
                shelf.put(Err(error.clone()));
                return Err(error);
            }
        };
        shelf.put(Ok(index.clone()));
        let matching: Vec<&Entry> = index
            .entries
            .iter()
            .rev()
            .filter(|entry| kind.is_none_or(|kind| entry.kind() == Some(kind)))
            .filter(|entry| {
                query.as_deref().is_none_or(|q| {
                    [entry.name.as_str(), entry.origin.as_deref().unwrap_or_default(), entry.asset.mime.essence()].iter().any(|f| f.to_lowercase().contains(q))
                })
            })
            .collect();
        let mut listed = json!({
            "assets": matching.iter().skip(*offset).take(*limit).map(|entry| entry.summary()).collect::<Vec<_>>(),
            "total": matching.len(), "offset": offset, "limit": limit,
            "used_bytes": index.used_bytes(), "max_bytes": MAX_LIBRARY_BYTES, "entries": index.entries.len(), "max_entries": MAX_ENTRIES,
        });
        if !index.damaged.is_empty() {
            listed["damaged"] = index.damaged.iter().map(|(why, _)| json!(why)).collect();
            listed["file"] = json!(INDEX_FILE);
        }
        return Ok(listed);
    }
    let _lock = store.lock()?;
    if request == Request::Repair {
        return match read(store)? {
            Read::Index(index) => {
                shelf.put(Ok(index));
                Ok(json!({"repaired": false}))
            }
            Read::Damaged { bytes, why } => {
                let kept_as = store.set_aside(&bytes)?;
                let index = rebuild(store)?;
                store.save(&index.to_bytes()?)?;
                let result = json!({"repaired": true, "why": why, "entries": index.entries.len(), "kept": index.damaged.len(), "kept_as": kept_as});
                shelf.put(Ok(index));
                Ok(result)
            }
        };
    }
    let mut index = match read(store)? {
        Read::Index(index) => index,
        Read::Damaged { .. } => {
            shelf.put(Err(needs_repair()));
            return Err(needs_repair());
        }
    };
    let mut result = match request {
        Request::List { .. } | Request::Repair => return Err(Error::new(ErrorKind::Internal, "that request doesn't change the library here")),
        Request::Save { items, origin } => {
            let origin = Some(label(&origin)).filter(|origin| !origin.is_empty());
            let mut used = index.used_bytes();
            let mut saved = Vec::with_capacity(items.len());
            for Saving { asset, name } in items {
                let hash = asset.hash;
                let outcome = if index.get(&hash).is_some() {
                    Ok("already")
                } else if index.len() >= MAX_ENTRIES {
                    Err("My library is full (4096 entries); remove entries you no longer use".to_owned())
                } else {
                    save_one(shelf, assets, &asset, used).and_then(|bytes| {
                        let entry = Entry { asset, name: name.clone(), origin: origin.clone(), bytes }.checked().map_err(|error| error.message)?;
                        used = used.saturating_add(bytes);
                        index.entries.push(entry);
                        Ok("saved")
                    })
                };
                saved.push(match outcome {
                    Ok(state) => json!({"hash": hash, "name": name, "state": state}),
                    Err(reason) => json!({"hash": hash, "name": name, "state": "failed", "reason": reason}),
                });
            }
            json!({"saved": saved})
        }
        Request::Remove { hashes } => {
            let before = index.entries.len();
            index.entries.retain(|entry| !hashes.contains(&entry.asset.hash));
            json!({"removed": before.saturating_sub(index.entries.len())})
        }
    };
    let bytes = index.to_bytes()?;
    store.save(&bytes)?;
    result["used_bytes"] = json!(index.used_bytes());
    // Files no entry holds (or a damaged one mentions) go; failing to tidy up loses nothing.
    let kept = index.media();
    for hash in store.media_kept().unwrap_or_default() {
        if !kept.contains(&hash) {
            let _ = store.drop_media(&hash);
        }
    }
    shelf.put(Ok(index));
    Ok(result)
}

/// Copies one project file into the store; its size, or why it wasn't saved.
fn save_one(shelf: &AssetShelf, assets: &nodecraft_format::AssetStore, asset: &AssetRef, used: u64) -> Result<u64, String> {
    let size = assets.descriptor(&asset.hash).map(|d| d.bytes).ok_or_else(|| "the file is missing from this project".to_owned())?;
    if size > MAX_ASSET_BYTES as u64 {
        return Err("files over 16 MiB aren't kept in My library yet".into());
    }
    if used.saturating_add(size) > MAX_LIBRARY_BYTES {
        return Err("My library is full (1 GiB); remove entries you no longer use".into());
    }
    // A file the store already has (one an earlier save left before a crash) is checked first.
    if shelf.media(&asset.hash).is_err() {
        let bytes = assets
            .open_verified(asset, &TransferCancel::default(), &StreamMetrics::default())
            .and_then(|verified| verified.read_small(MAX_ASSET_BYTES, &StreamMetrics::default()))
            .map_err(|error| error.message)?;
        shelf.store.put_media(&asset.hash, &bytes).map_err(|error| error.message)?;
    }
    Ok(size)
}

/// A file's public name for My library: a card's title or file name without control
/// characters, at most [`MAX_LABEL_CHARS`] characters.
pub fn entry_name(text: &str) -> String {
    let name = label(text);
    if name.is_empty() { "Saved file".into() } else { name }
}

/// Gives `session` My library in `store`.
pub fn install(session: &mut crate::Session, store: Rc<dyn AssetLibraryStore>) {
    session.swap_resource(Some(AssetShelf::new(store)));
}

#[cfg(test)]
mod tests;
