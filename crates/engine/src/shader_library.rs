//! The shader library (#90): custom shaders saved outside any project, each a named list of
//! numbered versions, so one can be placed in every project and a card can be moved to a newer
//! version (`docs/decisions/2026-10-23-shader-library.md`).
//!
//! The library is one file ([`ShaderLibrary`]) kept by a [`LibraryStore`]: `shaders.json` in the
//! host's data folder ([`file_store`]), or memory in the browser (a `MemoryLibrary`). The host's `shaders.store` service ([`install`])
//! reads and writes it, re-reading before every change so two windows never lose each other's
//! saves, and keeps the last copy it read on the session's [`ShaderShelf`], which placing and
//! updating read without touching storage. A version holds only the shader (WGSL and declared
//! parameters): no media, keys or project data.
use crate::library::LibraryStore;
use crate::session::Session;
use nodecraft_nodes::shader::{StoredShader, config_value};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// The host service the shader library commands ask.
pub const SHADERS_SERVICE: &str = "shaders.store";
/// The library file's format version.
pub const SHADERS_VERSION: u32 = 1;
/// Largest library file read back or written.
pub const MAX_SHADERS_BYTES: usize = 16 * 1024 * 1024;
/// Most shaders the library holds.
pub const MAX_SHADERS: usize = 1024;
/// Most versions one shader keeps.
pub const MAX_VERSIONS: usize = 64;
/// Longest shader name, in characters.
pub const MAX_NAME_CHARS: usize = 40;

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the shader library is busy; try again")
}

/// Whether `name` can name a library shader: 1 to 40 characters, no line breaks or other control
/// characters, and no space at either end.
pub fn valid_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAX_NAME_CHARS).contains(&count) && name.trim() == name && !name.chars().any(char::is_control)
}

/// One saved version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub version: u32,
    pub shader: StoredShader,
}

/// One named shader: its versions, oldest first, and the number the next one takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    /// Higher than every version this shader has had, so removing one never frees its number.
    pub next: u32,
    pub versions: Vec<Version>,
}

impl Entry {
    pub fn get(&self, version: u32) -> Option<&Version> {
        self.versions.iter().find(|kept| kept.version == version)
    }

    pub fn latest(&self) -> Option<&Version> {
        self.versions.last()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShadersFile {
    version: u32,
    #[serde(default)]
    shaders: Vec<Entry>,
}

/// The shaders in the library, by name ignoring case.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShaderLibrary {
    entries: BTreeMap<String, Entry>,
}

fn key(name: &str) -> String {
    name.to_lowercase()
}

/// `entry` if it keeps the library's rules, so a file edited by hand or by another build can't
/// bring in what saving refuses. The code itself is checked where it is placed, like any shader
/// a card is given.
fn checked(entry: Entry) -> Result<Entry, Error> {
    if !valid_name(&entry.name) {
        return Err(invalid("the shader library holds a shader without a valid name"));
    }
    if entry.versions.is_empty() || entry.versions.len() > MAX_VERSIONS {
        return Err(invalid("the shader library holds a shader with no versions or too many"));
    }
    let mut last = 0;
    for kept in &entry.versions {
        if kept.version <= last || kept.version >= entry.next {
            return Err(invalid("the shader library's version numbers are out of order"));
        }
        last = kept.version;
        bounded(&kept.shader)?;
    }
    Ok(entry)
}

/// Whether `shader` fits where a card keeps one.
fn bounded(shader: &StoredShader) -> Result<(), Error> {
    if shader.source.len() > nodecraft_gpu::shader::MAX_SOURCE || shader.params.len() > nodecraft_gpu::shader::MAX_PARAMS {
        return Err(invalid("the shader library holds a shader larger than a card can keep"));
    }
    config_value(shader).map(|_| ())
}

impl ShaderLibrary {
    /// A library file, checked: names unique ignoring case, versions in order and within limits.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_SHADERS_BYTES {
            return Err(invalid("the shader library file is too large"));
        }
        let file: ShadersFile = serde_json::from_slice(bytes).map_err(|_| invalid("the shader library file is damaged"))?;
        if file.version != SHADERS_VERSION {
            return Err(Error::new(ErrorKind::Unsupported, "the shader library was written by a newer NodeCraft"));
        }
        if file.shaders.len() > MAX_SHADERS {
            return Err(invalid("the shader library holds too many shaders"));
        }
        let mut library = Self::default();
        for entry in file.shaders {
            let entry = checked(entry)?;
            if library.entries.insert(key(&entry.name), entry).is_some() {
                return Err(invalid("the shader library holds one name twice"));
            }
        }
        Ok(library)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let file = ShadersFile { version: SHADERS_VERSION, shaders: self.entries.values().cloned().collect() };
        let bytes = serde_json::to_vec(&file).map_err(|_| Error::new(ErrorKind::Internal, "the shader library couldn't be written"))?;
        if bytes.len() > MAX_SHADERS_BYTES {
            return Err(invalid("the shader library would be too large; remove shaders you no longer use"));
        }
        Ok(bytes)
    }

    /// The shader called `name`, ignoring case.
    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&key(name))
    }

    /// Every shader, by name.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Saves `shader` as `name`'s next version, unless it is the same as its latest. The version
    /// it is kept as, and whether it was added.
    fn save(&mut self, name: &str, shader: StoredShader) -> Result<(u32, bool), Error> {
        if !valid_name(name) {
            return Err(invalid("a shader's name is 1 to 40 characters on one line"));
        }
        if shader.source.len() > nodecraft_gpu::shader::MAX_SOURCE || shader.params.len() > nodecraft_gpu::shader::MAX_PARAMS {
            return Err(invalid("that shader is larger than a card can keep"));
        }
        config_value(&shader)?;
        let full = self.entries.len() >= MAX_SHADERS;
        let entry = match self.entries.get_mut(&key(name)) {
            Some(entry) => entry,
            None if full => return Err(invalid("the shader library is full; remove shaders you no longer use")),
            None => self.entries.entry(key(name)).or_insert(Entry { name: name.into(), next: 1, versions: Vec::new() }),
        };
        if let Some(latest) = entry.latest().filter(|latest| latest.shader == shader) {
            return Ok((latest.version, false));
        }
        if entry.versions.len() >= MAX_VERSIONS {
            return Err(invalid("this shader keeps 64 versions; remove old ones to save another"));
        }
        let version = entry.next;
        entry.next = version.checked_add(1).ok_or_else(|| invalid("this shader has no version numbers left; save it under a new name"))?;
        entry.versions.push(Version { version, shader });
        Ok((version, true))
    }

    /// Removes one version of `name`, or all of it. A shader left with no versions goes.
    fn remove(&mut self, name: &str, version: Option<u32>) -> Result<(), Error> {
        let missing = || Error::new(ErrorKind::NotFound, "that shader isn't in the library");
        let entry = self.entries.get_mut(&key(name)).ok_or_else(missing)?;
        if let Some(version) = version {
            let before = entry.versions.len();
            entry.versions.retain(|kept| kept.version != version);
            if entry.versions.len() == before {
                return Err(Error::new(ErrorKind::NotFound, "that version isn't in the library"));
            }
        }
        if version.is_none() || entry.versions.is_empty() {
            self.entries.remove(&key(name));
        }
        Ok(())
    }
}

/// What the library says about one shader.
pub fn summary(entry: &Entry) -> Json {
    json!({"name": entry.name, "versions": entry.versions.iter().map(|kept| kept.version).collect::<Vec<_>>(), "latest": entry.latest().map(|kept| kept.version)})
}

/// The shader library as `shaders.json` in `dir`, which holds nothing else (its lock is its own).
#[cfg(not(target_arch = "wasm32"))]
pub fn file_store(dir: std::path::PathBuf) -> crate::library::FileLibrary {
    crate::library::FileLibrary::file(dir, "shaders.json", "shader library", MAX_SHADERS_BYTES)
}

/// The library as the service last read it, for placing and updating; why it couldn't be read,
/// if it couldn't.
#[derive(Clone)]
pub struct ShaderShelf(Rc<RefCell<Result<ShaderLibrary, Error>>>);

impl ShaderShelf {
    /// Calls `f` with the library last read.
    pub fn with<T>(&self, f: impl FnOnce(&ShaderLibrary) -> Result<T, Error>) -> Result<T, Error> {
        let shelved = self.0.try_borrow().map_err(|_| busy())?;
        match &*shelved {
            Ok(library) => f(library),
            Err(error) => Err(Error::new(error.kind, format!("the shader library isn't available: {}", error.message))),
        }
    }

    fn put(&self, read: Result<ShaderLibrary, Error>) {
        if let Ok(mut shelved) = self.0.try_borrow_mut() {
            *shelved = read;
        }
    }
}

fn read(store: &dyn LibraryStore) -> Result<ShaderLibrary, Error> {
    store.load()?.map_or_else(|| Ok(ShaderLibrary::default()), |bytes| ShaderLibrary::from_bytes(&bytes))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    List {},
    Save {
        name: String,
        shader: StoredShader,
    },
    Remove {
        name: String,
        #[serde(default)]
        version: Option<u32>,
    },
}

/// Answers one shader library service request against `store`, and shelves what it read.
pub fn handle(store: &dyn LibraryStore, shelf: &ShaderShelf, request: &Json) -> Result<Json, Error> {
    let request: Request = serde_json::from_value(request.clone()).map_err(|_| invalid("malformed shader library request"))?;
    if let Request::List {} = request {
        let read = read(store);
        shelf.put(read.clone());
        let library = read?;
        return Ok(json!({"shaders": library.entries().map(summary).collect::<Vec<_>>()}));
    }
    let _lock = store.lock()?;
    let mut library = read(store);
    shelf.put(library.clone());
    let library = library.as_mut().map_err(|e| e.clone())?;
    let result = match request {
        Request::List {} => return Err(Error::new(ErrorKind::Internal, "listing doesn't change the library")),
        Request::Save { name, shader } => {
            let (version, saved) = library.save(&name, shader)?;
            let name = library.get(&name).map_or(name, |entry| entry.name.clone());
            if !saved {
                // The latest version is this shader already, so nothing is written.
                return Ok(json!({"name": name, "version": version, "saved": false}));
            }
            json!({"name": name, "version": version, "saved": true})
        }
        Request::Remove { name, version } => {
            library.remove(&name, version)?;
            json!({"removed": true})
        }
    };
    store.save(&library.to_bytes()?)?;
    shelf.put(Ok(library.clone()));
    Ok(result)
}

/// Gives `session` the shader library in `store`: the service, and the shelf placing reads,
/// filled now.
pub fn install(session: &mut Session, store: Box<dyn LibraryStore>) -> Result<(), Error> {
    let shelf = ShaderShelf(Rc::new(RefCell::new(read(store.as_ref()))));
    let service = shelf.clone();
    session.register_host_service(SHADERS_SERVICE, move |request| handle(store.as_ref(), &service, request))?;
    session.swap_resource(Some(shelf));
    Ok(())
}

#[cfg(test)]
mod tests;
