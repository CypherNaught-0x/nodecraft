//! Building a `.ncraft-plugin` from an author's files, the same way every time (#175).
//!
//! The same files give the same bytes and the same digest on any machine: entries are written in
//! path order, stored, with the fixed timestamp the ZIP writer uses without a clock, and the
//! manifest is written in its canonical form, so authors can keep a readable `manifest.json`.
//! Nothing an author supplies runs: there are no build or install hooks. A packed archive is
//! opened again with the verifier install uses before it is handed back.
use crate::archive::{self, ArchiveLimits, wire};
use nodecraft_plugin::inventory::MANIFEST_PATH;
use nodecraft_plugin::manifest::MANIFEST_LIMITS;
use nodecraft_plugin::{Inventory, PackageDigest, json};
use nodecraft_types::{Error, ErrorKind};

/// Names the package format writes itself; an author's file may not take them.
pub const RESERVED_FILES: [&str; 2] = ["inventory.json", "signature.json"];

/// A packed package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed {
    pub bytes: Vec<u8>,
    pub digest: PackageDigest,
    /// The canonical `inventory.json` the digest is taken over.
    pub inventory: String,
}

/// The files as they go into the package: in path order, with the manifest canonical.
fn prepared(mut files: Vec<(String, Vec<u8>)>) -> Result<Vec<(String, Vec<u8>)>, Error> {
    if let Some((name, _)) = files.iter().find(|(name, _)| RESERVED_FILES.contains(&name.as_str())) {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{name} is written by the package format; remove it from the package's files")));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.windows(2).any(|w| matches!(w, [a, b] if a.0 == b.0)) {
        return Err(Error::new(ErrorKind::InvalidInput, "two files have the same path"));
    }
    let manifest = files
        .iter_mut()
        .find(|(name, _)| name == MANIFEST_PATH)
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "the package has no manifest.json at its root"))?;
    let parsed = json::parse(&manifest.1, MANIFEST_LIMITS).map_err(|e| wire(e.at("/manifest.json")))?;
    manifest.1 = json::canonical(&parsed).map_err(wire)?.into_bytes();
    Ok(files)
}

/// The canonical inventory of `files` and the digest a package of them has, without packing.
pub fn inventory(files: Vec<(String, Vec<u8>)>) -> Result<(String, PackageDigest), Error> {
    let files = prepared(files)?;
    let inventory = Inventory::from_files(files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice()))).map_err(wire)?;
    Ok((inventory.canonical_json(), inventory.digest()))
}

/// Packs `files` (path → bytes, `/`-separated) with `signature` if given, and checks the result
/// with the install verifier. A package the verifier would refuse is never written.
pub fn pack(files: Vec<(String, Vec<u8>)>, signature: Option<&[u8]>) -> Result<Packed, Error> {
    let files = prepared(files)?;
    let borrowed: Vec<(&str, &[u8])> = files.iter().map(|(name, bytes)| (name.as_str(), bytes.as_slice())).collect();
    let bytes = archive::build(&borrowed, signature)?;
    let package = archive::open(&bytes, ArchiveLimits::DEFAULT)?;
    Ok(Packed { bytes, digest: package.digest, inventory: package.inventory.canonical_json() })
}
