//! The local package store: verified package archives kept by digest, and an index saying which
//! version of each package is active and whether it is enabled.
//!
//! - **Explicit and inert.** Importing verifies a package and installs it *disabled*; nothing runs
//!   and nothing is granted. Enabling stages the package's registrations against everything already
//!   registered and publishes them all or none. Data packages need no capabilities (phase A
//!   refuses ones that ask), so enabling grants nothing either.
//! - **Atomic.** Every change is made on a copy of the index, written in full (the native backend
//!   writes aside and renames over), and only then becomes the store's state. A failed write (a
//!   full disk) leaves the store, its index and every package as they were.
//! - **Nothing referenced is lost.** Updating keeps the previous digest for rollback; uninstalling
//!   keeps the digests the caller says projects still reference, and those other installed
//!   packages lock. Projects are never edited here. Files the index no longer names are removed
//!   only when the store is next opened, after the index that dropped them is safely on disk.
//! - **One writer wins.** Before writing, the store checks the index is still the one it read;
//!   if another NodeCraft (the app, the CLI) changed it meanwhile, the change is refused rather
//!   than lost, and the store reloads.
//! - **Trust is checked when it matters.** A package's signature is checked against the trust
//!   file at import and again at enable, so a key revoked since import stops it being enabled.
use crate::archive::{self, ArchiveLimits, Package, wire};
use nodecraft_plugin::id::{HostId, PackageId, RegisteredId};
use nodecraft_plugin::json::{self, Json, Limits};
use nodecraft_plugin::lock;
use nodecraft_plugin::manifest::{Dependency, Version};
use nodecraft_plugin::signature::{self, Provenance, TrustStore};
use nodecraft_plugin::{PackageDigest, register};
use nodecraft_types::{Error, ErrorKind};
use std::collections::{BTreeMap, BTreeSet};

/// Index format this build reads.
pub const INDEX_VERSION: i64 = 1;
/// Most package digests kept (active and retained).
pub const MAX_OBJECTS: usize = 256;
/// Most bytes of package archives kept.
pub const MAX_STORE_BYTES: u64 = 512 * 1024 * 1024;

/// Where the store keeps its index and archives (a platform service).
pub trait StoreBackend {
    /// The index, or `None` before the first write.
    fn read_index(&self) -> Result<Option<Vec<u8>>, Error>;
    /// Replaces the index atomically: afterwards either the old or the new index is read.
    fn write_index(&mut self, bytes: &[u8]) -> Result<(), Error>;
    fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, Error>;
    /// Stores an archive under its digest (durably, before the index names it).
    fn write_object(&mut self, digest: &PackageDigest, bytes: &[u8]) -> Result<(), Error>;
    fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), Error>;
    /// Stored archives that may be swept if the index doesn't name them. A backend another
    /// process writes leaves out recent ones: they may belong to an import whose index isn't
    /// written yet.
    fn list_objects(&self) -> Result<Vec<PackageDigest>, Error>;
}

/// Whether an installed package's registrations are published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Disabled,
    Enabled,
}

/// One installed digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub package_id: PackageId,
    pub version: Version,
    pub digest: PackageDigest,
    pub bytes: u64,
    /// The SPDX expression the package declares (shown with its notice files; not a legal check).
    pub license: String,
    /// Who signed it, as checked at import against the trust file then.
    pub signed_by: Option<String>,
    /// The IDs it registers and its exact dependency locks, so the store's checks never need
    /// to open archives.
    pub registrations: Vec<String>,
    pub dependencies: Vec<Dependency>,
}

/// The store's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Index {
    /// Every kept digest.
    pub objects: BTreeMap<PackageDigest, Entry>,
    /// The active digest of each installed package, and its state.
    pub active: BTreeMap<PackageId, (PackageDigest, State)>,
}

/// What importing (or a dry run) found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReport {
    pub entry: Entry,
    pub provenance: Provenance,
    pub registrations: Vec<RegisteredId>,
    /// License and notice files, to show before enabling.
    pub notices: Vec<String>,
    /// The digest this replaced as active, for an update.
    pub replaced: Option<PackageDigest>,
}

/// What enabling published, and what stays off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnableReport {
    pub registered: Vec<RegisteredId>,
    /// Entries bound to a host implementation this host doesn't allow: kept, but off.
    pub unbound: Vec<RegisteredId>,
}

/// What uninstalling did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UninstallReport {
    /// Digests no longer kept (their files go when the store is next opened).
    pub removed: Vec<PackageDigest>,
    /// Digests kept because projects reference them or installed packages lock them.
    pub kept: Vec<PackageDigest>,
    /// Installed packages that lock this one; they can't be enabled until it is back.
    pub dependents: Vec<PackageId>,
}

/// A verified incoming package, and the archive bytes to store (none when its digest is kept).
struct Incoming {
    package: Package,
    provenance: Provenance,
    write: Option<Vec<u8>>,
    size: usize,
}

pub struct Store<B: StoreBackend> {
    backend: B,
    index: Index,
    /// The index bytes as last read or written, to notice another writer.
    seen: Option<Vec<u8>>,
    limits: ArchiveLimits,
}

fn conflict(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Conflict, detail)
}

fn not_installed() -> Error {
    Error::new(ErrorKind::NotFound, "that package isn't installed")
}

impl<B: StoreBackend> Store<B> {
    /// Opens the store over `backend`, reading its index (an unreadable index is an error, never
    /// a silent empty store).
    pub fn open(mut backend: B) -> Result<Self, Error> {
        let seen = backend.read_index()?;
        let index = match &seen {
            Some(bytes) => decode_index(bytes)?,
            None => Index::default(),
        };
        // Files the index doesn't name: dropped by an uninstall or prune, or left by a write that
        // failed before its index did. The index naming them is on disk by now.
        for digest in backend.list_objects()? {
            if !index.objects.contains_key(&digest) {
                let _ = backend.remove_object(&digest);
            }
        }
        Ok(Self { backend, index, seen, limits: ArchiveLimits::DEFAULT })
    }

    pub fn with_limits(mut self, limits: ArchiveLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Writes `next` and makes it the state, or changes nothing.
    fn commit(&mut self, next: Index) -> Result<(), Error> {
        let current = self.backend.read_index()?;
        if current != self.seen {
            // Another NodeCraft changed the store: take its state, refuse this change.
            if let Some(bytes) = &current {
                self.index = decode_index(bytes)?;
            }
            self.seen = current;
            return Err(conflict("the plugin store was changed by another NodeCraft window; try again"));
        }
        let bytes = encode_index(&next)?.into_bytes();
        self.backend.write_index(&bytes)?;
        self.index = next;
        self.seen = Some(bytes);
        Ok(())
    }

    fn verify(&self, bytes: &[u8], trust: &TrustStore) -> Result<(Package, Provenance), Error> {
        let package = archive::open(bytes, self.limits)?;
        let provenance = provenance(&package, trust)?;
        Ok((package, provenance))
    }

    /// Verifies an incoming archive; when its digest is already kept, the kept archive is what
    /// counts (one digest can come from differently built archives), and nothing is written.
    fn incoming(&self, bytes: &[u8], trust: &TrustStore) -> Result<Incoming, Error> {
        let (package, provenance) = self.verify(bytes, trust)?;
        if self.index.objects.contains_key(&package.digest) {
            let kept = self.backend.read_object(&package.digest)?;
            let (stored, provenance) = self.verify(&kept, trust)?;
            if stored.digest != package.digest {
                return Err(Error::new(ErrorKind::Internal, "a stored package no longer matches its digest; import it again"));
            }
            return Ok(Incoming { package: stored, provenance, write: None, size: kept.len() });
        }
        Ok(Incoming { package, provenance, write: Some(bytes.to_vec()), size: bytes.len() })
    }

    /// IDs registered by installed packages other than `except` (enabled or not): no two
    /// installed packages may spell the same ID, whichever is enabled first.
    fn installed_ids(&self, except: &PackageId) -> Result<BTreeSet<String>, Error> {
        let mut ids = BTreeSet::new();
        for (id, (digest, _)) in &self.index.active {
            if id != except {
                ids.extend(self.entry(digest)?.registrations.iter().cloned());
            }
        }
        Ok(ids)
    }

    fn entry(&self, digest: &PackageDigest) -> Result<&Entry, Error> {
        self.index.objects.get(digest).ok_or_else(|| Error::new(ErrorKind::Internal, "the plugin store's index names a package it doesn't keep"))
    }

    /// Re-reads the index if another NodeCraft changed it since this store last read or wrote
    /// it, so every request starts from the store's current state.
    pub fn refresh(&mut self) -> Result<(), Error> {
        let current = self.backend.read_index()?;
        if current != self.seen {
            self.index = match &current {
                Some(bytes) => decode_index(bytes)?,
                None => Index::default(),
            };
            self.seen = current;
        }
        Ok(())
    }

    fn report(&self, package: &Package, provenance: Provenance, bytes: usize, replaced: Option<PackageDigest>) -> ImportReport {
        let signed_by = match &provenance {
            Provenance::Verified { key_id } => Some(key_id.to_string()),
            _ => None,
        };
        let entry = Entry {
            package_id: package.manifest.package_id.clone(),
            version: package.manifest.version,
            digest: package.digest,
            bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
            license: package.manifest.license.clone(),
            signed_by,
            registrations: package.manifest.registered_ids().map(|id| id.to_string()).collect(),
            dependencies: package.manifest.dependencies.clone(),
        };
        ImportReport {
            entry,
            provenance,
            registrations: package.manifest.registered_ids().collect(),
            notices: package.notices().map(|p| p.to_string()).collect(),
            replaced,
        }
    }

    fn admit(&self, next: &mut Index, entry: &Entry) -> Result<(), Error> {
        if next.objects.contains_key(&entry.digest) {
            return Ok(());
        }
        if next.objects.len() >= MAX_OBJECTS {
            return Err(Error::new(ErrorKind::Budget, "the plugin store holds as many packages as it can; uninstall some first"));
        }
        let total = next.objects.values().try_fold(entry.bytes, |sum, e| sum.checked_add(e.bytes));
        if total.is_none_or(|t| t > MAX_STORE_BYTES) {
            return Err(Error::new(ErrorKind::Budget, "the plugin store is full; uninstall some packages first"));
        }
        next.objects.insert(entry.digest, entry.clone());
        Ok(())
    }

    /// Verifies `bytes` and installs the package disabled; with `dry_run`, only reports.
    /// A package already installed is updated with [`Store::update`] instead.
    pub fn import(&mut self, bytes: &[u8], trust: &TrustStore, dry_run: bool) -> Result<ImportReport, Error> {
        let Incoming { package, provenance, write, size } = self.incoming(bytes, trust)?;
        if self.index.active.contains_key(&package.manifest.package_id) {
            return Err(conflict("that package is already installed; update it instead"));
        }
        let taken = self.installed_ids(&package.manifest.package_id)?;
        register::stage(&package.manifest, &taken).map_err(|e| conflict(format!("an installed package already uses one of its IDs ({e})")))?;
        let report = self.report(&package, provenance, size, None);
        let mut next = self.index.clone();
        self.admit(&mut next, &report.entry)?;
        next.active.insert(report.entry.package_id.clone(), (report.entry.digest, State::Disabled));
        if !dry_run {
            if let Some(bytes) = write {
                self.backend.write_object(&package.digest, &bytes)?;
            }
            self.commit(next)?;
        }
        Ok(report)
    }

    /// Installs a new digest of an installed package, disabled, keeping the old one for
    /// rollback. Disable the package first; its dependents keep their exact locks, so they
    /// can't be enabled against the new digest until they are updated too.
    pub fn update(&mut self, bytes: &[u8], trust: &TrustStore, dry_run: bool) -> Result<ImportReport, Error> {
        let Incoming { package, provenance, write, size } = self.incoming(bytes, trust)?;
        let Some((current, state)) = self.index.active.get(&package.manifest.package_id).copied() else {
            return Err(not_installed());
        };
        if current == package.digest {
            return Err(conflict("that exact package is already installed"));
        }
        if state == State::Enabled && !dry_run {
            return Err(conflict("disable the package before updating it"));
        }
        let taken = self.installed_ids(&package.manifest.package_id)?;
        register::stage(&package.manifest, &taken).map_err(|e| conflict(format!("an installed package already uses one of its IDs ({e})")))?;
        let report = self.report(&package, provenance, size, Some(current));
        let mut next = self.index.clone();
        self.admit(&mut next, &report.entry)?;
        next.active.insert(report.entry.package_id.clone(), (report.entry.digest, State::Disabled));
        if !dry_run {
            if let Some(bytes) = write {
                self.backend.write_object(&package.digest, &bytes)?;
            }
            self.commit(next)?;
        }
        Ok(report)
    }

    /// Makes a kept digest of an installed (disabled) package active again.
    pub fn rollback(&mut self, package: &PackageId, digest: &PackageDigest) -> Result<(), Error> {
        let Some((_, state)) = self.index.active.get(package).copied() else {
            return Err(not_installed());
        };
        if state == State::Enabled {
            return Err(conflict("disable the package before rolling it back"));
        }
        if self.index.objects.get(digest).is_none_or(|e| e.package_id != *package) {
            return Err(Error::new(ErrorKind::NotFound, "that version of the package isn't kept"));
        }
        let mut next = self.index.clone();
        next.active.insert(package.clone(), (*digest, State::Disabled));
        self.commit(next)
    }

    /// Reads and re-verifies the active package `package`.
    pub fn load(&self, package: &PackageId) -> Result<Package, Error> {
        let (digest, _) = self.index.active.get(package).ok_or_else(not_installed)?;
        self.load_digest(digest)
    }

    fn load_digest(&self, digest: &PackageDigest) -> Result<Package, Error> {
        let package = archive::open(&self.backend.read_object(digest)?, self.limits)?;
        if package.digest != *digest {
            return Err(Error::new(ErrorKind::Internal, "a stored package no longer matches its digest; import it again"));
        }
        Ok(package)
    }

    /// Enables `package`: its signature must still verify against `trust`, its dependencies
    /// must be installed at their locked versions and enabled, and none of its IDs may be taken
    /// (`taken`: the host's own IDs; installed packages' IDs are checked too). Entries bound to a
    /// host implementation outside `bindings` stay off.
    pub fn enable(&mut self, package: &PackageId, trust: &TrustStore, taken: &BTreeSet<String>, bindings: &BTreeSet<HostId>) -> Result<EnableReport, Error> {
        let Some((digest, state)) = self.index.active.get(package).copied() else {
            return Err(not_installed());
        };
        if state == State::Enabled {
            return Err(conflict("that package is already enabled"));
        }
        let loaded = self.load_digest(&digest)?;
        provenance(&loaded, trust)?;
        // Every lock names an installed, enabled package at exactly that version and digest.
        // Enabled packages had their own locks checked when enabled, and stay put while
        // enabled, so the chain below them holds too.
        for dep in &loaded.manifest.dependencies {
            match self.index.active.get(&dep.package_id) {
                Some((d, _)) if *d != dep.digest || self.entry(d)?.version != dep.version => {
                    return Err(conflict(format!("its dependencies aren't met ({} isn't installed at the locked version)", dep.package_id)));
                }
                Some((_, State::Disabled)) => return Err(conflict(format!("enable {} first", dep.package_id))),
                Some((_, State::Enabled)) => {}
                None => return Err(conflict(format!("its dependencies aren't met ({} isn't installed)", dep.package_id))),
            }
        }
        if depth(&self.index, &digest)? > lock::MAX_DEPTH {
            return Err(conflict("its dependencies nest deeper than 8"));
        }
        let mut registered: BTreeSet<String> = taken.clone();
        registered.extend(self.installed_ids(package)?);
        let staged = register::stage(&loaded.manifest, &registered).map_err(|e| conflict(format!("nothing was enabled: {e}")))?;
        let mut report = EnableReport { registered: Vec::new(), unbound: Vec::new() };
        for (r, id) in loaded.manifest.registrations.iter().zip(staged) {
            match &r.binding {
                Some(binding) if !bindings.contains(binding) => report.unbound.push(id),
                _ => report.registered.push(id),
            }
        }
        let mut next = self.index.clone();
        next.active.insert(package.clone(), (digest, State::Enabled));
        self.commit(next)?;
        Ok(report)
    }

    /// Disables `package`; refused while an enabled package depends on it.
    pub fn disable(&mut self, package: &PackageId) -> Result<(), Error> {
        let Some((digest, state)) = self.index.active.get(package).copied() else {
            return Err(not_installed());
        };
        if state == State::Disabled {
            return Ok(());
        }
        for (id, (d, s)) in &self.index.active {
            if id != package && *s == State::Enabled && self.entry(d)?.dependencies.iter().any(|dep| dep.package_id == *package) {
                return Err(conflict(format!("disable {id} first; it depends on this package")));
            }
        }
        let mut next = self.index.clone();
        next.active.insert(package.clone(), (digest, State::Disabled));
        self.commit(next)
    }

    /// Digests installed packages lock, and the packages locking each.
    fn locked(&self) -> Result<BTreeMap<PackageDigest, Vec<PackageId>>, Error> {
        let mut locked: BTreeMap<PackageDigest, Vec<PackageId>> = BTreeMap::new();
        for (id, (digest, _)) in &self.index.active {
            for dep in &self.entry(digest)?.dependencies {
                locked.entry(dep.digest).or_default().push(id.clone());
            }
        }
        Ok(locked)
    }

    /// Uninstalls a disabled `package`. Its kept digests that `referenced` names (projects still
    /// use them) or that installed packages lock stay in the store; the rest are dropped.
    pub fn uninstall(&mut self, package: &PackageId, referenced: &BTreeSet<PackageDigest>) -> Result<UninstallReport, Error> {
        let Some((_, state)) = self.index.active.get(package).copied() else {
            return Err(not_installed());
        };
        if state == State::Enabled {
            return Err(conflict("disable the package before uninstalling it"));
        }
        let locked = self.locked()?;
        let mut next = self.index.clone();
        next.active.remove(package);
        let mut report = UninstallReport { removed: Vec::new(), kept: Vec::new(), dependents: Vec::new() };
        for entry in self.index.objects.values().filter(|e| e.package_id == *package) {
            if let Some(by) = locked.get(&entry.digest) {
                report.dependents.extend(by.iter().filter(|id| *id != package).cloned());
            }
            if referenced.contains(&entry.digest) || locked.get(&entry.digest).is_some_and(|by| by.iter().any(|id| id != package)) {
                report.kept.push(entry.digest);
            } else {
                report.removed.push(entry.digest);
                next.objects.remove(&entry.digest);
            }
        }
        report.dependents.sort();
        report.dependents.dedup();
        self.commit(next)?;
        Ok(report)
    }

    /// Drops every kept digest that isn't active, referenced by projects (`referenced`) or locked
    /// by an installed package: old versions kept for rollback. Returns what was dropped.
    pub fn prune(&mut self, referenced: &BTreeSet<PackageDigest>) -> Result<Vec<PackageDigest>, Error> {
        let locked = self.locked()?;
        let active: BTreeSet<PackageDigest> = self.index.active.values().map(|(d, _)| *d).collect();
        let mut next = self.index.clone();
        let dropped: Vec<PackageDigest> =
            self.index.objects.keys().filter(|d| !active.contains(d) && !referenced.contains(d) && !locked.contains_key(d)).copied().collect();
        for digest in &dropped {
            next.objects.remove(digest);
        }
        self.commit(next)?;
        Ok(dropped)
    }
}

/// How many levels of dependencies lie below `digest`, from the index: a level-by-level walk
/// over a seen set, stopping past the limit.
fn depth(index: &Index, digest: &PackageDigest) -> Result<usize, Error> {
    let mut seen = BTreeSet::from([*digest]);
    let mut level = vec![*digest];
    let mut depth = 0;
    while depth <= lock::MAX_DEPTH {
        let next: Vec<PackageDigest> =
            level.iter().filter_map(|d| index.objects.get(d)).flat_map(|e| e.dependencies.iter().map(|dep| dep.digest)).filter(|d| seen.insert(*d)).collect();
        if next.is_empty() {
            break;
        }
        depth += 1;
        level = next;
    }
    Ok(depth)
}

/// A package's signature checked against `trust` now.
fn provenance(package: &Package, trust: &TrustStore) -> Result<Provenance, Error> {
    signature::verify(&package.digest, package.signature.as_ref(), trust, package.manifest.publisher_key_id.as_ref()).map_err(wire)
}

const INDEX_LIMITS: Limits = Limits { bytes: 4 * 1024 * 1024, depth: 6, string_bytes: 4096, entries: 4 * MAX_OBJECTS, raw_numbers: false };

fn encode_index(index: &Index) -> Result<String, Error> {
    let s = |t: String| Json::Str(t);
    let objects = index
        .objects
        .values()
        .map(|e| {
            let mut m = BTreeMap::from([
                ("bytes".to_owned(), s(e.bytes.to_string())),
                ("digest".to_owned(), s(e.digest.to_string())),
                ("license".to_owned(), s(e.license.clone())),
                ("package_id".to_owned(), s(e.package_id.to_string())),
                ("version".to_owned(), s(e.version.to_string())),
            ]);
            if let Some(key) = &e.signed_by {
                m.insert("signed_by".to_owned(), s(key.clone()));
            }
            m.insert("registrations".to_owned(), Json::Array(e.registrations.iter().map(|r| s(r.clone())).collect()));
            let deps = e
                .dependencies
                .iter()
                .map(|d| {
                    Json::Object(BTreeMap::from([
                        ("digest".to_owned(), s(d.digest.to_string())),
                        ("package_id".to_owned(), s(d.package_id.to_string())),
                        ("version".to_owned(), s(d.version.to_string())),
                    ]))
                })
                .collect();
            m.insert("dependencies".to_owned(), Json::Array(deps));
            Json::Object(m)
        })
        .collect();
    let active = index
        .active
        .iter()
        .map(|(id, (digest, state))| {
            let state = if *state == State::Enabled { "enabled" } else { "disabled" };
            (id.to_string(), Json::Object(BTreeMap::from([("digest".to_owned(), s(digest.to_string())), ("state".to_owned(), s(state.to_owned()))])))
        })
        .collect();
    let root = Json::Object(BTreeMap::from([
        ("active".to_owned(), Json::Object(active)),
        ("index_version".to_owned(), Json::Int(INDEX_VERSION)),
        ("objects".to_owned(), Json::Array(objects)),
    ]));
    json::canonical(&root).map_err(wire)
}

fn damaged() -> Error {
    Error::new(ErrorKind::InvalidInput, "the plugin store's index is damaged; packages weren't changed")
}

fn text(m: &mut BTreeMap<String, Json>, key: &str) -> Result<String, Error> {
    match m.remove(key) {
        Some(Json::Str(s)) => Ok(s),
        _ => Err(damaged()),
    }
}

fn decode_index(bytes: &[u8]) -> Result<Index, Error> {
    let Json::Object(mut root) = archive::parse(bytes, INDEX_LIMITS)? else { return Err(damaged()) };
    match root.remove("index_version") {
        Some(Json::Int(INDEX_VERSION)) => {}
        Some(Json::Int(v)) if v > INDEX_VERSION => return Err(Error::new(ErrorKind::Unsupported, "the plugin store was written by a newer NodeCraft")),
        _ => return Err(damaged()),
    }
    let mut index = Index::default();
    let Some(Json::Array(objects)) = root.remove("objects") else { return Err(damaged()) };
    for object in objects {
        let Json::Object(mut m) = object else { return Err(damaged()) };
        let entry = Entry {
            package_id: PackageId::parse(&text(&mut m, "package_id")?).map_err(|_| damaged())?,
            version: Version::parse(&text(&mut m, "version")?).map_err(|_| damaged())?,
            digest: PackageDigest::parse(&text(&mut m, "digest")?).map_err(|_| damaged())?,
            bytes: json::decimal(&text(&mut m, "bytes")?).map_err(|_| damaged())?,
            license: text(&mut m, "license")?,
            signed_by: match m.remove("signed_by") {
                Some(Json::Str(s)) => Some(s),
                None => None,
                Some(_) => return Err(damaged()),
            },
            registrations: match m.remove("registrations") {
                Some(Json::Array(items)) if items.len() <= nodecraft_plugin::manifest::MAX_REGISTRATIONS => items
                    .into_iter()
                    .map(|i| match i {
                        Json::Str(id) if RegisteredId::parse(&id).is_ok() => Ok(id),
                        _ => Err(damaged()),
                    })
                    .collect::<Result<_, _>>()?,
                _ => return Err(damaged()),
            },
            dependencies: match m.remove("dependencies") {
                Some(Json::Array(items)) if items.len() <= nodecraft_plugin::manifest::MAX_DEPENDENCIES => items
                    .into_iter()
                    .map(|i| {
                        let Json::Object(mut d) = i else { return Err(damaged()) };
                        let dep = Dependency {
                            package_id: PackageId::parse(&text(&mut d, "package_id")?).map_err(|_| damaged())?,
                            version: Version::parse(&text(&mut d, "version")?).map_err(|_| damaged())?,
                            digest: PackageDigest::parse(&text(&mut d, "digest")?).map_err(|_| damaged())?,
                        };
                        if d.is_empty() { Ok(dep) } else { Err(damaged()) }
                    })
                    .collect::<Result<_, _>>()?,
                _ => return Err(damaged()),
            },
        };
        if !m.is_empty() || index.objects.insert(entry.digest, entry).is_some() || index.objects.len() > MAX_OBJECTS {
            return Err(damaged());
        }
    }
    let Some(Json::Object(active)) = root.remove("active") else { return Err(damaged()) };
    for (id, value) in active {
        let Json::Object(mut m) = value else { return Err(damaged()) };
        let package = PackageId::parse(&id).map_err(|_| damaged())?;
        let digest = PackageDigest::parse(&text(&mut m, "digest")?).map_err(|_| damaged())?;
        let state = match text(&mut m, "state")?.as_str() {
            "enabled" => State::Enabled,
            "disabled" => State::Disabled,
            _ => return Err(damaged()),
        };
        if !m.is_empty() || index.objects.get(&digest).is_none_or(|e| e.package_id != package) {
            return Err(damaged());
        }
        index.active.insert(package, (digest, state));
    }
    if !root.is_empty() {
        return Err(damaged());
    }
    Ok(index)
}
