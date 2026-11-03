//! Bounded session-local verified archive cache. Bundled versions always remain available.
use super::{Entry, PreparedTemplate, entries, invalid, validate_archive};
use crate::{Session, commands::Resources};
use nodecraft_types::{Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
};

pub const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CACHE_FILES: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub id: String,
    pub version: u32,
    pub blake3: String,
}
impl From<&Entry> for Identity {
    fn from(entry: &Entry) -> Self {
        Self { id: entry.id.clone(), version: entry.version, blake3: entry.blake3.clone() }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Listing {
    pub templates: Vec<Entry>,
    pub available: Vec<Identity>,
    pub stamp: u64,
}
impl Listing {
    pub fn available(&self, entry: &Entry) -> bool {
        self.available.contains(&Identity::from(entry))
    }
}

#[derive(Default)]
pub struct Catalog {
    stamp: u64,
    remote: Vec<Entry>,
    cached: VecDeque<(Entry, Arc<[u8]>)>,
}
impl Catalog {
    fn bump(&mut self) -> Result<(), Error> {
        self.stamp = self.stamp.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "Too many template updates; reopen the app"))?;
        Ok(())
    }
    pub fn listing(&self) -> Result<Listing, Error> {
        let bundled = entries()?;
        let mut records = BTreeMap::new();
        for entry in bundled.iter().chain(self.cached.iter().map(|(entry, _)| entry)).chain(self.remote.iter()) {
            records.insert((entry.id.clone(), entry.version), entry.clone());
        }
        let available = bundled.iter().chain(self.cached.iter().map(|(entry, _)| entry)).map(Identity::from).collect();
        Ok(Listing { templates: records.into_values().collect(), available, stamp: self.stamp })
    }
    /// Admission completes before the catalog changes. Unknown schemas retain the last good set.
    pub fn install_index(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let remote = super::parse_index(bytes)?;
        let known = self.listing()?;
        for entry in &remote {
            if known.templates.iter().any(|old| old.id == entry.id && old.version == entry.version && old.blake3 != entry.blake3) {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    "A published template version changed its content; keeping the previous catalog. If this persists, use Clear downloaded",
                ));
            }
        }
        self.bump()?;
        self.remote = remote;
        Ok(())
    }
    pub fn requested(&self, identity: &Identity) -> Result<Entry, Error> {
        self.listing()?
            .templates
            .into_iter()
            .find(|entry| Identity::from(entry) == *identity)
            .ok_or_else(|| Error::new(ErrorKind::Conflict, "This template selection changed; refresh the list"))
    }
    /// Admit one verified archive for a listed entry. Returns the entries evicted to stay within
    /// the cache bounds, so a persistent store can drop them too.
    pub fn install_archive(&mut self, entry: Entry, bytes: Vec<u8>) -> Result<Vec<Entry>, Error> {
        let current = self.requested(&Identity::from(&entry))?;
        if current != entry {
            return Err(Error::new(ErrorKind::Conflict, "The template metadata changed; select it again"));
        }
        validate_archive(&entry, &bytes)?;
        self.admit(entry, bytes)
    }
    /// Re-admit an archive read back from the persistent cache (#954), verifying it again.
    pub fn restore(&mut self, entry: Entry, bytes: Vec<u8>) -> Result<Vec<Entry>, Error> {
        validate_archive(&entry, &bytes)?;
        self.restore_verified(entry, bytes)
    }
    /// [`Self::restore`] for bytes [`super::disk::load`] has already verified. Its record need not
    /// be in the current index, but it may not contradict the index, a bundled version or another
    /// cached copy of the same version.
    pub(super) fn restore_verified(&mut self, entry: Entry, bytes: Vec<u8>) -> Result<Vec<Entry>, Error> {
        let contradicts = |known: &Entry| known.id == entry.id && known.version == entry.version && known.blake3 != entry.blake3;
        if entries()?.iter().chain(self.remote.iter()).chain(self.cached.iter().map(|(cached, _)| cached)).any(contradicts) {
            return Err(Error::new(ErrorKind::Conflict, "A cached template contradicts a published version; use Clear downloaded to remove it"));
        }
        self.admit(entry, bytes)
    }
    fn admit(&mut self, entry: Entry, bytes: Vec<u8>) -> Result<Vec<Entry>, Error> {
        if bytes.len() > MAX_CACHE_BYTES {
            return Err(invalid());
        }
        self.bump()?;
        self.cached.retain(|(old, _)| Identity::from(old) != Identity::from(&entry));
        let mut evicted = Vec::new();
        let mut total = self.cached.iter().fold(bytes.len(), |sum, (_, body)| sum.saturating_add(body.len()));
        while total > MAX_CACHE_BYTES || self.cached.len() >= MAX_CACHE_FILES {
            let Some((old, body)) = self.cached.pop_front() else { break };
            total = total.saturating_sub(body.len());
            evicted.push(old);
        }
        self.cached.push_back((entry, Arc::from(bytes)));
        Ok(evicted)
    }
    /// Forget every downloaded archive; bundled versions and the indexed metadata stay.
    pub fn clear_cached(&mut self) -> Result<(), Error> {
        self.bump()?;
        self.cached.clear();
        Ok(())
    }
    pub fn cached_count(&self) -> usize {
        self.cached.len()
    }
    fn bytes(&self, entry: &Entry) -> Option<&[u8]> {
        nodecraft_template_data::FILES
            .iter()
            .find(|(file, _)| *file == entry.file)
            .map(|(_, bytes)| *bytes)
            .or_else(|| self.cached.iter().find(|(old, _)| Identity::from(old) == Identity::from(entry)).map(|(_, bytes)| bytes.as_ref()))
    }
    pub fn resolve(&self, id: &str, version: Option<u32>, digest: Option<&str>) -> Result<PreparedTemplate, Error> {
        let listing = self.listing()?;
        let entry = listing
            .templates
            .into_iter()
            .filter(|entry| entry.id == id && version.is_none_or(|v| v == entry.version) && digest.is_none_or(|d| d == entry.blake3))
            .filter(|entry| self.bytes(entry).is_some())
            .max_by_key(|entry| entry.version)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "This template version is not available offline; retrieve it from the template list first"))?;
        let bytes = self.bytes(&entry).ok_or_else(invalid)?;
        let snapshot = validate_archive(&entry, bytes)?;
        Ok(PreparedTemplate { entry, snapshot })
    }
    pub fn cached_bytes(&self) -> usize {
        self.cached.iter().fold(0usize, |sum, (_, body)| sum.saturating_add(body.len()))
    }
}

pub fn listing(resources: &Resources) -> Result<Listing, Error> {
    resources.get::<Catalog>().map_or_else(|| Catalog::default().listing(), Catalog::listing)
}
/// How many downloaded archives the session holds.
pub fn cached(session: &Session) -> usize {
    session.resource::<Catalog>().map_or(0, Catalog::cached_count)
}
pub fn stamp(session: &Session) -> u64 {
    session.resource::<Catalog>().map_or(0, |catalog| catalog.stamp)
}
/// Admit a fetched index and, when the host keeps a persistent cache, store it as the last good one.
pub fn install_index(session: &mut Session, bytes: &[u8]) -> Result<(), Error> {
    install_signed_index(session, bytes, None)
}
/// [`install_index`], storing the index's verified signature beside it.
pub fn install_signed_index(session: &mut Session, bytes: &[u8], signature: Option<&[u8]>) -> Result<(), Error> {
    update(session, |catalog| catalog.install_index(bytes))?;
    super::disk::persist_index(session, bytes, signature);
    Ok(())
}
/// Admit a fetched archive and, when the host keeps a persistent cache, store it there.
pub fn install_archive(session: &mut Session, entry: Entry, bytes: Vec<u8>) -> Result<(), Error> {
    let stored = entry.clone();
    let persist = super::disk::persistent(session).then(|| bytes.clone());
    let evicted = update(session, |catalog| catalog.install_archive(entry, bytes))?;
    if let Some(bytes) = persist {
        super::disk::persist_archive(session, &stored, bytes, &evicted);
    }
    Ok(())
}
pub(super) fn update<T>(session: &mut Session, f: impl FnOnce(&mut Catalog) -> Result<T, Error>) -> Result<T, Error> {
    let mut catalog = session.swap_resource::<Catalog>(None).unwrap_or_default();
    let result = f(&mut catalog);
    session.swap_resource(Some(catalog));
    result
}
pub(super) fn resolve(resources: &Resources, id: &str, version: Option<u32>, digest: Option<&str>) -> Result<PreparedTemplate, Error> {
    resources.get::<Catalog>().map_or_else(|| Catalog::default().resolve(id, version, digest), |catalog| catalog.resolve(id, version, digest))
}
