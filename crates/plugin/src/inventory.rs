//! The file inventory (`inventory.json`): every file in a package with its exact length and
//! BLAKE3 hash, in path order. Its canonical JSON is what the package digest covers, so one set
//! of files has exactly one digest.
//!
//! Paths are portable: ASCII `[A-Za-z0-9._-]` segments joined by `/`, at most 128 bytes and 8
//! segments, no empty, `.` or `..` segment, no segment ending in `.`, no Windows device name, and
//! no two paths equal after ASCII case folding. `inventory.json` and `signature.json` are never
//! listed (the first is this file, the second signs its digest); `manifest.json` always is.
use crate::digest::PackageDigest;
use crate::error::{Code, Result, WireError, err};
use crate::id::located;
use crate::json::{self, Fields, Json, Limits};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Inventory format this host reads.
pub const INVENTORY_VERSION: u32 = 1;
/// Most files in a package.
pub const MAX_FILES: usize = 1024;
/// Most bytes of all files together, expanded.
pub const MAX_PACKAGE_BYTES: u64 = 128 * 1024 * 1024;
/// Longest path.
pub const MAX_PATH: usize = 128;
/// Most path segments.
pub const MAX_SEGMENTS: usize = 8;
/// The manifest's path.
pub const MANIFEST_PATH: &str = "manifest.json";
/// Files a package has but the inventory never lists.
pub const UNLISTED: [&str; 2] = ["inventory.json", "signature.json"];

const DEVICES: [&str; 24] = [
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
    "lpt6", "lpt7", "lpt8", "lpt9",
];

/// A portable relative path inside a package.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InventoryPath(String);

impl InventoryPath {
    pub fn parse(text: &str) -> Result<Self> {
        if text.is_empty() || text.len() > MAX_PATH || text.split('/').count() > MAX_SEGMENTS {
            return err(Code::BadPath, "a path is 1–128 bytes in at most 8 segments");
        }
        for segment in text.split('/') {
            let charset = segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
            if segment.is_empty() || !charset || segment.ends_with('.') {
                return err(Code::BadPath, "a path segment is [A-Za-z0-9._-], not empty, not . or .., not ending in .");
            }
            let stem = segment.split('.').next().unwrap_or("").to_ascii_lowercase();
            if DEVICES.contains(&stem.as_str()) {
                return err(Code::BadPath, "a path segment names a Windows device");
            }
        }
        // The inventory and signature are never package content, in any spelling: a path can't
        // point a manifest or a listing at a file the digest doesn't cover.
        if UNLISTED.iter().any(|name| text.eq_ignore_ascii_case(name)) {
            return err(Code::BadPath, "inventory.json and signature.json aren't package content");
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for InventoryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One listed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: InventoryPath,
    pub bytes: u64,
    pub blake3: [u8; 32],
}

/// A package's inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inventory {
    files: BTreeMap<InventoryPath, FileEntry>,
}

impl Inventory {
    /// Builds an inventory from the package's files (for authors, the verifier and tests).
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Result<Self> {
        let entries = files.into_iter().map(|(path, bytes)| {
            Ok(FileEntry { path: InventoryPath::parse(path)?, bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX), blake3: *blake3::hash(bytes).as_bytes() })
        });
        Self::from_entries(entries.collect::<Result<Vec<_>>>()?)
    }

    fn from_entries(entries: Vec<FileEntry>) -> Result<Self> {
        if entries.len() > MAX_FILES {
            return err(Code::TooLarge, "a package lists more than 1,024 files");
        }
        let mut files = BTreeMap::new();
        let mut folded = BTreeSet::new();
        let mut total: u64 = 0;
        for (i, entry) in entries.into_iter().enumerate() {
            let at = format!("/files/{i}");
            if UNLISTED.contains(&entry.path.as_str()) {
                return Err(WireError::new(Code::BadPath, "inventory.json and signature.json are never listed").at(&at));
            }
            if !folded.insert(entry.path.as_str().to_ascii_lowercase()) {
                return Err(WireError::new(Code::Collision, "two paths are the same after case folding").at(&at));
            }
            total = total
                .checked_add(entry.bytes)
                .filter(|t| *t <= MAX_PACKAGE_BYTES)
                .ok_or_else(|| WireError::new(Code::TooLarge, "the files are larger than 128 MiB together").at(&at))?;
            files.insert(entry.path.clone(), entry);
        }
        if !files.keys().any(|p| p.as_str() == MANIFEST_PATH) {
            return err(Code::MissingField, "the inventory doesn't list manifest.json");
        }
        Ok(Self { files })
    }

    /// Decodes `inventory.json`. Entries must already be in path order; bytes needn't be
    /// canonical (the digest is taken over [`Inventory::canonical_json`]).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let limits = Limits { bytes: 512 * 1024, depth: 4, string_bytes: 256, entries: MAX_FILES + 1, raw_numbers: false };
        let mut f = Fields::new(json::parse(bytes, limits)?, "")?;
        if f.u32("inventory_version")? != INVENTORY_VERSION {
            return Err(WireError::new(Code::BadVersion, "this host reads inventory version 1").at("/inventory_version"));
        }
        let items = f.array("files", MAX_FILES)?;
        f.finish()?;
        let mut entries = Vec::with_capacity(items.len());
        for (i, item) in items.into_iter().enumerate() {
            let at = format!("/files/{i}");
            let mut e = Fields::new(item, &at)?;
            let path = located(InventoryPath::parse(&e.string("path")?), &e.path("path"))?;
            let bytes = located(json::decimal(&e.string("bytes")?), &e.path("bytes"))?;
            let blake3 = located(json::hex::<32>(&e.string("blake3")?), &e.path("blake3"))?;
            e.finish()?;
            if entries.last().is_some_and(|last: &FileEntry| last.path >= path) {
                return Err(WireError::new(Code::NotCanonical, "files are listed in path order, each once").at(&at));
            }
            entries.push(FileEntry { path, bytes, blake3 });
        }
        Self::from_entries(entries)
    }

    /// The inventory in canonical JSON.
    pub fn canonical_json(&self) -> String {
        let files = self
            .files
            .values()
            .map(|e| {
                Json::Object(BTreeMap::from([
                    ("blake3".to_owned(), Json::Str(json::to_hex(&e.blake3))),
                    ("bytes".to_owned(), Json::Str(e.bytes.to_string())),
                    ("path".to_owned(), Json::Str(e.path.to_string())),
                ]))
            })
            .collect();
        let value =
            Json::Object(BTreeMap::from([("files".to_owned(), Json::Array(files)), ("inventory_version".to_owned(), Json::Int(i64::from(INVENTORY_VERSION)))]));
        // Strings and integers only: always canonicalisable.
        json::canonical(&value).unwrap_or_default()
    }

    /// The package digest.
    pub fn digest(&self) -> PackageDigest {
        PackageDigest::of_inventory(&self.canonical_json())
    }

    pub fn files(&self) -> impl Iterator<Item = &FileEntry> {
        self.files.values()
    }

    pub fn get(&self, path: &str) -> Option<&FileEntry> {
        InventoryPath::parse(path).ok().and_then(|p| self.files.get(&p))
    }

    /// Checks the package's file names against the listing: every listed file present, nothing
    /// else besides `inventory.json` and `signature.json`.
    pub fn check_listing<'a>(&self, present: impl IntoIterator<Item = &'a str>) -> Result<()> {
        let mut seen = BTreeSet::new();
        for path in present {
            if UNLISTED.contains(&path) {
                continue;
            }
            if self.get(path).is_none() {
                return Err(WireError::new(Code::BadPath, "the package holds a file the inventory doesn't list").at(&format!("/files/{path}")));
            }
            seen.insert(path);
        }
        match self.files.keys().find(|p| !seen.contains(p.as_str())) {
            Some(missing) => Err(WireError::new(Code::MissingField, "a listed file is missing from the package").at(&format!("/files/{missing}"))),
            None => Ok(()),
        }
    }

    /// Checks that every file `manifest` names (its module and descriptors) is listed, so all of
    /// them are covered by the digest.
    pub fn check_manifest(&self, manifest: &crate::manifest::Manifest) -> Result<()> {
        if let crate::manifest::Runtime::RustWasm { module } = &manifest.runtime
            && !self.files.contains_key(module)
        {
            return Err(WireError::new(Code::BadPath, "the manifest names a file the inventory doesn't list").at("/runtime/module"));
        }
        for (i, r) in manifest.registrations.iter().enumerate() {
            if !self.files.contains_key(&r.descriptor) {
                return Err(WireError::new(Code::BadPath, "the manifest names a file the inventory doesn't list").at(&format!("/registrations/{i}/descriptor")));
            }
        }
        Ok(())
    }

    /// Checks a file read from the package against its entry: listed, same length, same hash.
    /// A reader checks the length as it streams, before hashing the rest.
    pub fn verify(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let entry = self.get(path).ok_or_else(|| WireError::new(Code::BadPath, "a file the inventory doesn't list").at(&format!("/files/{path}")))?;
        if u64::try_from(bytes.len()).ok() != Some(entry.bytes) {
            return Err(WireError::new(Code::BadDigest, "a file's length differs from the inventory").at(&format!("/files/{path}")));
        }
        if *blake3::hash(bytes).as_bytes() != entry.blake3 {
            return Err(WireError::new(Code::BadDigest, "a file's hash differs from the inventory").at(&format!("/files/{path}")));
        }
        Ok(())
    }
}
