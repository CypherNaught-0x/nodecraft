use crate::{AssetStore, MAX_ASSET_BYTES, MAX_ASSETS, MAX_TOTAL_ASSET_BYTES, invalid};
use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read, Write},
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
/// Current graph writer/schema declaration, not the greatest supported container version.
/// Wire 4 is permanently reserved for streaming graph-v3 content. The next graph schema
/// must skip to 5 and supply a frozen-v3 reader/migration before changing this declaration.
pub(crate) const VERSION: u32 = 3;
pub(crate) const GRAPH_V3: u32 = 3;
pub(crate) const STREAMING_V4: u32 = 4;

pub(crate) fn is_classic_wire(version: u32) -> bool {
    matches!(version, 1 | 2 | GRAPH_V3)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub project: Project,
    pub assets: AssetStore,
    /// Increment for each saved checkpoint, including when the document has not changed.
    pub epoch: u64,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    #[serde(default)]
    epoch: u64,
    project: Project,
}

pub fn encode(snapshot: &Snapshot) -> Result<Vec<u8>, Error> {
    if !snapshot.assets.is_resident_small() {
        return Err(Error::new(ErrorKind::Unsupported, "legacy byte encoder requires small resident assets; use a streaming archive plan"));
    }
    let referenced = snapshot.assets.validate_project(&snapshot.project)?;
    let json = manifest_json(&snapshot.project, snapshot.epoch)?;
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("project.json", options).map_err(|_| invalid("cannot encode project archive"))?;
    writer.write_all(&json).map_err(|_| invalid("cannot encode project manifest"))?;
    for hash in referenced {
        let bytes = snapshot.assets.get(&hash).ok_or_else(|| invalid("project references a missing asset"))?;
        writer.start_file(format!("assets/{hash}"), options).map_err(|_| invalid("cannot encode asset entry"))?;
        writer.write_all(bytes).map_err(|_| invalid("cannot encode asset bytes"))?;
    }
    let bytes = writer.finish().map_err(|_| invalid("cannot finish project archive"))?.into_inner();
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(invalid("project archive exceeds 64 MiB limit"));
    }
    Ok(bytes)
}

/// Frozen-v1 admission for an already current graph. No assets are copied or storage touched.
/// Uses a borrowed traversal after graph validation, so editing an incrementally large
/// document does not impose the smaller archive manifest ceiling.
pub fn validate_v1_project(project: &Project) -> Result<(), Error> {
    #[cfg(any(test, feature = "validation-metrics"))]
    crate::validation_metrics::record(|metrics| metrics.full_v1_checks = metrics.full_v1_checks.saturating_add(1));
    project.validate()?;
    crate::compatibility::project(project)
}
/// Current writer admission for Save availability. The real archive manifest has this same cap;
/// incremental document edits use bounded replay admission instead of imposing it per edit.
pub fn validate_current_project(project: &Project) -> Result<(), Error> {
    project.validate()?;
    manifest_json(project, u64::MAX).map(|_| ())
}
fn manifest_json(project: &Project, epoch: u64) -> Result<Vec<u8>, Error> {
    #[derive(Serialize)]
    struct ManifestRef<'a> {
        version: u32,
        epoch: u64,
        project: &'a Project,
    }
    let json = bounded_json(&ManifestRef { version: VERSION, epoch, project }, MAX_MANIFEST_BYTES)?;
    Ok(json)
}

pub fn decode(bytes: &[u8]) -> Result<Snapshot, Error> {
    decode_versioned(bytes).map(|(snapshot, _)| snapshot)
}

pub(crate) fn decode_versioned(bytes: &[u8]) -> Result<(Snapshot, u32), Error> {
    let entries = preflight(bytes)?;
    let mut archive = ZipArchive::new(Cursor::new(bytes)).map_err(|_| invalid("invalid project ZIP directory"))?;
    if archive.len() != entries {
        return Err(invalid("ZIP directory parser disagreement"));
    }
    let mut json = Vec::new();
    let mut assets = AssetStore::default();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).map_err(|_| invalid("invalid ZIP entry"))?;
        if file.encrypted() || file.is_symlink() || file.compression() != CompressionMethod::Stored || file.compressed_size() != file.size() {
            return Err(invalid("project entries must be stored regular files without encryption"));
        }
        let name = file.name().map_err(|_| invalid("invalid ZIP entry name"))?.into_owned();
        let cap = if name == "project.json" { MAX_MANIFEST_BYTES } else { MAX_ASSET_BYTES };
        let mut data = Vec::new();
        (&mut file).take(cap as u64 + 1).read_to_end(&mut data).map_err(|_| invalid("invalid ZIP contents or checksum"))?;
        if data.len() > cap || data.len() as u64 != file.size() {
            return Err(invalid("ZIP entry exceeds declared size or content limit"));
        }
        if name == "project.json" {
            json = data;
        } else {
            let expected: Blake3 = name.strip_prefix("assets/").ok_or_else(|| invalid("unexpected ZIP path"))?.parse()?;
            if crate::hash(&data) != expected {
                return Err(invalid("asset content does not match its BLAKE3 name"));
            }
            assets.insert(&data)?;
        }
    }
    // Gate version before deserializing typed enums/graph data from a newer writer.
    #[derive(Deserialize)]
    struct Version {
        version: u32,
    }
    let version: Version = serde_json::from_slice(&json).map_err(|_| invalid("invalid project manifest or missing format version"))?;
    let json = migrate(version.version, &json)?;
    let manifest: Manifest = serde_json::from_slice(json).map_err(|_| invalid("invalid project graph or manifest"))?;
    assets.validate_project(&manifest.project)?;
    Ok((Snapshot { project: manifest.project, assets, epoch: manifest.epoch }, version.version))
}

/// Structurally decode a bounded index. Version4 assets remain explicitly unverified until
/// consumption; legacy-v1 content validation retains its published eager admission semantics.
pub fn decode_indexed(index: &crate::zip_index::ArchiveIndex, context: crate::AssetContext) -> Result<Snapshot, Error> {
    let snapshot = decode_indexed_presence(index, context)?;
    if is_classic_wire(index.version()) {
        for hash in snapshot.assets.hashes() {
            snapshot.assets.verified_content(hash, &crate::asset_source::TransferCancel::default(), &crate::asset_source::StreamMetrics::default())?;
        }
        snapshot.assets.validate_project(&snapshot.project)?;
    }
    Ok(snapshot)
}
pub(crate) fn decode_indexed_presence(index: &crate::zip_index::ArchiveIndex, context: crate::AssetContext) -> Result<Snapshot, Error> {
    migrate(index.version(), index.manifest())?;
    let manifest: Manifest = serde_json::from_slice(index.manifest()).map_err(|_| invalid("invalid project graph or manifest"))?;
    let assets = AssetStore::from_index(index, context).map_err(|error| nodecraft_types::Error::new(nodecraft_types::ErrorKind::Conflict, error.message))?;
    assets.validate_project_presence(&manifest.project)?;
    Ok(Snapshot { project: manifest.project, assets, epoch: manifest.epoch })
}

pub(crate) fn bounded_json(value: &impl Serialize, cap: usize) -> Result<Vec<u8>, Error> {
    struct Bounded {
        bytes: Vec<u8>,
        cap: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len().saturating_add(bytes.len()) > self.cap {
                return Err(std::io::Error::other("JSON limit exceeded"));
            }
            self.bytes.try_reserve(bytes.len()).map_err(|_| std::io::Error::other("cannot allocate bounded JSON"))?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded { bytes: Vec::new(), cap };
    serde_json::to_writer(&mut writer, value).map_err(|_| invalid("serialized data exceeds JSON limit"))?;
    Ok(writer.bytes)
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Error> {
    let data = bytes.get(at..at.saturating_add(2)).ok_or_else(|| invalid("truncated ZIP header"))?;
    Ok(u16::from_le_bytes(data.try_into().map_err(|_| invalid("invalid ZIP header"))?))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, Error> {
    let data = bytes.get(at..at.saturating_add(4)).ok_or_else(|| invalid("truncated ZIP header"))?;
    Ok(u32::from_le_bytes(data.try_into().map_err(|_| invalid("invalid ZIP header"))?))
}

/// ZIP metadata is untrusted too. Bound counts, names, sizes and offsets BEFORE the ZIP
/// library allocates its directory. ZIP64/multi-disk/extra fields are outside format v1.
fn preflight(bytes: &[u8]) -> Result<usize, Error> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(invalid("archive exceeds 64 MiB limit"));
    }
    let end = bytes.len().checked_sub(22).ok_or_else(|| invalid("truncated ZIP archive"))?;
    if u32_at(bytes, end)? != 0x06054b50 || u16_at(bytes, end + 4)? != 0 || u16_at(bytes, end + 6)? != 0 || u16_at(bytes, end + 20)? != 0 {
        return Err(invalid("unsupported ZIP footer; no split archives or archive comments"));
    }
    let count = usize::from(u16_at(bytes, end + 10)?);
    if count == 0 || count > MAX_ASSETS + 1 || usize::from(u16_at(bytes, end + 8)?) != count {
        return Err(invalid("invalid ZIP entry count"));
    }
    let start = u32_at(bytes, end + 16)? as usize;
    let size = u32_at(bytes, end + 12)? as usize;
    if start.checked_add(size) != Some(end) {
        return Err(invalid("invalid ZIP directory extent"));
    }
    let mut cursor = start;
    let mut names = BTreeSet::new();
    let mut total = 0_usize;
    let mut ranges = Vec::new();
    for _ in 0..count {
        let header = bytes.get(cursor..cursor.saturating_add(46)).ok_or_else(|| invalid("truncated ZIP directory"))?;
        if u32_at(header, 0)? != 0x02014b50 || u16_at(header, 8)? & !0x0800 != 0 || u16_at(header, 10)? != 0 || u16_at(header, 34)? != 0 {
            return Err(invalid("unsupported ZIP entry flags or compression"));
        }
        let mode = u32_at(header, 38)? >> 16;
        if mode & 0xf000 != 0 && mode & 0xf000 != 0x8000 {
            return Err(invalid("ZIP entry is not a regular file"));
        }
        let length = u32_at(header, 24)? as usize;
        if u32_at(header, 20)? as usize != length || u16_at(header, 30)? != 0 || u16_at(header, 32)? != 0 {
            return Err(invalid("unsupported ZIP extra data or compressed entry"));
        }
        let name_len = usize::from(u16_at(header, 28)?);
        if !(1..=71).contains(&name_len) {
            return Err(invalid("invalid ZIP entry name length"));
        }
        let name_start = cursor.saturating_add(46);
        let name_end = name_start.saturating_add(name_len);
        let name = bytes.get(name_start..name_end).ok_or_else(|| invalid("truncated ZIP entry name"))?;
        let manifest = name == b"project.json";
        if !manifest
            && !(name.len() == 71
                && name.starts_with(b"assets/")
                && name.get(7..).is_some_and(|tail| tail.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))))
        {
            return Err(invalid("unexpected ZIP path; only project.json and assets/<blake3> are allowed"));
        }
        if !names.insert(name) {
            return Err(invalid("duplicate ZIP path"));
        }
        if length > if manifest { MAX_MANIFEST_BYTES } else { MAX_ASSET_BYTES } {
            return Err(invalid("ZIP entry exceeds size limit"));
        }
        if !manifest {
            total = total.saturating_add(length);
        }
        if total > MAX_TOTAL_ASSET_BYTES {
            return Err(invalid("ZIP assets exceed total size limit"));
        }
        let offset = u32_at(header, 42)? as usize;
        let local = bytes.get(offset..offset.saturating_add(30)).ok_or_else(|| invalid("truncated local ZIP header"))?;
        if u32_at(local, 0)? != 0x04034b50
            || u16_at(local, 6)? != u16_at(header, 8)?
            || u16_at(local, 8)? != 0
            || u32_at(local, 14)? != u32_at(header, 16)?
            || u32_at(local, 18)? as usize != length
            || u32_at(local, 22)? as usize != length
            || usize::from(u16_at(local, 26)?) != name_len
            || u16_at(local, 28)? != 0
        {
            return Err(invalid("inconsistent local ZIP header"));
        }
        let local_name_start = offset.saturating_add(30);
        let data_start = local_name_start.saturating_add(name_len);
        let data_end = data_start.saturating_add(length);
        if bytes.get(local_name_start..data_start) != Some(name) || data_end > start {
            return Err(invalid("invalid local ZIP entry extent"));
        }
        if manifest {
            // Identify an incompatible family before applying small-host asset limits or
            // decoding graph data. Streaming writers put this bounded entry first.
            #[derive(Deserialize)]
            struct Version {
                version: u32,
            }
            let json = bytes.get(data_start..data_end).ok_or_else(|| invalid("invalid manifest extent"))?;
            let version: Version = serde_json::from_slice(json).map_err(|_| invalid("invalid project manifest or missing format version"))?;
            if !is_classic_wire(version.version) {
                return Err(Error::new(ErrorKind::Unsupported, "unsupported project format version; use a compatible NodeCraft build"));
            }
        }
        ranges.push((offset, data_end));
        cursor = name_end;
    }
    ranges.sort_unstable();
    let mut previous = 0;
    for (start, end) in ranges {
        if start != previous {
            return Err(invalid("overlapping or hidden ZIP entry data"));
        }
        previous = end;
    }
    if previous != start || cursor != end || !names.contains(b"project.json".as_slice()) {
        return Err(invalid("missing manifest or inconsistent ZIP directory"));
    }
    Ok(count)
}

/// Route wire families before typed parsing. Legacy roots retain their frozen role guards;
/// both classic 3 and reserved streaming 4 carry graph-v3, never a mutable "latest" schema.
fn migrate(from: u32, json: &[u8]) -> Result<&[u8], Error> {
    match from {
        1 => {
            crate::legacy::validate(json)?;
            Ok(json)
        }
        2 => {
            crate::v2::validate(json)?;
            Ok(json)
        }
        GRAPH_V3 | STREAMING_V4 => admit_graph_v3(json, VERSION),
        _ => Err(Error::new(ErrorKind::Unsupported, "unsupported project format version; update NodeCraft or use a compatible exporter")),
    }
}

/// Current typed Project parsing is the v3 reader only while the schema declaration is 3.
/// This is a future migration stop, not a claim that current serde freezes all future fields.
fn admit_graph_v3(json: &[u8], reader_schema: u32) -> Result<&[u8], Error> {
    if reader_schema != GRAPH_V3 {
        return Err(Error::new(ErrorKind::Unsupported, "graph-v3 migration is required before reading wire families 3 or 4 with this schema"));
    }
    Ok(json)
}

#[cfg(test)]
mod wire_family_tests {
    use super::*;

    #[test]
    fn v3_reader_gate_fails_closed_before_typed_parsing_after_a_schema_change() {
        let malformed = b"not a typed project";
        assert_eq!(admit_graph_v3(malformed, 3).unwrap(), malformed);
        for schema in [4, 5] {
            assert_eq!(admit_graph_v3(malformed, schema).unwrap_err().kind, ErrorKind::Unsupported);
        }
        assert!(is_classic_wire(3));
        assert!(!is_classic_wire(4));
        assert!(!is_classic_wire(5));
    }
}
