//! An Element bundle (#765): one library entry and its images in a file another person adds to
//! their own Elements library (`library.export_element`, `library.import_element`).
//!
//! The file is a ZIP of stored entries: `element.json` (the entry) and `media/<blake3>` (each
//! image, named by its hash and checked against it when read). Nothing else goes in, so a bundle
//! never carries keys, account data or a project's history.
use crate::element_library::Saved;
use nodecraft_format::MAX_ASSET_BYTES;
use nodecraft_nodes::element::MAX_IMAGES;
use nodecraft_types::{Blake3, Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};

/// What `element.json` says it is.
pub const BUNDLE_FORMAT: &str = "nodecraft.element";
/// The bundle format's version.
pub const BUNDLE_VERSION: u32 = 1;
/// The file name extension bundles are saved with.
pub const BUNDLE_EXTENSION: &str = "ncelement";
/// Largest `element.json`.
const MAX_ENTRY_BYTES: usize = 1024 * 1024;
/// Most entries in a bundle: the entry, its images and its sheet.
const MAX_FILES: usize = MAX_IMAGES + 2;
/// Largest bundle read: every image at its largest, and the entry.
pub const MAX_BUNDLE_BYTES: usize = (MAX_IMAGES + 1) * MAX_ASSET_BYTES + MAX_ENTRY_BYTES;

const ENTRY: &str = "element.json";
const MEDIA: &str = "media/";

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFile {
    format: String,
    version: u32,
    element: Saved,
}

/// The bundle for `entry`, with each image's bytes from `media` (checked by the caller). It is
/// written private: whether to share it is the receiver's choice.
pub fn encode(entry: &Saved, media: impl Fn(&Blake3) -> Result<Vec<u8>, Error>) -> Result<Vec<u8>, Error> {
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
    let failed = |_| Error::new(ErrorKind::Internal, "the Element bundle couldn't be written");
    let element = Saved { shared: false, ..entry.clone() };
    let text = serde_json::to_vec_pretty(&BundleFile { format: BUNDLE_FORMAT.into(), version: BUNDLE_VERSION, element })
        .map_err(|_| Error::new(ErrorKind::Internal, "the Element couldn't be written"))?;
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(ENTRY, options).map_err(failed)?;
    zip.write_all(&text).map_err(|_| Error::new(ErrorKind::Internal, "the Element bundle couldn't be written"))?;
    let mut written = std::collections::BTreeSet::new();
    for asset in entry.media() {
        // The sheet may be one of the images: each file goes in once.
        if !written.insert(asset.hash) {
            continue;
        }
        let bytes = media(&asset.hash)?;
        zip.start_file(format!("{MEDIA}{}", asset.hash), options).map_err(failed)?;
        zip.write_all(&bytes).map_err(|_| Error::new(ErrorKind::Internal, "the Element bundle couldn't be written"))?;
    }
    Ok(zip.finish().map_err(failed)?.into_inner())
}

/// The entry in a bundle and its images by hash, every image checked against its name and
/// every one the entry shows present. Refuses anything else in the file.
pub fn decode(bytes: &[u8]) -> Result<(Saved, BTreeMap<Blake3, Vec<u8>>), Error> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(invalid("that Element bundle is too large"));
    }
    let not_bundle = || invalid("that file isn't an Element bundle");
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| not_bundle())?;
    if archive.len() > MAX_FILES {
        return Err(invalid("that Element bundle holds more files than an Element has images"));
    }
    let mut entry = None;
    let mut media = BTreeMap::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|_| not_bundle())?;
        if file.encrypted() || file.is_symlink() || !file.is_file() {
            return Err(invalid("an Element bundle holds only plain files"));
        }
        let name = file.name().map_err(|_| not_bundle())?.into_owned();
        let cap = if name == ENTRY { MAX_ENTRY_BYTES } else { MAX_ASSET_BYTES };
        if file.size() > u64::try_from(cap).unwrap_or(u64::MAX) {
            return Err(invalid("a file in that Element bundle is too large"));
        }
        let mut data = Vec::new();
        (&mut file)
            .take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut data)
            .map_err(|_| invalid("that Element bundle is damaged"))?;
        if data.len() > cap {
            return Err(invalid("a file in that Element bundle is too large"));
        }
        if name == ENTRY {
            if entry.is_some() {
                return Err(not_bundle());
            }
            // What it is and its version first: a newer bundle may hold fields this build doesn't know.
            let value: serde_json::Value = serde_json::from_slice(&data).map_err(|_| invalid("that Element bundle's entry is damaged"))?;
            if value.get("format").and_then(serde_json::Value::as_str) != Some(BUNDLE_FORMAT) {
                return Err(not_bundle());
            }
            if value.get("version").and_then(serde_json::Value::as_u64) != Some(u64::from(BUNDLE_VERSION)) {
                return Err(Error::new(ErrorKind::Unsupported, "that Element bundle was written by a newer NodeCraft"));
            }
            let file: BundleFile = serde_json::from_value(value).map_err(|_| invalid("that Element bundle's entry is damaged"))?;
            entry = Some(file.element);
        } else {
            let hash: Blake3 = name.strip_prefix(MEDIA).and_then(|hash| hash.parse().ok()).ok_or_else(not_bundle)?;
            if Blake3::from_bytes(*blake3::hash(&data).as_bytes()) != hash {
                return Err(invalid("an image in that Element bundle is damaged"));
            }
            if media.insert(hash, data).is_some() {
                return Err(not_bundle());
            }
        }
    }
    let entry = entry.ok_or_else(not_bundle)?.checked()?;
    if entry.media().any(|asset| !media.contains_key(&asset.hash)) {
        return Err(invalid("that Element bundle is missing an image"));
    }
    if media.keys().any(|hash| !entry.media().any(|asset| asset.hash == *hash)) {
        return Err(invalid("that Element bundle holds an image its Element doesn't show"));
    }
    Ok((Saved { shared: false, ..entry }, media))
}
