//! The persistent verified template cache (#954): the last good index and every verified archive,
//! under the app data directory in a versioned folder. The store only moves bounded bytes; every
//! byte read back is verified again here before it is listed, so a damaged cache is reported and
//! skipped, never trusted and never a panic. Browser and test hosts keep it in memory.
use super::{Entry, catalog, parse_index};
use crate::Session;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// The cache layout's version: its folder name. A later layout uses a new folder, and an older
/// build ignores it.
pub const LAYOUT: &str = "v1";
/// Most files one cache read admits (an archive and its record per template, plus the index).
pub const MAX_FILES: usize = 2 * 128 + 1;
/// Largest index record (`<blake3>.json`) read back.
pub const MAX_RECORD_BYTES: usize = 4 * 1024;
/// Most cache problems kept for `templates.status`.
const MAX_PROBLEMS: usize = 32;

/// What a store read back, before verification.
#[derive(Default)]
pub struct Stored {
    pub index: Option<Vec<u8>>,
    /// `(blake3 name, record bytes, archive bytes)`.
    pub archives: Vec<(String, Vec<u8>, Vec<u8>)>,
    /// Files that could not be read back, in words a person can act on.
    pub problems: Vec<String>,
}

/// Where verified templates are kept between launches.
pub trait TemplateCacheStore {
    /// Every file, bounded by [`MAX_FILES`], [`catalog::MAX_CACHE_BYTES`] and the per-file caps.
    fn read(&self) -> Result<Stored, Error>;
    fn write_index(&self, bytes: &[u8]) -> Result<(), Error>;
    /// Stores an archive and then its record; the record's presence commits the pair.
    fn write_archive(&self, name: &str, record: &[u8], bytes: &[u8]) -> Result<(), Error>;
    fn remove_archive(&self, name: &str) -> Result<(), Error>;
    /// Removes the whole cache.
    fn clear(&self) -> Result<(), Error>;
    /// Whether this store survives a restart.
    fn persistent(&self) -> bool;
    /// The cache folder, when it is one.
    fn location(&self) -> Option<String>;
}

/// A cache in memory: the browser and tests.
#[derive(Clone, Default)]
pub struct MemoryCache(Rc<RefCell<Held>>);
/// The index, and each archive's `(record, bytes)` by name.
type Held = (Option<Vec<u8>>, BTreeMap<String, (Vec<u8>, Vec<u8>)>);

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "The template cache is busy; try again")
}
impl TemplateCacheStore for MemoryCache {
    fn read(&self) -> Result<Stored, Error> {
        let held = self.0.try_borrow().map_err(|_| busy())?;
        Ok(Stored {
            index: held.0.clone(),
            archives: held.1.iter().map(|(name, (record, bytes))| (name.clone(), record.clone(), bytes.clone())).collect(),
            problems: Vec::new(),
        })
    }
    fn write_index(&self, bytes: &[u8]) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.0 = Some(bytes.to_vec());
        Ok(())
    }
    fn write_archive(&self, name: &str, record: &[u8], bytes: &[u8]) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.1.insert(name.to_owned(), (record.to_vec(), bytes.to_vec()));
        Ok(())
    }
    fn remove_archive(&self, name: &str) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.1.remove(name);
        Ok(())
    }
    fn clear(&self) -> Result<(), Error> {
        *self.0.try_borrow_mut().map_err(|_| busy())? = Default::default();
        Ok(())
    }
    fn persistent(&self) -> bool {
        false
    }
    fn location(&self) -> Option<String> {
        None
    }
}

/// The desktop cache: `<app data>/templates/v1/`.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileCache {
    root: std::path::PathBuf,
}
#[cfg(not(target_arch = "wasm32"))]
impl FileCache {
    /// The cache under `data` (the settings folder): `data/templates/v1`.
    pub fn new(data: &std::path::Path) -> Self {
        Self { root: data.join("templates") }
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.join(LAYOUT)
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn unwritable() -> Error {
    Error::new(ErrorKind::Internal, "The template cache couldn't be written (is the disk full or the folder read-only?)")
}
/// Reads an ordinary file up to `cap` bytes; `None` when it is larger.
#[cfg(not(target_arch = "wasm32"))]
fn read_capped(path: &std::path::Path, cap: usize) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1)).read_to_end(&mut bytes)?;
    Ok((bytes.len() <= cap).then_some(bytes))
}
#[cfg(not(target_arch = "wasm32"))]
impl TemplateCacheStore for FileCache {
    fn read(&self) -> Result<Stored, Error> {
        let dir = self.dir();
        let mut stored = Stored::default();
        let listing = match std::fs::read_dir(&dir) {
            Ok(listing) => listing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(stored),
            Err(_) => return Err(Error::new(ErrorKind::Internal, "The template cache folder couldn't be read")),
        };
        let mut names = Vec::new();
        for (seen, item) in listing.enumerate() {
            if seen >= MAX_FILES {
                stored.problems.push(format!("The template cache holds more than {MAX_FILES} files; the rest were ignored"));
                break;
            }
            let Ok(item) = item else {
                stored.problems.push("A template cache entry couldn't be read".into());
                continue;
            };
            let name = item.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with(".tmp") {
                continue;
            }
            match item.file_type() {
                Ok(kind) if kind.is_file() => names.push(name),
                _ => stored.problems.push(format!("Ignored {name} in the template cache: not an ordinary file")),
            }
        }
        names.sort();
        let mut total = 0usize;
        for name in &names {
            if name == "index.json" {
                match read_capped(&dir.join(name), super::MAX_INDEX_BYTES) {
                    Ok(Some(bytes)) => stored.index = Some(bytes),
                    Ok(None) => stored.problems.push("The cached template index is larger than its limit".into()),
                    Err(_) => stored.problems.push("The cached template index couldn't be read".into()),
                }
                continue;
            }
            if let Some(stem) = name.strip_suffix(".ncraft") {
                if !names.iter().any(|other| other.strip_suffix(".json") == Some(stem)) {
                    stored.problems.push(format!("{name} in the template cache has no record; it was ignored"));
                }
                continue;
            }
            let Some(stem) = name.strip_suffix(".json") else {
                stored.problems.push(format!("Ignored unexpected file {name} in the template cache"));
                continue;
            };
            let record = match read_capped(&dir.join(name), MAX_RECORD_BYTES) {
                Ok(Some(record)) => record,
                _ => {
                    stored.problems.push(format!("The template cache record {name} is damaged or too large"));
                    continue;
                }
            };
            let bytes = match read_capped(&dir.join(format!("{stem}.ncraft")), super::MAX_BYTES) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    stored.problems.push(format!("The cached template {stem}.ncraft is larger than its limit"));
                    continue;
                }
                Err(_) => {
                    stored.problems.push(format!("The cached template {stem}.ncraft is missing or unreadable"));
                    continue;
                }
            };
            total = total.saturating_add(bytes.len());
            if total > catalog::MAX_CACHE_BYTES {
                stored.problems.push("The template cache is larger than its limit; the rest was ignored".into());
                break;
            }
            stored.archives.push((stem.to_owned(), record, bytes));
        }
        Ok(stored)
    }
    fn write_index(&self, bytes: &[u8]) -> Result<(), Error> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&dir.join("index.json"), bytes).map_err(|_| unwritable())
    }
    fn write_archive(&self, name: &str, record: &[u8], bytes: &[u8]) -> Result<(), Error> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&dir.join(format!("{name}.ncraft")), bytes).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&dir.join(format!("{name}.json")), record).map_err(|_| unwritable())
    }
    fn remove_archive(&self, name: &str) -> Result<(), Error> {
        let dir = self.dir();
        for file in [format!("{name}.json"), format!("{name}.ncraft")] {
            match std::fs::remove_file(dir.join(file)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unwritable()),
            }
        }
        Ok(())
    }
    fn clear(&self) -> Result<(), Error> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::new(ErrorKind::Internal, "The template cache couldn't be removed; close other NodeCraft windows and try again")),
        }
    }
    fn persistent(&self) -> bool {
        true
    }
    fn location(&self) -> Option<String> {
        Some(self.dir().display().to_string())
    }
}

/// The session's cache: the store and what is wrong with it.
pub(super) struct Cache {
    pub(super) store: Box<dyn TemplateCacheStore>,
    pub(super) problems: Vec<String>,
}
impl Cache {
    fn report(&mut self, problem: String) {
        if self.problems.len() < MAX_PROBLEMS && !self.problems.contains(&problem) {
            self.problems.push(problem);
        }
    }
}

/// A store name is a lowercase hex BLAKE3 digest, so it can't name another path.
fn valid_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Gives the session its template cache and loads what it holds, verifying every file again.
/// Reads only local storage, never the network. Returns the problems found, which
/// `templates.status` keeps reporting; the bundled set is always available.
pub fn install(session: &mut Session, store: Box<dyn TemplateCacheStore>) -> Vec<String> {
    let mut cache = Cache { store, problems: Vec::new() };
    let stored = match cache.store.read() {
        Ok(stored) => stored,
        Err(error) => {
            cache.report(error.message);
            Stored::default()
        }
    };
    for problem in stored.problems {
        cache.report(problem);
    }
    let restored = catalog::update(session, |catalog| {
        let mut problems = Vec::new();
        let mut evicted = Vec::new();
        for (name, record, bytes) in stored.archives {
            let entry = match record_entry(&name, &record) {
                Ok(entry) => entry,
                Err(_) => {
                    problems.push(format!("The template cache record {name}.json is damaged; it was ignored"));
                    continue;
                }
            };
            match catalog.restore(entry, bytes) {
                Ok(mut gone) => evicted.append(&mut gone),
                Err(_) => problems.push(format!("The cached template {name}.ncraft doesn't match its digest or is damaged; it was ignored")),
            }
        }
        if let Some(index) = stored.index
            && catalog.install_index(&index).is_err()
        {
            problems.push("The cached template index is damaged or out of date; it was ignored".into());
        }
        Ok((problems, evicted))
    });
    match restored {
        Ok((problems, evicted)) => {
            for problem in problems {
                cache.report(problem);
            }
            for entry in evicted {
                let _ = cache.store.remove_archive(&entry.blake3);
            }
        }
        Err(error) => cache.report(error.message),
    }
    let problems = cache.problems.clone();
    session.swap_resource(Some(cache));
    problems
}

/// The index record stored beside an archive: one entry, named by its digest.
fn record_entry(name: &str, record: &[u8]) -> Result<Entry, Error> {
    let entry: Entry = serde_json::from_slice(record).map_err(|_| super::invalid())?;
    let checked = parse_index(&serde_json::to_vec(&json!({"schema_version":1,"templates":[entry]})).map_err(|_| super::invalid())?)?;
    let entry = checked.into_iter().next().ok_or_else(super::invalid)?;
    if !valid_name(name) || entry.blake3 != name {
        return Err(super::invalid());
    }
    Ok(entry)
}

/// What is wrong with the session's template cache, for a host's status line.
pub fn problems(session: &Session) -> &[String] {
    session.resource::<Cache>().map_or(&[], |cache| cache.problems.as_slice())
}
pub(super) fn persistent(session: &Session) -> bool {
    session.resource::<Cache>().is_some()
}
fn with_cache(session: &mut Session, f: impl FnOnce(&dyn TemplateCacheStore) -> Result<(), Error>) {
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return };
    if let Err(error) = f(cache.store.as_ref()) {
        cache.report(error.message);
    }
    session.swap_resource(Some(cache));
}
pub(super) fn persist_index(session: &mut Session, bytes: &[u8]) {
    with_cache(session, |store| store.write_index(bytes));
}
pub(super) fn persist_archive(session: &mut Session, entry: &Entry, bytes: &[u8], evicted: &[Entry]) {
    with_cache(session, |store| {
        if !valid_name(&entry.blake3) {
            return Err(super::invalid());
        }
        let record = serde_json::to_vec(entry).map_err(|_| super::invalid())?;
        store.write_archive(&entry.blake3, &record, bytes)?;
        for old in evicted {
            if valid_name(&old.blake3) {
                store.remove_archive(&old.blake3)?;
            }
        }
        Ok(())
    });
}
/// Removes every downloaded template, on disk and in the session; bundled ones stay.
pub(super) fn clear(session: &mut Session) -> Result<(), Error> {
    catalog::update(session, catalog::Catalog::clear_cached)?;
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return Ok(()) };
    let cleared = cache.store.clear();
    if cleared.is_ok() {
        cache.problems.clear();
    }
    session.swap_resource(Some(cache));
    cleared
}

/// What `templates.status` says about the cache.
pub(super) fn report(resources: &crate::commands::Resources) -> serde_json::Value {
    let catalog = resources.get::<catalog::Catalog>();
    let files = catalog.map_or(0, catalog::Catalog::cached_count);
    let bytes = catalog.map_or(0, catalog::Catalog::cached_bytes);
    match resources.get::<Cache>() {
        Some(cache) => json!({
            "layout": LAYOUT,
            "persistent": cache.store.persistent(),
            "location": cache.store.location(),
            "files": files,
            "bytes": bytes,
            "problems": cache.problems,
        }),
        None => json!({"layout": LAYOUT, "persistent": false, "location": null, "files": files, "bytes": bytes, "problems": []}),
    }
}
