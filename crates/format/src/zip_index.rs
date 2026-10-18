//! Bounded stored-ZIP metadata admission. Indexed asset ranges are explicitly unverified.
//! No filesystem path is extracted and no asset payload is read by this module.
use crate::asset_source::{AssetRange, MAX_LARGE_ARCHIVE_BYTES, MAX_LARGE_ASSET_BYTES, MAX_LARGE_TOTAL_ASSET_BYTES, StreamMetrics, TransferCancel};
use nodecraft_types::{Blake3, Error, ErrorKind};
use serde::Deserialize;
use std::collections::BTreeSet;

const MAX_DIRECTORY: u64 = 512 * 1024;
const MAX_ENTRIES: usize = 1025;
const MAX_MANIFEST: u64 = 4 * 1024 * 1024;
const U32_SENTINEL: u64 = u32::MAX as u64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryName {
    Manifest,
    Asset(Blake3),
}
#[derive(Clone, Debug)]
pub struct IndexedEntry {
    pub name: EntryName,
    pub data: AssetRange,
    pub crc32: u32,
    pub local_offset: u64,
    pub data_offset: u64,
    pub flags: u16,
}
impl PartialEq for IndexedEntry {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.crc32 == other.crc32
            && self.local_offset == other.local_offset
            && self.flags == other.flags
            && self.data_offset == other.data_offset
            && self.data.byte_len() == other.data.byte_len()
    }
}
impl Eq for IndexedEntry {}
/// This identity compares admitted metadata and raw manifest bytes, not payload integrity.
/// It is used when reindexing a privately frozen candidate after copying a mutable source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexIdentity {
    pub archive_bytes: u64,
    pub directory_offset: u64,
    pub version: u32,
    pub manifest: Vec<u8>,
    pub entries: Vec<IndexedEntry>,
}
pub struct ArchiveIndex {
    identity: IndexIdentity,
}
impl ArchiveIndex {
    pub fn identity(&self) -> &IndexIdentity {
        &self.identity
    }
    pub fn entries(&self) -> &[IndexedEntry] {
        &self.identity.entries
    }
    pub fn manifest(&self) -> &[u8] {
        &self.identity.manifest
    }
    pub fn version(&self) -> u32 {
        self.identity.version
    }
    /// Reads bounded directory/local metadata and the manifest only. Callers must next
    /// apply the frozen graph-role gate and structural reference admission. Content-address,
    /// CRC, MIME and metadata validation of each asset remains a separate verifier step.
    pub fn read(source: &AssetRange, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Self, Error> {
        cancel.check()?;
        if source.byte_len() > MAX_LARGE_ARCHIVE_BYTES {
            return Err(invalid("project archive exceeds 65 GiB limit"));
        }
        let footer = Footer::read(source, metrics)?;
        if footer.count == 0 || footer.count > MAX_ENTRIES as u64 || footer.directory_bytes > MAX_DIRECTORY {
            return Err(invalid("ZIP directory exceeds bounded entry or metadata limits"));
        }
        if add(footer.directory_offset, footer.directory_bytes)? != footer.end {
            return Err(invalid("invalid ZIP directory extent"));
        }
        let directory = source.slice(footer.directory_offset, footer.directory_bytes)?.read_small(MAX_DIRECTORY as usize, metrics)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(usize::try_from(footer.count).map_err(|_| invalid("invalid ZIP count"))?)
            .map_err(|_| invalid("cannot allocate bounded ZIP index"))?;
        let mut names = BTreeSet::new();
        let mut cursor = 0usize;
        let mut total = 0u64;
        let mut uses_zip64 = footer.zip64;
        for _ in 0..footer.count {
            cancel.check()?;
            let header = take(&directory, cursor, 46)?;
            if n32(header, 0)? != 0x02014b50 || n16(header, 8)? & !0x0800 != 0 || n16(header, 10)? != 0 || n16(header, 34)? != 0 {
                return Err(invalid("unsupported ZIP flags, compression or split entry"));
            }
            let mode = n32(header, 38)? >> 16;
            if mode & 0xf000 != 0 && mode & 0xf000 != 0x8000 {
                return Err(invalid("ZIP entry must be a regular file"));
            }
            let name_len = usize::from(n16(header, 28)?);
            let extra_len = usize::from(n16(header, 30)?);
            if !(1..=71).contains(&name_len) || extra_len > 28 || n16(header, 32)? != 0 {
                return Err(invalid("unsupported ZIP name, extra data or comment"));
            }
            let name_at = cursor.checked_add(46).ok_or_else(|| invalid("ZIP metadata extent overflow"))?;
            let extra_at = name_at.checked_add(name_len).ok_or_else(|| invalid("ZIP metadata extent overflow"))?;
            let name_bytes = take(&directory, name_at, name_len)?;
            let name = parse_name(name_bytes)?;
            if !names.insert(name.clone()) {
                return Err(invalid("duplicate ZIP entry name"));
            }
            let extra = take(&directory, extra_at, extra_len)?;
            let raw_size = u64::from(n32(header, 24)?);
            let raw_compressed = u64::from(n32(header, 20)?);
            let raw_offset = u64::from(n32(header, 42)?);
            let resolved = resolve_extra(extra, raw_size, raw_compressed, Some(raw_offset))?;
            uses_zip64 |= !extra.is_empty();
            let len = resolved.length;
            let local_offset = resolved.offset.ok_or_else(|| invalid("missing ZIP local offset"))?;
            if len > if name == EntryName::Manifest { MAX_MANIFEST } else { MAX_LARGE_ASSET_BYTES } {
                return Err(invalid("ZIP entry exceeds content limit"));
            }
            if matches!(name, EntryName::Asset(_)) {
                total = add(total, len)?;
            }
            if total > MAX_LARGE_TOTAL_ASSET_BYTES {
                return Err(invalid("ZIP assets exceed 64 GiB total limit"));
            }
            let mut local = [0u8; 30];
            source.read_exact_at(local_offset, &mut local, metrics)?;
            let local_extra_len = usize::from(n16(&local, 28)?);
            if n32(&local, 0)? != 0x04034b50
                || n16(&local, 6)? != n16(header, 8)?
                || n16(&local, 8)? != 0
                || n32(&local, 14)? != n32(header, 16)?
                || usize::from(n16(&local, 26)?) != name_len
                || local_extra_len > 20
            {
                return Err(invalid("inconsistent local ZIP header"));
            }
            let local_name_at = add(local_offset, 30)?;
            let mut local_name = [0u8; 71];
            source.read_exact_at(local_name_at, local_name.get_mut(..name_len).ok_or_else(|| invalid("invalid local ZIP name"))?, metrics)?;
            if local_name.get(..name_len) != Some(name_bytes) {
                return Err(invalid("local and directory ZIP names disagree"));
            }
            let local_extra_at = add(local_name_at, name_len as u64)?;
            let mut local_extra = [0u8; 20];
            let local_extra = local_extra.get_mut(..local_extra_len).ok_or_else(|| invalid("invalid local ZIP extra length"))?;
            source.read_exact_at(local_extra_at, local_extra, metrics)?;
            let local_resolved = resolve_extra(local_extra, u64::from(n32(&local, 22)?), u64::from(n32(&local, 18)?), None)?;
            if local_resolved.length != len {
                return Err(invalid("local and directory ZIP sizes disagree"));
            }
            uses_zip64 |= !local_extra.is_empty();
            let data_at = add(local_extra_at, local_extra_len as u64)?;
            if add(data_at, len)? > footer.directory_offset {
                return Err(invalid("ZIP entry overlaps its directory"));
            }
            entries.push(IndexedEntry {
                name,
                data: source.slice(data_at, len)?,
                crc32: n32(header, 16)?,
                local_offset,
                data_offset: data_at,
                flags: n16(header, 8)?,
            });
            cursor = extra_at.checked_add(extra_len).ok_or_else(|| invalid("ZIP metadata extent overflow"))?;
        }
        if cursor != directory.len() {
            return Err(invalid("hidden ZIP directory data"));
        }
        entries.sort_by_key(|entry| entry.local_offset);
        let mut previous_end = 0u64;
        for entry in &entries {
            if entry.local_offset != previous_end {
                return Err(invalid("overlapping or hidden local ZIP data"));
            }
            previous_end = add(entry.data_offset, entry.data.byte_len())?;
        }
        if previous_end != footer.directory_offset {
            return Err(invalid("hidden data before ZIP directory"));
        }
        let manifest_entry = entries.iter().find(|entry| entry.name == EntryName::Manifest).ok_or_else(|| invalid("missing project manifest"))?;
        let manifest = manifest_entry.data.read_small(MAX_MANIFEST as usize, metrics)?;
        if crc32fast::hash(&manifest) != manifest_entry.crc32 {
            return Err(invalid("project manifest CRC mismatch"));
        }
        #[derive(Deserialize)]
        struct Version {
            version: u32,
        }
        let version: Version = serde_json::from_slice(&manifest).map_err(|_| invalid("invalid project manifest or missing format version"))?;
        match version.version {
            1 => {
                if uses_zip64
                    || source.byte_len() > crate::MAX_ARCHIVE_BYTES as u64
                    || total > crate::MAX_TOTAL_ASSET_BYTES as u64
                    || entries.iter().any(|e| matches!(e.name, EntryName::Asset(_)) && e.data.byte_len() > crate::MAX_ASSET_BYTES as u64)
                {
                    return Err(invalid("format v1 requires legacy classic ZIP size limits"));
                }
            }
            2 => {}
            _ => return Err(Error::new(ErrorKind::Unsupported, "unsupported project format version; update NodeCraft or use a compatible exporter")),
        }
        cancel.check()?;
        Ok(Self {
            identity: IndexIdentity {
                archive_bytes: source.byte_len(),
                directory_offset: footer.directory_offset,
                version: version.version,
                manifest,
                entries,
            },
        })
    }
}
struct Footer {
    count: u64,
    directory_bytes: u64,
    directory_offset: u64,
    end: u64,
    zip64: bool,
}
impl Footer {
    fn read(source: &AssetRange, metrics: &StreamMetrics) -> Result<Self, Error> {
        let end = source.byte_len().checked_sub(22).ok_or_else(|| invalid("truncated ZIP footer"))?;
        let mut classic = [0u8; 22];
        source.read_exact_at(end, &mut classic, metrics)?;
        if n32(&classic, 0)? != 0x06054b50
            || n16(&classic, 4)? != 0
            || n16(&classic, 6)? != 0
            || n16(&classic, 20)? != 0
            || n16(&classic, 8)? != n16(&classic, 10)?
        {
            return Err(invalid("unsupported ZIP footer or archive comment"));
        }
        let count = u64::from(n16(&classic, 10)?);
        let size = u64::from(n32(&classic, 12)?);
        let offset = u64::from(n32(&classic, 16)?);
        let sentinel = count == u16::MAX as u64 || size == U32_SENTINEL || offset == U32_SENTINEL;
        if !sentinel {
            return Ok(Self { count, directory_bytes: size, directory_offset: offset, end, zip64: false });
        }
        let locator_at = end.checked_sub(20).ok_or_else(|| invalid("truncated ZIP64 locator"))?;
        let mut locator = [0u8; 20];
        source.read_exact_at(locator_at, &mut locator, metrics)?;
        if n32(&locator, 0)? != 0x07064b50 || n32(&locator, 4)? != 0 || n32(&locator, 16)? != 1 {
            return Err(invalid("invalid ZIP64 locator or split archive"));
        }
        let record_at = n64(&locator, 8)?;
        if add(record_at, 56)? != locator_at {
            return Err(invalid("invalid ZIP64 footer extent"));
        }
        let mut record = [0u8; 56];
        source.read_exact_at(record_at, &mut record, metrics)?;
        if n32(&record, 0)? != 0x06064b50
            || n64(&record, 4)? != 44
            || n32(&record, 16)? != 0
            || n32(&record, 20)? != 0
            || n64(&record, 24)? != n64(&record, 32)?
        {
            return Err(invalid("unsupported ZIP64 footer"));
        }
        let real_count = n64(&record, 32)?;
        let real_size = n64(&record, 40)?;
        let real_offset = n64(&record, 48)?;
        if (count != u16::MAX as u64 && count != real_count) || (size != U32_SENTINEL && size != real_size) || (offset != U32_SENTINEL && offset != real_offset)
        {
            return Err(invalid("classic and ZIP64 footer fields disagree"));
        }
        Ok(Self { count: real_count, directory_bytes: real_size, directory_offset: real_offset, end: record_at, zip64: true })
    }
}
struct Resolved {
    length: u64,
    offset: Option<u64>,
}
fn resolve_extra(extra: &[u8], size: u64, compressed: u64, offset: Option<u64>) -> Result<Resolved, Error> {
    let wide_size = size == U32_SENTINEL;
    let wide_compressed = compressed == U32_SENTINEL;
    let wide_offset = offset == Some(U32_SENTINEL);
    let fields = usize::from(wide_size) + usize::from(wide_compressed) + usize::from(wide_offset);
    if fields == 0 {
        if !extra.is_empty() || size != compressed {
            return Err(invalid("unexpected ZIP extra data or compressed entry"));
        }
        return Ok(Resolved { length: size, offset });
    }
    // Stored files use the same size; the pinned writer deliberately carries both sizes.
    if wide_size != wide_compressed || extra.len() != 4 + fields * 8 || n16(extra, 0)? != 1 || usize::from(n16(extra, 2)?) != fields * 8 {
        return Err(invalid("ZIP64 extra must contain exactly the sentinel fields in order"));
    }
    let mut at = 4;
    let size = if wide_size {
        let n = n64(extra, at)?;
        at += 8;
        n
    } else {
        size
    };
    let compressed = if wide_compressed {
        let n = n64(extra, at)?;
        at += 8;
        n
    } else {
        compressed
    };
    let offset = if wide_offset { Some(n64(extra, at)?) } else { offset };
    if size != compressed {
        return Err(invalid("stored ZIP sizes disagree"));
    }
    Ok(Resolved { length: size, offset })
}
fn parse_name(name: &[u8]) -> Result<EntryName, Error> {
    if name == b"project.json" {
        return Ok(EntryName::Manifest);
    }
    if name.len() != 71
        || !name.starts_with(b"assets/")
        || !name.get(7..).is_some_and(|tail| tail.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)))
    {
        return Err(invalid("unexpected ZIP path; only project.json and assets/<blake3> are allowed"));
    }
    let hash = std::str::from_utf8(name.get(7..).ok_or_else(|| invalid("invalid asset name"))?).map_err(|_| invalid("invalid asset name"))?.parse()?;
    Ok(EntryName::Asset(hash))
}
fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b).ok_or_else(|| invalid("ZIP extent overflow"))
}
fn take(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], Error> {
    bytes.get(at..at.checked_add(len).ok_or_else(|| invalid("ZIP extent overflow"))?).ok_or_else(|| invalid("truncated ZIP metadata"))
}
fn n16(bytes: &[u8], at: usize) -> Result<u16, Error> {
    Ok(u16::from_le_bytes(take(bytes, at, 2)?.try_into().map_err(|_| invalid("invalid ZIP integer"))?))
}
fn n32(bytes: &[u8], at: usize) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(take(bytes, at, 4)?.try_into().map_err(|_| invalid("invalid ZIP integer"))?))
}
fn n64(bytes: &[u8], at: usize) -> Result<u64, Error> {
    Ok(u64::from_le_bytes(take(bytes, at, 8)?.try_into().map_err(|_| invalid("invalid ZIP integer"))?))
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
