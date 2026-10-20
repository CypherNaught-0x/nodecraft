//! The technique library (#96): published technique versions kept outside any project, so one
//! can be placed in every project (`docs/decisions/2026-10-20-technique-library.md`).
//!
//! The library is one file of version canvases ([`Library`]) in the host's data folder
//! ([`FileLibrary`]); the browser keeps it in memory ([`MemoryLibrary`]). The host's `library`
//! service ([`install`]) reads and writes it, re-reading before every change so two windows (or
//! the app and the CLI) never lose each other's additions, and keeps the last copy it read on the session's
//! [`Shelf`], which `library.place` reads without touching storage.
use crate::session::Session;
use nodecraft_graph::{Canvas, Technique};
use nodecraft_types::{Error, ErrorKind, TechniqueId, Value};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// The host service the `library.*` commands ask.
pub const LIBRARY_SERVICE: &str = "library.store";
/// The library file's format version.
pub const LIBRARY_VERSION: u32 = 1;
/// Largest library file read back or written.
pub const MAX_LIBRARY_BYTES: usize = 64 * 1024 * 1024;
/// Most versions the library holds.
pub const MAX_LIBRARY_VERSIONS: usize = 1024;
/// Largest version, as JSON, that a command hands to the service.
pub const MAX_ENTRY_BYTES: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryFile {
    version: u32,
    #[serde(default)]
    techniques: Vec<Canvas>,
}

/// The versions in the library, one per technique and version number.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Library {
    versions: BTreeMap<(TechniqueId, u32), Canvas>,
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

impl Library {
    /// A library file, checked: every version is a whole published version, once.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_LIBRARY_BYTES {
            return Err(invalid("the technique library file is too large"));
        }
        let file: LibraryFile = serde_json::from_slice(bytes).map_err(|_| invalid("the technique library file is damaged"))?;
        if file.version != LIBRARY_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, "the technique library was written by a newer NodeCraft"));
        }
        if file.techniques.len() > MAX_LIBRARY_VERSIONS {
            return Err(invalid("the technique library holds too many versions"));
        }
        let mut library = Self::default();
        for canvas in file.techniques {
            if !library.insert(checked(canvas)?) {
                return Err(invalid("the technique library holds one version twice"));
            }
        }
        Ok(library)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let file = LibraryFile { version: LIBRARY_VERSION, techniques: self.versions.values().cloned().collect() };
        let bytes = serde_json::to_vec(&file).map_err(|_| Error::new(ErrorKind::Internal, "the technique library couldn't be written"))?;
        if bytes.len() > MAX_LIBRARY_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, "the technique library would be too large; remove versions you no longer use"));
        }
        Ok(bytes)
    }

    pub fn get(&self, technique: TechniqueId, version: u32) -> Option<&Canvas> {
        self.versions.get(&(technique, version))
    }

    /// Every version, by technique and then version number.
    pub fn versions(&self) -> impl Iterator<Item = &Canvas> {
        self.versions.values()
    }

    /// Adds `canvas` unless its version is already there; whether it was added.
    fn insert(&mut self, canvas: Canvas) -> bool {
        let Some(key) = canvas.technique_info().map(|info| (info.id, info.version)) else { return false };
        if self.versions.contains_key(&key) {
            return false;
        }
        self.versions.insert(key, canvas);
        true
    }
}

/// `canvas` if it is a published version that isn't removed and keeps the library's rules (see
/// [`entry`]), so a file edited by hand or by another build can't bring in what adding refuses.
fn checked(canvas: Canvas) -> Result<Canvas, Error> {
    let info = canvas.technique_info().filter(|info| !info.retired).ok_or_else(|| invalid("only a published technique version goes in the library"))?;
    if info.source.is_some() {
        return Err(invalid("a library version doesn't name the canvas it was published from"));
    }
    for node in canvas.nodes().values() {
        if node.kind == nodecraft_nodes::technique::ID {
            return Err(nested());
        }
        if node.params.values().chain(node.config.values()).any(holds_media) {
            return Err(media());
        }
        let input = info.inputs.iter().any(|input| input.node == node.id);
        let example = match node.history.as_slice() {
            [] => node.active.is_none(),
            [kept] => input && node.active == Some(kept.id) && kept.settings.is_empty(),
            _ => false,
        };
        if !example {
            return Err(invalid("a library version keeps only its inputs' examples"));
        }
        if node.history.iter().flat_map(|kept| &kept.outputs).any(holds_media) {
            return Err(media());
        }
    }
    // Rebuilt through the constructor, which checks the version as a whole.
    Canvas::technique(canvas.id(), canvas.title(), info.clone(), canvas.nodes().values().cloned().collect(), canvas.edges().values().cloned().collect())
}

fn holds_media(value: &Value) -> bool {
    match value {
        Value::Media(_) => true,
        Value::List(items) => items.iter().any(holds_media),
        _ => false,
    }
}

fn media() -> Error {
    Error::new(ErrorKind::Unsupported, "a version holding images, video or other media can't go in the library yet")
}

fn nested() -> Error {
    Error::new(ErrorKind::Unsupported, "a version that runs other techniques can't go in the library yet")
}

/// The copy of a project's version that goes in the library. The canvas it was published from
/// and its nodes' results stay in the project, except each input's example, which a placed node
/// uses while nothing feeds that input (without the settings it was made with). A version holding
/// media can't go yet, as its files belong to the project; nor can one running other techniques,
/// whose versions would have to come along.
pub fn entry(canvas: &Canvas) -> Result<Canvas, Error> {
    let info = canvas.technique_info().filter(|info| !info.retired).ok_or_else(|| invalid("that canvas isn't a published technique version"))?;
    let mut nodes = Vec::with_capacity(canvas.nodes().len());
    for node in canvas.nodes().values() {
        let mut node = node.clone();
        let example = info
            .inputs
            .iter()
            .any(|input| input.node == node.id)
            .then(|| node.active.and_then(|active| node.history.iter().find(|g| g.id == active).cloned()))
            .flatten();
        node.history = example
            .map(|mut kept| {
                kept.settings.clear();
                kept
            })
            .into_iter()
            .collect();
        node.active = node.history.first().map(|kept| kept.id);
        nodes.push(node);
    }
    let info = Technique { source: None, ..info.clone() };
    checked(Canvas::technique(canvas.id(), canvas.title(), info, nodes, canvas.edges().values().cloned().collect())?)
}

/// What the library says about one version.
pub fn summary(canvas: &Canvas) -> Json {
    let Some(info) = canvas.technique_info() else { return Json::Null };
    let names = |ports: &[nodecraft_graph::TechniquePort]| ports.iter().map(|port| port.name.clone()).collect::<Vec<_>>();
    json!({"technique": info.id, "version": info.version, "name": info.name, "description": info.description, "category": info.category,
        "tags": info.tags, "inputs": names(&info.inputs), "outputs": names(&info.outputs), "nodes": canvas.nodes().len()})
}

/// Where a host keeps its library.
pub trait LibraryStore {
    /// The library file's bytes, or `None` when there is none yet.
    fn load(&self) -> Result<Option<Vec<u8>>, Error>;
    fn save(&self, bytes: &[u8]) -> Result<(), Error>;
    /// Held while a change reads, edits and writes the library, so another process's change
    /// can't be lost between them. Busy while another holds it.
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        Ok(Box::new(()))
    }
}

/// A library in memory: tests, and the browser, where it lasts for the session.
#[derive(Clone, Default)]
pub struct MemoryLibrary(Rc<RefCell<Option<Vec<u8>>>>);

impl MemoryLibrary {
    /// The library as last saved.
    pub fn saved(&self) -> Option<Library> {
        self.0.try_borrow().ok()?.as_deref().and_then(|bytes| Library::from_bytes(bytes).ok())
    }
}

impl LibraryStore for MemoryLibrary {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.0.try_borrow().map_err(|_| busy())?.clone())
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        *self.0.try_borrow_mut().map_err(|_| busy())? = Some(bytes.to_vec());
        Ok(())
    }
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the technique library is busy; try again")
}

/// The library as `techniques.json` in a folder, replaced atomically so a crash or a full disk
/// keeps the last whole file; `.lock` beside it orders changes between processes.
#[cfg(not(target_arch = "wasm32"))]
pub struct FileLibrary {
    dir: std::path::PathBuf,
}

#[cfg(not(target_arch = "wasm32"))]
impl FileLibrary {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self { dir }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl LibraryStore for FileLibrary {
    fn load(&self) -> Result<Option<Vec<u8>>, Error> {
        use std::io::Read;
        let file = match std::fs::File::open(self.dir.join("techniques.json")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::new(ErrorKind::Internal, "the technique library can't be read")),
        };
        let mut bytes = Vec::new();
        let cap = u64::try_from(MAX_LIBRARY_BYTES).unwrap_or(u64::MAX).saturating_add(1);
        file.take(cap).read_to_end(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "the technique library can't be read"))?;
        Ok(Some(bytes))
    }
    fn save(&self, bytes: &[u8]) -> Result<(), Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| Error::new(ErrorKind::Internal, "the technique library's folder can't be made"))?;
        craft_atomic::atomic_write(&self.dir.join("techniques.json"), bytes)
            .map_err(|_| Error::new(ErrorKind::Internal, "the technique library couldn't be saved (is the disk full?)"))
    }
    fn lock(&self) -> Result<Box<dyn std::any::Any>, Error> {
        std::fs::create_dir_all(&self.dir).map_err(|_| Error::new(ErrorKind::Internal, "the technique library's folder can't be made"))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join(".lock"))
            .map_err(|_| Error::new(ErrorKind::Internal, "the technique library's lock can't be opened"))?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => busy(),
            std::fs::TryLockError::Error(_) => Error::new(ErrorKind::Internal, "the technique library's lock failed"),
        })?;
        Ok(Box::new(file))
    }
}

/// The library as the service last read it, for `library.place`; why it couldn't be read, if it
/// couldn't.
#[derive(Clone)]
pub struct Shelf(Rc<RefCell<Result<Library, Error>>>);

impl Shelf {
    /// Calls `f` with the library last read.
    pub fn with<T>(&self, f: impl FnOnce(&Library) -> Result<T, Error>) -> Result<T, Error> {
        let shelved = self.0.try_borrow().map_err(|_| busy())?;
        match &*shelved {
            Ok(library) => f(library),
            Err(error) => Err(Error::new(error.kind, format!("the technique library isn't available: {}", error.message))),
        }
    }

    fn put(&self, read: Result<Library, Error>) {
        if let Ok(mut shelved) = self.0.try_borrow_mut() {
            *shelved = read;
        }
    }
}

fn read(store: &dyn LibraryStore) -> Result<Library, Error> {
    store.load()?.map_or_else(|| Ok(Library::default()), |bytes| Library::from_bytes(&bytes))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List {},
    /// `canvas` is the version's JSON, as [`entry`] made it.
    Add {
        canvas: String,
    },
    Remove {
        technique: TechniqueId,
        version: u32,
    },
}

/// Answers one `library` service request against `store`, and shelves what it read.
pub fn handle(store: &dyn LibraryStore, shelf: &Shelf, request: &Json) -> Result<Json, Error> {
    let request: Request = serde_json::from_value(request.clone()).map_err(|_| invalid("malformed technique library request"))?;
    if let Request::List {} = request {
        let read = read(store);
        shelf.put(read.clone());
        let library = read?;
        return Ok(json!({"techniques": library.versions().map(summary).collect::<Vec<_>>()}));
    }
    let _lock = store.lock()?;
    let mut library = read(store);
    shelf.put(library.clone());
    let library = library.as_mut().map_err(|e| e.clone())?;
    let result = match request {
        Request::List {} => return Err(Error::new(ErrorKind::Internal, "listing doesn't change the library")),
        Request::Add { canvas } => {
            if canvas.len() > MAX_ENTRY_BYTES {
                return Err(invalid("that version is too large for the library"));
            }
            let canvas: Canvas = serde_json::from_str(&canvas).map_err(|_| invalid("malformed technique version"))?;
            let canvas = checked(canvas)?;
            let described = summary(&canvas);
            let full = library.versions.len() >= MAX_LIBRARY_VERSIONS;
            if !library.insert(canvas) {
                // A version number names one content, so the one already there is this one.
                return Ok(json!({"added": false, "technique": described}));
            }
            if full {
                return Err(invalid("the technique library is full; remove versions you no longer use"));
            }
            json!({"added": true, "technique": described})
        }
        Request::Remove { technique, version } => {
            let removed = library.versions.remove(&(technique, version)).ok_or_else(|| Error::new(ErrorKind::NotFound, "that version isn't in the library"))?;
            json!({"removed": summary(&removed)})
        }
    };
    store.save(&library.to_bytes()?)?;
    shelf.put(Ok(library.clone()));
    Ok(result)
}

/// Gives `session` the library in `store`: the `library` service, and the shelf
/// `library.place` reads, filled now.
pub fn install(session: &mut Session, store: Box<dyn LibraryStore>) -> Result<(), Error> {
    let shelf = Shelf(Rc::new(RefCell::new(read(store.as_ref()))));
    let service = shelf.clone();
    session.register_host_service(LIBRARY_SERVICE, move |request| handle(store.as_ref(), &service, request))?;
    session.swap_resource(Some(shelf));
    Ok(())
}

#[cfg(test)]
mod tests;
