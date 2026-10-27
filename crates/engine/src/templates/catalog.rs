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
                return Err(Error::new(ErrorKind::Conflict, "A published template version changed its content; keeping the previous catalog"));
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
    pub fn install_archive(&mut self, entry: Entry, bytes: Vec<u8>) -> Result<(), Error> {
        let current = self.requested(&Identity::from(&entry))?;
        if current != entry {
            return Err(Error::new(ErrorKind::Conflict, "The template metadata changed; select it again"));
        }
        validate_archive(&entry, &bytes)?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err(invalid());
        }
        self.bump()?;
        self.cached.retain(|(old, _)| Identity::from(old) != Identity::from(&entry));
        let mut total = self.cached.iter().fold(bytes.len(), |sum, (_, body)| sum.saturating_add(body.len()));
        while total > MAX_CACHE_BYTES || self.cached.len() >= MAX_CACHE_FILES {
            let Some((_, old)) = self.cached.pop_front() else { break };
            total = total.saturating_sub(old.len());
        }
        self.cached.push_back((entry, Arc::from(bytes)));
        Ok(())
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
pub fn stamp(session: &Session) -> u64 {
    session.resource::<Catalog>().map_or(0, |catalog| catalog.stamp)
}
pub fn install_index(session: &mut Session, bytes: &[u8]) -> Result<(), Error> {
    update(session, |catalog| catalog.install_index(bytes))
}
pub fn install_archive(session: &mut Session, entry: Entry, bytes: Vec<u8>) -> Result<(), Error> {
    update(session, |catalog| catalog.install_archive(entry, bytes))
}
fn update(session: &mut Session, f: impl FnOnce(&mut Catalog) -> Result<(), Error>) -> Result<(), Error> {
    let mut catalog = session.swap_resource::<Catalog>(None).unwrap_or_default();
    let result = f(&mut catalog);
    session.swap_resource(Some(catalog));
    result
}
pub(super) fn resolve(resources: &Resources, id: &str, version: Option<u32>, digest: Option<&str>) -> Result<PreparedTemplate, Error> {
    resources.get::<Catalog>().map_or_else(|| Catalog::default().resolve(id, version, digest), |catalog| catalog.resolve(id, version, digest))
}
