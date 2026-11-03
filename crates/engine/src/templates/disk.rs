//! The persistent verified template cache (#954): the last good index and every verified archive,
//! under the app data directory in a versioned folder. The store only moves bounded bytes; every
//! byte read back is verified again ([`load`]) before it is listed, so a damaged cache is reported
//! and skipped, never trusted and never a panic. Browser and test hosts keep it in memory.
//!
//! A desktop host keeps the disk off the UI thread: [`install_deferred`] gives the session a cache
//! whose reads and writes the host runs on its worker. The worker calls [`load`] and hands the
//! verified [`Loaded`] back to [`adopt`]; the session queues every write as a [`Write`] that the
//! host drains with [`take_writes`], applies with [`Write::apply`] and reports with [`wrote`].
//! [`install`] does all of that inline, for hosts without a worker.
use super::{Entry, catalog, parse_index};
use crate::Session;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

/// The cache layout's version: its folder name. A later layout uses a new folder, and an older
/// build ignores it.
pub const LAYOUT: &str = "v1";
/// Most files one cache read admits: an archive and its record per cached template (at most
/// 128), plus the index and its signature.
pub const MAX_FILES: usize = 2 * 128 + 2;
/// Largest index record (`<blake3>.json`) read back.
pub const MAX_RECORD_BYTES: usize = 4 * 1024;
/// Largest index signature read back.
const MAX_SIGNATURE_BYTES: usize = 128;
/// Most cache problems kept for `templates.status`.
const MAX_PROBLEMS: usize = 32;
/// Most writes a session queues before the host drains them; more means the host has stopped.
const MAX_PENDING: usize = 512;

/// What a store read back, before verification.
#[derive(Default)]
pub struct Stored {
    pub index: Option<Vec<u8>>,
    /// The index's signature, when one was stored with it.
    pub signature: Option<Vec<u8>>,
    /// `(blake3 name, record bytes, archive bytes)`.
    pub archives: Vec<(String, Vec<u8>, Vec<u8>)>,
    /// Files that could not be read back, in words a person can act on.
    pub problems: Vec<String>,
    /// Digest names whose archive or record is missing: a write or removal cut off (by a crash or
    /// an exit mid-write). [`load`] removes what is left of them instead of reporting them forever.
    pub incomplete: Vec<String>,
}

/// Where verified templates are kept between launches.
pub trait TemplateCacheStore {
    /// Every file, bounded by [`MAX_FILES`], [`catalog::MAX_CACHE_BYTES`] and the per-file caps.
    fn read(&self) -> Result<Stored, Error>;
    /// Stores the index, and its signature or no signature.
    fn write_index(&self, bytes: &[u8], signature: Option<&[u8]>) -> Result<(), Error>;
    /// Stores an archive and then its record; the record's presence commits the pair.
    fn write_archive(&self, name: &str, record: &[u8], bytes: &[u8]) -> Result<(), Error>;
    fn remove_archive(&self, name: &str) -> Result<(), Error>;
    /// Removes this layout's cache.
    fn clear(&self) -> Result<(), Error>;
    /// Whether this store survives a restart.
    fn persistent(&self) -> bool;
    /// The cache folder, when it is one.
    fn location(&self) -> Option<String>;
}

/// A cache in memory: the browser and tests.
#[derive(Clone, Default)]
pub struct MemoryCache(Rc<RefCell<Held>>);
/// The index and its signature, and each archive's `(record, bytes)` by name.
type Held = (Option<(Vec<u8>, Option<Vec<u8>>)>, BTreeMap<String, (Vec<u8>, Vec<u8>)>);

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "The template cache is busy; try again")
}
impl TemplateCacheStore for MemoryCache {
    fn read(&self) -> Result<Stored, Error> {
        let held = self.0.try_borrow().map_err(|_| busy())?;
        Ok(Stored {
            index: held.0.as_ref().map(|(index, _)| index.clone()),
            signature: held.0.as_ref().and_then(|(_, signature)| signature.clone()),
            archives: held.1.iter().map(|(name, (record, bytes))| (name.clone(), record.clone(), bytes.clone())).collect(),
            problems: Vec::new(),
            incomplete: Vec::new(),
        })
    }
    fn write_index(&self, bytes: &[u8], signature: Option<&[u8]>) -> Result<(), Error> {
        self.0.try_borrow_mut().map_err(|_| busy())?.0 = Some((bytes.to_vec(), signature.map(<[u8]>::to_vec)));
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

/// The desktop cache: `<app data>/templates/v1/`. Only a path, so a host can hand a copy to its
/// worker thread.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct FileCache {
    dir: std::path::PathBuf,
}
#[cfg(not(target_arch = "wasm32"))]
impl FileCache {
    /// The cache under `data` (the settings folder): `data/templates/v1`.
    pub fn new(data: &std::path::Path) -> Self {
        Self { dir: data.join("templates").join(LAYOUT) }
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn unwritable() -> Error {
    Error::new(ErrorKind::Internal, "The template cache couldn't be written (is the disk full or the folder read-only?)")
}
/// Reads an ordinary file (never through a symlink) up to `cap` bytes; `None` when it is larger.
#[cfg(not(target_arch = "wasm32"))]
fn read_capped(path: &std::path::Path, cap: usize) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    if !std::fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not an ordinary file"));
    }
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1)).read_to_end(&mut bytes)?;
    Ok((bytes.len() <= cap).then_some(bytes))
}
#[cfg(not(target_arch = "wasm32"))]
impl TemplateCacheStore for FileCache {
    fn read(&self) -> Result<Stored, Error> {
        let dir = &self.dir;
        let mut stored = Stored::default();
        let listing = match std::fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(stored),
            Err(_) => return Err(Error::new(ErrorKind::Internal, "The template cache folder couldn't be read")),
        };
        let mut names = Vec::new();
        // Bounds the work a huge folder can cause even when everything in it is skipped.
        for item in listing.take(MAX_FILES.saturating_mul(4)) {
            let Ok(item) = item else {
                stored.problems.push("A template cache entry couldn't be read".into());
                continue;
            };
            let name = item.file_name().to_string_lossy().into_owned();
            // Hidden files and atomic-write temporaries are not cache content and use no slot.
            if name.starts_with('.') || name.ends_with(".tmp") {
                continue;
            }
            let expected = name.ends_with(".json") || name.ends_with(".ncraft") || name == "index.json.sig";
            match item.file_type() {
                // Only names the cache could hold use a slot.
                Ok(kind) if kind.is_file() && !expected => stored.problems.push(format!("Ignored unexpected file {name} in the template cache")),
                Ok(kind) if kind.is_file() => {
                    if names.len() >= MAX_FILES {
                        stored.problems.push(format!("The template cache holds more than {MAX_FILES} files; the rest were ignored"));
                        break;
                    }
                    names.push(name);
                }
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
            if name == "index.json.sig" {
                match read_capped(&dir.join(name), MAX_SIGNATURE_BYTES) {
                    Ok(Some(bytes)) => stored.signature = Some(bytes),
                    _ => stored.problems.push("The cached template index signature couldn't be read".into()),
                }
                continue;
            }
            if let Some(stem) = name.strip_suffix(".ncraft") {
                if !names.iter().any(|other| other.strip_suffix(".json") == Some(stem)) {
                    if valid_name(stem) {
                        stored.incomplete.push(stem.to_owned());
                    } else {
                        stored.problems.push(format!("{name} in the template cache has no record; it was ignored"));
                    }
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
            let partner = dir.join(format!("{stem}.ncraft"));
            if valid_name(stem) && matches!(std::fs::symlink_metadata(&partner), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
                stored.incomplete.push(stem.to_owned());
                continue;
            }
            let bytes = match read_capped(&partner, super::MAX_BYTES) {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    stored.problems.push(format!("The cached template {stem}.ncraft is larger than its limit"));
                    continue;
                }
                Err(_) => {
                    stored.problems.push(format!("The cached template {stem}.ncraft is missing, unreadable or not an ordinary file"));
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
    fn write_index(&self, bytes: &[u8], signature: Option<&[u8]>) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| unwritable())?;
        let sig = self.dir.join("index.json.sig");
        match signature {
            // The signature first: a crash between the two leaves an index that fails to verify
            // in signed mode, never an old signature beside a new index that happens to pass.
            Some(signature) => craft_atomic::atomic_write(&sig, signature).map_err(|_| unwritable())?,
            None => match std::fs::remove_file(&sig) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unwritable()),
            },
        }
        craft_atomic::atomic_write(&self.dir.join("index.json"), bytes).map_err(|_| unwritable())
    }
    fn write_archive(&self, name: &str, record: &[u8], bytes: &[u8]) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&self.dir.join(format!("{name}.ncraft")), bytes).map_err(|_| unwritable())?;
        craft_atomic::atomic_write(&self.dir.join(format!("{name}.json")), record).map_err(|_| unwritable())
    }
    fn remove_archive(&self, name: &str) -> Result<(), Error> {
        for file in [format!("{name}.json"), format!("{name}.ncraft")] {
            match std::fs::remove_file(self.dir.join(file)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unwritable()),
            }
        }
        Ok(())
    }
    fn clear(&self) -> Result<(), Error> {
        // Only this layout's folder: another layout belongs to another build.
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::new(ErrorKind::Internal, "The template cache couldn't be removed; close other NodeCraft windows and try again")),
        }
    }
    fn persistent(&self) -> bool {
        true
    }
    fn location(&self) -> Option<String> {
        Some(self.dir.display().to_string())
    }
}

/// One change to the store, queued by the session and applied by the host (on its worker).
#[derive(Clone, Debug)]
pub enum Write {
    Index { bytes: Vec<u8>, signature: Option<Vec<u8>> },
    Archive { name: String, record: Vec<u8>, bytes: Vec<u8> },
    Remove { name: String },
    Clear,
}
impl Write {
    pub fn apply(&self, store: &dyn TemplateCacheStore) -> Result<(), Error> {
        match self {
            Self::Index { bytes, signature } => store.write_index(bytes, signature.as_deref()),
            Self::Archive { name, record, bytes } => store.write_archive(name, record, bytes),
            Self::Remove { name } => store.remove_archive(name),
            Self::Clear => store.clear(),
        }
    }
}

/// The session's cache: the store (inline) or the host's worker (deferred), and what is wrong.
pub(super) struct Cache {
    /// `None` when the host's worker owns the store.
    store: Option<Box<dyn TemplateCacheStore>>,
    persistent: bool,
    location: Option<String>,
    /// The host's worker hasn't handed back the startup read yet.
    loading: bool,
    /// The host's worker hasn't finished a queued clear yet.
    clearing: bool,
    pending: Vec<Write>,
    problems: Vec<String>,
}
impl Cache {
    fn report(&mut self, problem: String) {
        if self.problems.len() < MAX_PROBLEMS && !self.problems.contains(&problem) {
            self.problems.push(problem);
        }
    }
    /// Applies `write` inline, or queues it for the host's worker.
    fn write(&mut self, write: Write) {
        match &self.store {
            Some(store) => {
                if let Err(error) = write.apply(store.as_ref()) {
                    self.report(error.message);
                }
            }
            None if self.pending.len() < MAX_PENDING => self.pending.push(write),
            None => self.report("Downloaded templates couldn't be saved: the template worker isn't keeping up".into()),
        }
    }
}

/// A store name is a lowercase hex BLAKE3 digest, so it can't name another path.
fn valid_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The cache as read back and verified, ready for [`adopt`]. Only [`load`] makes one, so adopting
/// it on the UI thread needn't hash or decode anything again.
pub struct Loaded {
    index: Option<(Vec<u8>, Option<Vec<u8>>)>,
    archives: Vec<(Entry, Vec<u8>)>,
    problems: Vec<String>,
}
impl Loaded {
    /// A read that couldn't finish (the worker stopped or panicked): nothing cached is used, the
    /// bundled set stays, and `problem` is reported. Adopting it ends the startup wait.
    pub fn failed(problem: impl Into<String>) -> Self {
        Self { index: None, archives: Vec::new(), problems: vec![problem.into()] }
    }
}

/// Reads and verifies the cache: each archive against the digest its record names, its graph,
/// assets and node kinds; the index's schema and, when `keys` is not empty, its stored signature.
/// The expensive half of startup, which a host runs on its worker. Never touches the network.
pub fn load(store: &dyn TemplateCacheStore, keys: &[ed25519_dalek::VerifyingKey]) -> Loaded {
    let mut loaded = Loaded { index: None, archives: Vec::new(), problems: Vec::new() };
    let stored = match store.read() {
        Ok(stored) => stored,
        Err(error) => {
            loaded.problems.push(error.message);
            return loaded;
        }
    };
    loaded.problems.extend(stored.problems);
    for name in stored.incomplete {
        if store.remove_archive(&name).is_err() {
            loaded.problems.push(format!("An incomplete cached template ({name}) couldn't be removed"));
        }
    }
    if let Some(index) = stored.index {
        let signed = keys.is_empty() || stored.signature.as_deref().is_some_and(|signature| super::update::signature_valid(keys, &index, signature));
        if !signed {
            loaded.problems.push("The cached template index isn't signed by a trusted key; it was ignored".into());
        } else if parse_index(&index).is_err() {
            loaded.problems.push("The cached template index is damaged or out of date; it was ignored".into());
        } else {
            loaded.index = Some((index, stored.signature));
        }
    }
    let kinds = crate::generation_catalog::default_registry().map(|(kinds, _)| kinds);
    for (name, record, bytes) in stored.archives {
        let Ok(entry) = record_entry(&name, &record) else {
            loaded.problems.push(format!("The template cache record {name}.json is damaged; it was ignored"));
            continue;
        };
        let verified = super::validate_archive(&entry, &bytes)
            .and_then(|snapshot| kinds.as_ref().map_err(Clone::clone).and_then(|kinds| super::validate_nodes(&snapshot, kinds)));
        match verified {
            Ok(()) => loaded.archives.push((entry, bytes)),
            Err(_) => loaded.problems.push(format!("The cached template {name}.ncraft doesn't match its digest or is damaged; it was ignored")),
        }
    }
    loaded
}

/// Installs a verified [`Loaded`]: the cached index first, then each archive, dropping any that
/// contradicts the index, a bundled version or another cached copy (the index wins).
pub fn adopt(session: &mut Session, loaded: Loaded) -> Vec<String> {
    let mut cache = session.swap_resource::<Cache>(None).unwrap_or_else(|| Cache {
        store: Some(Box::new(MemoryCache::default())),
        persistent: false,
        location: None,
        loading: false,
        clearing: false,
        pending: Vec::new(),
        problems: Vec::new(),
    });
    cache.loading = false;
    for problem in loaded.problems {
        cache.report(problem);
    }
    let restored = catalog::update(session, |catalog| {
        let mut problems = Vec::new();
        let mut gone = Vec::new();
        if let Some((index, _)) = &loaded.index
            && catalog.install_index(index).is_err()
        {
            problems.push("The cached template index contradicts the bundled templates; it was ignored".into());
        }
        for (entry, bytes) in loaded.archives {
            let name = entry.blake3.clone();
            match catalog.restore_verified(entry.clone(), bytes) {
                Ok(mut evicted) => gone.append(&mut evicted),
                // Only a contradiction removes the file; anything else leaves it for a later start.
                Err(error) if error.kind == ErrorKind::Conflict => {
                    problems.push(format!(
                        "The cached template {name}.ncraft contradicts the published index; it was removed. Use Clear downloaded if this repeats"
                    ));
                    gone.push(entry);
                }
                Err(error) => problems.push(format!("The cached template {name}.ncraft couldn't be restored: {}", error.message)),
            }
        }
        Ok((problems, gone))
    });
    match restored {
        Ok((problems, gone)) => {
            for problem in problems {
                cache.report(problem);
            }
            for entry in gone {
                if valid_name(&entry.blake3) {
                    cache.write(Write::Remove { name: entry.blake3 });
                }
            }
        }
        Err(error) => cache.report(error.message),
    }
    let problems = cache.problems.clone();
    session.swap_resource(Some(cache));
    problems
}

/// Gives the session its template cache and loads what it holds inline, verifying every file
/// again. For hosts without a worker (the browser, tests); a desktop host uses
/// [`install_deferred`]. Reads only local storage, never the network. Returns the problems found,
/// which `templates.status` keeps reporting; the bundled set is always available.
pub fn install(session: &mut Session, store: Box<dyn TemplateCacheStore>) -> Vec<String> {
    install_keyed(session, store, &[])
}
/// [`install`], requiring the cached index to carry a signature from one of `keys`.
pub fn install_keyed(session: &mut Session, store: Box<dyn TemplateCacheStore>, keys: &[ed25519_dalek::VerifyingKey]) -> Vec<String> {
    let loaded = load(store.as_ref(), keys);
    let cache = Cache {
        persistent: store.persistent(),
        location: store.location(),
        store: Some(store),
        loading: false,
        clearing: false,
        pending: Vec::new(),
        problems: Vec::new(),
    };
    session.swap_resource(Some(cache));
    adopt(session, loaded)
}
/// Gives the session a cache whose store the host's worker owns: the host runs [`load`] there
/// and hands the result to [`adopt`], and drains writes with [`take_writes`]. Until it adopts,
/// `templates.update` waits (it would download what the cache already holds).
pub fn install_deferred(session: &mut Session, persistent: bool, location: Option<String>) {
    session.swap_resource(Some(Cache { store: None, persistent, location, loading: true, clearing: false, pending: Vec::new(), problems: Vec::new() }));
}
/// The writes queued since the last call, for the host's worker, in order.
pub fn take_writes(session: &mut Session) -> Vec<Write> {
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return Vec::new() };
    let writes = std::mem::take(&mut cache.pending);
    session.swap_resource(Some(cache));
    writes
}
/// Problems the host's worker met applying writes.
pub fn wrote(session: &mut Session, problems: Vec<String>) {
    if problems.is_empty() {
        return;
    }
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return };
    for problem in problems {
        cache.report(problem);
    }
    session.swap_resource(Some(cache));
}

/// The host's worker finished a queued clear: `Err` holds why the folder couldn't be removed.
pub fn cleared(session: &mut Session, result: Result<(), String>) {
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return };
    cache.clearing = false;
    if let Err(problem) = result {
        cache.report(problem);
    }
    session.swap_resource(Some(cache));
}
/// Whether a clear is still running on the host's worker.
pub fn is_clearing(session: &Session) -> bool {
    session.resource::<Cache>().is_some_and(|cache| cache.clearing)
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
/// Whether the host's worker is still reading this session's cache.
pub fn is_loading(session: &Session) -> bool {
    session.resource::<Cache>().is_some_and(|cache| cache.loading)
}
/// Whether the host's worker is still reading the cache at startup.
pub(super) fn loading(resources: &crate::commands::Resources) -> bool {
    resources.get::<Cache>().is_some_and(|cache| cache.loading)
}
/// Whether the session's cache outlives it, so downloads are worth writing (an in-memory cache
/// would only duplicate what the catalog already holds).
pub(super) fn persistent(session: &Session) -> bool {
    session.resource::<Cache>().is_some_and(|cache| cache.persistent)
}
fn with_cache(session: &mut Session, f: impl FnOnce(&mut Cache)) {
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return };
    if !cache.persistent {
        session.swap_resource(Some(cache));
        return;
    }
    f(&mut cache);
    session.swap_resource(Some(cache));
}
pub(super) fn persist_index(session: &mut Session, bytes: &[u8], signature: Option<&[u8]>) {
    with_cache(session, |cache| cache.write(Write::Index { bytes: bytes.to_vec(), signature: signature.map(<[u8]>::to_vec) }));
}
pub(super) fn persist_archive(session: &mut Session, entry: &Entry, bytes: Vec<u8>, evicted: &[Entry]) {
    with_cache(session, |cache| {
        let Ok(record) = serde_json::to_vec(entry) else {
            cache.report("A downloaded template couldn't be saved".into());
            return;
        };
        if valid_name(&entry.blake3) {
            cache.write(Write::Archive { name: entry.blake3.clone(), record, bytes });
        }
        for old in evicted {
            if valid_name(&old.blake3) {
                cache.write(Write::Remove { name: old.blake3.clone() });
            }
        }
    });
}
/// Removes every downloaded template, on disk and in the session; bundled ones stay.
pub(super) fn clear(session: &mut Session) -> Result<(), Error> {
    catalog::update(session, catalog::Catalog::clear_cached)?;
    let Some(mut cache) = session.swap_resource::<Cache>(None) else { return Ok(()) };
    cache.problems.clear();
    cache.pending.clear();
    // With a worker the removal finishes later; [`cleared`] reports how it went.
    cache.clearing = cache.store.is_none();
    cache.write(Write::Clear);
    let failed = cache.problems.first().cloned();
    session.swap_resource(Some(cache));
    match failed {
        Some(problem) => Err(Error::new(ErrorKind::Internal, problem)),
        None => Ok(()),
    }
}

/// What `templates.status` says about the cache.
pub(super) fn report(resources: &crate::commands::Resources) -> serde_json::Value {
    let catalog = resources.get::<catalog::Catalog>();
    let files = catalog.map_or(0, catalog::Catalog::cached_count);
    let bytes = catalog.map_or(0, catalog::Catalog::cached_bytes);
    match resources.get::<Cache>() {
        Some(cache) => json!({
            "layout": LAYOUT,
            "persistent": cache.persistent,
            "location": cache.location,
            "loading": cache.loading,
            "clearing": cache.clearing,
            "files": files,
            "bytes": bytes,
            "problems": cache.problems,
        }),
        None => {
            json!({"layout": LAYOUT, "persistent": false, "location": null, "loading": false, "clearing": false, "files": files, "bytes": bytes, "problems": []})
        }
    }
}
