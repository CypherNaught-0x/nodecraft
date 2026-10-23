//! Where each installed package was last imported from, so `plugins.reload` can install that file
//! again. Kept in its own file beside the index: the index format stays what older builds read, and
//! a lost or damaged sources file costs only the Reload button, never a package.
use super::{MAX_OBJECTS, Store, StoreBackend};
use crate::archive;
use nodecraft_plugin::id::PackageId;
use nodecraft_plugin::json::{self, Json, Limits};
use nodecraft_types::{Error, ErrorKind};
use std::collections::BTreeMap;

/// Sources format this build reads and writes.
pub const SOURCES_VERSION: i64 = 1;
/// Longest location kept, in bytes.
pub const MAX_SOURCE: usize = 4096;

const SOURCES_LIMITS: Limits = Limits { bytes: 2 * 1024 * 1024, depth: 3, string_bytes: MAX_SOURCE, entries: MAX_OBJECTS, raw_numbers: false };

/// What the sources file holds.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    /// Readable (absent reads as empty). Entries that aren't a package ID and a location are
    /// skipped.
    Sources(BTreeMap<PackageId, String>),
    /// Damaged, or written in another format (a newer NodeCraft): read as none remembered, and
    /// never written over.
    Unreadable,
}

fn decode(bytes: &[u8]) -> Read {
    let Ok(Json::Object(mut root)) = archive::parse(bytes, SOURCES_LIMITS) else { return Read::Unreadable };
    let (Some(Json::Int(SOURCES_VERSION)), Some(Json::Object(entries))) = (root.remove("sources_version"), root.remove("sources")) else {
        return Read::Unreadable;
    };
    if !root.is_empty() {
        return Read::Unreadable;
    }
    let mut sources = BTreeMap::new();
    for (id, location) in entries {
        if let (Ok(id), Json::Str(location)) = (PackageId::parse(&id), location)
            && !location.is_empty()
        {
            sources.insert(id, location);
        }
    }
    Read::Sources(sources)
}

/// The file's bytes, refused unless they read back as exactly `sources`: a file over the read
/// limits once escaped would later read as unreadable, and every location would be lost.
fn encode(sources: &BTreeMap<PackageId, String>) -> Result<Vec<u8>, Error> {
    let entries = sources.iter().map(|(id, location)| (id.to_string(), Json::Str(location.clone()))).collect();
    let root = Json::Object(BTreeMap::from([("sources".to_owned(), Json::Object(entries)), ("sources_version".to_owned(), Json::Int(SOURCES_VERSION))]));
    let bytes = json::canonical(&root).map_err(archive::wire)?.into_bytes();
    match decode(&bytes) {
        Read::Sources(read) if read == *sources => Ok(bytes),
        _ => Err(Error::new(ErrorKind::Unsupported, "the plugin store can't remember that many package locations; uninstall packages you no longer use")),
    }
}

/// Why a location isn't saved over a file this build can't read.
fn unreadable() -> Error {
    Error::new(
        ErrorKind::Conflict,
        "the plugin store's sources.json is damaged or from a newer NodeCraft, so NodeCraft doesn't write over it; the package's location isn't saved",
    )
}

impl<B: StoreBackend> Store<B> {
    fn read_sources(&self) -> Result<Read, Error> {
        Ok(self.backend.read_sources()?.map(|bytes| decode(&bytes)).unwrap_or(Read::Sources(BTreeMap::new())))
    }

    /// Readable sources, with an unreadable file read as none.
    fn readable_sources(&self) -> Result<BTreeMap<PackageId, String>, Error> {
        Ok(match self.read_sources()? {
            Read::Sources(sources) => sources,
            Read::Unreadable => BTreeMap::new(),
        })
    }

    /// The location `package` was last imported or updated from, if it is installed and one was
    /// remembered.
    pub fn source(&self, package: &PackageId) -> Result<Option<String>, Error> {
        let _transaction = self.backend.transaction()?;
        if !self.index.active.contains_key(package) {
            return Ok(None);
        }
        Ok(self.readable_sources()?.remove(package))
    }

    /// The remembered location of every installed package that has one.
    pub fn sources(&self) -> Result<BTreeMap<PackageId, String>, Error> {
        let _transaction = self.backend.transaction()?;
        let mut sources = self.readable_sources()?;
        sources.retain(|id, _| self.index.active.contains_key(id));
        Ok(sources)
    }

    /// Remembers (or with `None` forgets) where `package` was imported from. Locations longer
    /// than [`MAX_SOURCE`] aren't kept. Sources of packages no longer installed are dropped.
    /// Refused, writing nothing, when the existing file can't be read (damaged, or from a newer
    /// NodeCraft) or when the file would grow past what can be read back.
    pub fn remember_source(&mut self, package: &PackageId, location: Option<&str>) -> Result<(), Error> {
        let _transaction = self.backend.transaction()?;
        self.forget_or_remember(package, location)
    }

    /// The same, under the caller's transaction.
    pub(super) fn forget_or_remember(&mut self, package: &PackageId, location: Option<&str>) -> Result<(), Error> {
        let Read::Sources(mut sources) = self.read_sources()? else { return Err(unreadable()) };
        let before = sources.clone();
        sources.retain(|id, _| self.index.active.contains_key(id));
        match location {
            Some(location) if !location.is_empty() && location.len() <= MAX_SOURCE && self.index.active.contains_key(package) => {
                sources.insert(package.clone(), location.to_owned());
            }
            _ => {
                sources.remove(package);
            }
        }
        if sources == before {
            return Ok(());
        }
        if sources.len() > MAX_OBJECTS {
            return Err(Error::new(ErrorKind::Unsupported, "the plugin store can't remember more package sources"));
        }
        self.backend.write_sources(&encode(&sources)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> PackageId {
        PackageId::parse(s).unwrap()
    }

    #[test]
    fn sources_round_trip_and_damage_reads_as_none() {
        let sources = BTreeMap::from([(id("io.example.a"), "/tmp/a.ncraft-plugin".to_owned()), (id("io.example.b"), "C:\\b.ncraft-plugin".to_owned())]);
        let bytes = encode(&sources).unwrap();
        assert_eq!(decode(&bytes), Read::Sources(sources));
        for damaged in [&b"{"[..], b"[]", b"{\"sources\":{},\"sources_version\":2}", b"{\"sources\":{},\"sources_version\":1,\"x\":1}", b"\xff\xfe"] {
            assert_eq!(decode(damaged), Read::Unreadable);
        }
        // A bad entry is skipped, not the file.
        let mixed = br#"{"sources":{"io.example.a":"/a","not an id":"/b","io.example.c":7},"sources_version":1}"#;
        assert_eq!(decode(mixed), Read::Sources(BTreeMap::from([(id("io.example.a"), "/a".to_owned())])));
    }
}
