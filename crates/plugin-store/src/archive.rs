//! Reading a `.ncraft-plugin` archive: a ZIP of stored (uncompressed) entries holding the
//! package's files, `inventory.json` and optionally `signature.json`.
//!
//! Everything is checked before anything is kept: the archive's size; its ZIP structure, on the
//! raw bytes (`layout`: stored regular files laid end to end, every header agreeing, nothing
//! hidden, so a declared size can't hide a bomb and no reader sees other files); every entry's
//! name (a portable path, see `nodecraft_plugin::inventory`); the listing against the inventory (nothing unlisted, nothing missing, no two names
//! equal after case folding); every file's length and hash; the canonical manifest and that every
//! file it names is listed; the descriptors' JSON; a license file; and the signature. Nothing in
//! a package runs, here or later in this phase.
use nodecraft_plugin::inventory::{InventoryPath, MANIFEST_PATH};
use nodecraft_plugin::json;
use nodecraft_plugin::manifest::Runtime;
use nodecraft_plugin::preserve::DescriptorSnapshot;
use nodecraft_plugin::signature::PackageSignature;
use nodecraft_plugin::{Inventory, Manifest, PackageDigest, WireError};
use nodecraft_types::{Error, ErrorKind};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::Arc;
use zip::CompressionMethod;

/// What an archive may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// The archive itself.
    pub archive_bytes: usize,
    /// All entries together.
    pub expanded_bytes: u64,
    /// Entries, including `inventory.json`, `signature.json` and directory entries.
    pub entries: usize,
}

impl ArchiveLimits {
    /// The RFC's ceilings (§7): 32 MiB archives, 128 MiB expanded, 1,024 listed files plus the
    /// inventory, signature and up to 1,024 directory entries.
    pub const DEFAULT: ArchiveLimits = ArchiveLimits { archive_bytes: 32 * 1024 * 1024, expanded_bytes: 128 * 1024 * 1024, entries: 2 * 1024 + 2 };
}

/// A verified package: its files are exactly those its inventory lists, with the listed hashes.
#[derive(Clone, Debug)]
pub struct Package {
    pub manifest: Manifest,
    pub inventory: Inventory,
    pub digest: PackageDigest,
    pub signature: Option<PackageSignature>,
    pub files: BTreeMap<InventoryPath, Arc<[u8]>>,
}

impl Package {
    /// The license and notice files at the package's root.
    pub fn notices(&self) -> impl Iterator<Item = &InventoryPath> {
        self.files.keys().filter(|p| is_notice(p.as_str()))
    }
}

fn is_notice(path: &str) -> bool {
    let upper = path.to_ascii_uppercase();
    !path.contains('/') && ["LICENSE", "LICENCE", "COPYING", "NOTICE"].iter().any(|n| upper.starts_with(n))
}

fn refused(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, detail)
}

/// A contract error, with its code and location (never a value).
pub fn wire(error: WireError) -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the package is invalid ({error})"))
}

/// Verifies `bytes` as a data package.
pub fn open(bytes: &[u8], limits: ArchiveLimits) -> Result<Package, Error> {
    if bytes.len() > limits.archive_bytes {
        return Err(refused(format!("the package is larger than {} MiB", limits.archive_bytes / (1024 * 1024))));
    }
    // The structure first, on the raw bytes: one layout, so one set of files (see `layout`).
    let entries = crate::layout::check(bytes, limits.entries)?;
    let mut files: BTreeMap<String, Arc<[u8]>> = BTreeMap::new();
    let mut folded = BTreeSet::new();
    let mut expanded: u64 = 0;
    for entry in entries {
        let name = entry.name;
        let special = name == "inventory.json" || name == "signature.json";
        if !special {
            InventoryPath::parse(name).map_err(wire)?;
        }
        if !folded.insert(name.to_ascii_lowercase()) {
            return Err(refused("two package entries have the same name, ignoring case"));
        }
        let size = u64::try_from(entry.data.len()).unwrap_or(u64::MAX);
        expanded = expanded.checked_add(size).filter(|e| *e <= limits.expanded_bytes).ok_or_else(|| refused("the package expands past its limit"))?;
        files.insert(name.to_owned(), entry.data.into());
    }
    let inventory_bytes = files.remove("inventory.json").ok_or_else(|| refused("the package has no inventory.json"))?;
    let inventory = Inventory::decode(&inventory_bytes).map_err(wire)?;
    let signature = files.remove("signature.json").map(|bytes| PackageSignature::decode(&bytes)).transpose().map_err(wire)?;
    inventory.check_listing(files.keys().map(String::as_str)).map_err(wire)?;
    let mut checked = BTreeMap::new();
    for (name, bytes) in files {
        inventory.verify(&name, &bytes).map_err(wire)?;
        checked.insert(InventoryPath::parse(&name).map_err(wire)?, bytes);
    }
    let manifest_path = InventoryPath::parse(MANIFEST_PATH).map_err(wire)?;
    let manifest = Manifest::decode(checked.get(&manifest_path).map(|b| &b[..]).unwrap_or_default()).map_err(wire)?;
    inventory.check_manifest(&manifest).map_err(wire)?;
    if let Runtime::RustWasm { .. } = manifest.runtime {
        return Err(Error::new(ErrorKind::Unsupported, "only data and text action packages can be installed; module packages come later"));
    }
    for r in &manifest.registrations {
        let bytes = checked.get(&r.descriptor).map(|b| &b[..]).unwrap_or_default();
        DescriptorSnapshot::of(bytes).map_err(|e| wire(e.at(&format!("/{}", r.descriptor))))?;
    }
    let package = Package { digest: inventory.digest(), manifest, inventory, signature, files: checked };
    if package.notices().next().is_none() {
        return Err(refused("the package has no LICENSE, COPYING or NOTICE file at its root"));
    }
    Ok(package)
}

/// Writes `files` (path → bytes) as a package archive with its inventory, and `signature` if
/// given: for authors' tools and tests.
pub fn build(files: &[(&str, &[u8])], signature: Option<&[u8]>) -> Result<Vec<u8>, Error> {
    use std::io::Write;
    let inventory = Inventory::from_files(files.iter().copied()).map_err(wire)?;
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // Fixed time and mode, never the clock or the file's own, so the same files give the same bytes.
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::DEFAULT)
        .unix_permissions(0o644);
    let canonical = inventory.canonical_json();
    let extra = [("inventory.json", canonical.as_bytes())].into_iter().chain(signature.map(|s| ("signature.json", s)));
    for (name, bytes) in files.iter().copied().chain(extra) {
        out.start_file(name, options).map_err(|_| Error::new(ErrorKind::Internal, "the package couldn't be written"))?;
        out.write_all(bytes).map_err(|_| Error::new(ErrorKind::Internal, "the package couldn't be written"))?;
    }
    Ok(out.finish().map_err(|_| Error::new(ErrorKind::Internal, "the package couldn't be written"))?.into_inner())
}

/// Parses JSON under the contract profile, for callers that read the store index.
pub(crate) fn parse(bytes: &[u8], limits: json::Limits) -> Result<json::Json, Error> {
    json::parse(bytes, limits).map_err(wire)
}
