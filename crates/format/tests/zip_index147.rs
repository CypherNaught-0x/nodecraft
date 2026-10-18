//! Stored ZIP range-index contract tests.
use nodecraft_format::asset_source::{AssetRange, AssetSource, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_format::zip_index::{ArchiveIndex, EntryName};
use nodecraft_types::{Error, ErrorKind};
use std::{
    io::{Cursor, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

fn encoded(version: u32, forced: bool, payload: &[u8]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    zip.start_file("project.json", options).unwrap();
    write!(zip, "{{\"version\":{version}}}").unwrap();
    let hash = blake3::hash(payload).to_hex();
    zip.start_file(format!("assets/{hash}"), options.large_file(forced)).unwrap();
    zip.write_all(payload).unwrap();
    zip.finish().unwrap().into_inner()
}
fn range(bytes: Vec<u8>) -> AssetRange {
    let length = bytes.len() as u64;
    AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, length).unwrap()
}
struct Meter {
    bytes: Vec<u8>,
    body: std::ops::Range<u64>,
    body_reads: AtomicU64,
}
impl AssetSource for Meter {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_at(&self, offset: u64, out: &mut [u8]) -> Result<usize, Error> {
        let start = usize::try_from(offset).map_err(|_| Error::new(ErrorKind::InvalidInput, "bad test offset"))?;
        let source = self.bytes.get(start..).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "bad test range"))?;
        let n = source.len().min(out.len());
        if offset < self.body.end && offset + n as u64 > self.body.start {
            self.body_reads.fetch_add(1, Ordering::Relaxed);
        }
        out[..n].copy_from_slice(&source[..n]);
        Ok(n)
    }
}
#[test]
fn actual_pinned_writer_classic_and_forced_zip64_are_indexed_without_asset_reads() {
    for forced in [false, true] {
        let bytes = encoded(2, forced, &vec![53; 1024 * 1024]);
        let mut zip = ZipArchive::new(Cursor::new(&bytes)).unwrap();
        let file = zip.by_index(1).unwrap();
        let start = file.data_start().unwrap();
        let length = file.size();
        let source = Arc::new(Meter { bytes: bytes.clone(), body: start..start + length, body_reads: AtomicU64::new(0) });
        let input = AssetRange::new(source.clone(), 0, source.byte_len()).unwrap();
        let metrics = StreamMetrics::default();
        let index = ArchiveIndex::read(&input, &TransferCancel::default(), &metrics).unwrap();
        assert_eq!(index.version(), 2);
        assert_eq!(index.manifest(), b"{\"version\":2}");
        assert_eq!(index.entries().len(), 2);
        let asset = index.entries().iter().find(|e| matches!(e.name, EntryName::Asset(_))).unwrap();
        assert_eq!(asset.data_offset, start);
        assert_eq!(asset.data.byte_len(), length);
        assert_eq!(source.body_reads.load(Ordering::Relaxed), 0);
        assert!(metrics.stats().source_bytes < 4096);
        let repeated = ArchiveIndex::read(&input, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert_eq!(index.identity(), repeated.identity());
    }
}
#[test]
fn legacy_constraints_and_raw_version_gate_remain_explicit() {
    let fixture = include_bytes!("fixtures/v1/comments-v1.ncraft").to_vec();
    assert_eq!(ArchiveIndex::read(&range(fixture), &TransferCancel::default(), &StreamMetrics::default()).unwrap().version(), 1);
    assert!(ArchiveIndex::read(&range(encoded(1, true, b"x")), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    let error = ArchiveIndex::read(&range(encoded(999, false, b"x")), &TransferCancel::default(), &StreamMetrics::default()).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Unsupported);
}
#[test]
fn hostile_metadata_and_manifest_crc_refuse_before_any_asset_verification() {
    let original = encoded(2, false, b"payload");
    let index = ArchiveIndex::read(&range(original.clone()), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let body = index.entries().iter().find(|e| matches!(e.name, EntryName::Asset(_))).unwrap().data_offset as usize;
    let mut payload_corrupt = original.clone();
    payload_corrupt[body] ^= 1;
    // Indexing is not content-address admission: a payload error is intentionally deferred.
    assert!(ArchiveIndex::read(&range(payload_corrupt), &TransferCancel::default(), &StreamMetrics::default()).is_ok());
    let manifest = index.entries().iter().find(|e| e.name == EntryName::Manifest).unwrap().data_offset as usize;
    let mut wrong_manifest = original.clone();
    wrong_manifest[manifest] ^= 1;
    assert!(ArchiveIndex::read(&range(wrong_manifest), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    let mut prefixed = vec![0];
    prefixed.extend_from_slice(&original);
    assert!(ArchiveIndex::read(&range(prefixed), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    for truncate in [1, 21, original.len() - 1] {
        assert!(ArchiveIndex::read(&range(original[..truncate].to_vec()), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    }
    for (relative, replacement) in [(8, 1u8), (10, 8), (34, 1), (30, 1)] {
        let mut bad = original.clone();
        let central = index.identity().directory_offset as usize;
        bad[central + relative] = replacement;
        assert!(ArchiveIndex::read(&range(bad), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    }
    let mut bad_name = original.clone();
    bad_name[30] = b'/';
    assert!(ArchiveIndex::read(&range(bad_name), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    let cancel = TransferCancel::default();
    cancel.cancel();
    assert!(ArchiveIndex::read(&range(original), &cancel, &StreamMetrics::default()).is_err());
}

// A virtual seekable file with >4 GiB logical body. These are parser/addressing tests,
// not a claim that multi-GiB physical output was written or read.
struct VirtualZip {
    len: u64,
    segments: Vec<(u64, Vec<u8>)>,
    body: std::ops::Range<u64>,
    body_reads: AtomicU64,
}
impl AssetSource for VirtualZip {
    fn byte_len(&self) -> u64 {
        self.len
    }
    fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
        let n = out.len().min(usize::try_from(self.len.saturating_sub(at)).unwrap_or(usize::MAX));
        out[..n].fill(0);
        if at < self.body.end && at + n as u64 > self.body.start {
            self.body_reads.fetch_add(1, Ordering::Relaxed);
        }
        for (start, bytes) in &self.segments {
            let from = at.max(*start);
            let to = (at + n as u64).min(*start + bytes.len() as u64);
            if from < to {
                out[(from - at) as usize..(to - at) as usize].copy_from_slice(&bytes[(from - start) as usize..(to - start) as usize]);
            }
        }
        Ok(n)
    }
}
fn u16w(v: &mut Vec<u8>, n: u16) {
    v.extend_from_slice(&n.to_le_bytes());
}
fn u32w(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_le_bytes());
}
fn u64w(v: &mut Vec<u8>, n: u64) {
    v.extend_from_slice(&n.to_le_bytes());
}
fn local(name: &[u8], len: u64, crc: u32, wide: bool) -> Vec<u8> {
    let mut v = Vec::new();
    u32w(&mut v, 0x04034b50);
    u16w(&mut v, if wide { 45 } else { 20 });
    for n in [0, 0, 0, 0] {
        u16w(&mut v, n);
    }
    u32w(&mut v, crc);
    for _ in 0..2 {
        u32w(&mut v, if wide { u32::MAX } else { len as u32 });
    }
    u16w(&mut v, name.len() as u16);
    u16w(&mut v, if wide { 20 } else { 0 });
    v.extend_from_slice(name);
    if wide {
        u16w(&mut v, 1);
        u16w(&mut v, 16);
        u64w(&mut v, len);
        u64w(&mut v, len);
    }
    v
}
fn central(name: &[u8], len: u64, offset: u64, crc: u32, wide: bool) -> Vec<u8> {
    let wide_offset = offset >= u32::MAX as u64;
    let extra_len = if wide { 20 } else { 0 } + if wide_offset { if wide { 8 } else { 12 } } else { 0 };
    let mut v = Vec::new();
    u32w(&mut v, 0x02014b50);
    u16w(&mut v, 45);
    u16w(&mut v, if wide || wide_offset { 45 } else { 20 });
    for n in [0, 0, 0, 0] {
        u16w(&mut v, n);
    }
    u32w(&mut v, crc);
    for _ in 0..2 {
        u32w(&mut v, if wide { u32::MAX } else { len as u32 });
    }
    u16w(&mut v, name.len() as u16);
    u16w(&mut v, extra_len);
    for _ in 0..3 {
        u16w(&mut v, 0);
    }
    u32w(&mut v, 0);
    u32w(&mut v, if wide_offset { u32::MAX } else { offset as u32 });
    v.extend_from_slice(name);
    if extra_len != 0 {
        u16w(&mut v, 1);
        u16w(&mut v, extra_len - 4);
        if wide {
            u64w(&mut v, len);
            u64w(&mut v, len);
        }
        if wide_offset {
            u64w(&mut v, offset);
        }
    }
    v
}
fn virtual_large() -> VirtualZip {
    virtual_with_length(u32::MAX as u64 + 1)
}
fn virtual_with_length(length: u64) -> VirtualZip {
    let manifest = b"{\"version\":2}";
    let name = format!("assets/{}", "0".repeat(64));
    let mut beginning = local(b"project.json", manifest.len() as u64, crc32fast::hash(manifest), false);
    beginning.extend_from_slice(manifest);
    let offset = beginning.len() as u64;
    let wide = length >= u32::MAX as u64;
    beginning.extend(local(name.as_bytes(), length, 0, wide));
    let body_at = beginning.len() as u64;
    let body_end = body_at + length;
    let last_name = format!("assets/{}", "1".repeat(64));
    let mut suffix = local(last_name.as_bytes(), 1, crc32fast::hash(b"Z"), false);
    suffix.push(b'Z');
    let directory_at = body_end + suffix.len() as u64;
    let mut tail = central(b"project.json", manifest.len() as u64, 0, crc32fast::hash(manifest), false);
    tail.extend(central(name.as_bytes(), length, offset, 0, wide));
    tail.extend(central(last_name.as_bytes(), 1, body_end, crc32fast::hash(b"Z"), false));
    let size = tail.len() as u64;
    let record_at = directory_at + size;
    u32w(&mut tail, 0x06064b50);
    u64w(&mut tail, 44);
    u16w(&mut tail, 45);
    u16w(&mut tail, 45);
    u32w(&mut tail, 0);
    u32w(&mut tail, 0);
    u64w(&mut tail, 3);
    u64w(&mut tail, 3);
    u64w(&mut tail, size);
    u64w(&mut tail, directory_at);
    u32w(&mut tail, 0x07064b50);
    u32w(&mut tail, 0);
    u64w(&mut tail, record_at);
    u32w(&mut tail, 1);
    u32w(&mut tail, 0x06054b50);
    u16w(&mut tail, 0);
    u16w(&mut tail, 0);
    u16w(&mut tail, 3);
    u16w(&mut tail, 3);
    u32w(&mut tail, u32::MAX);
    u32w(&mut tail, u32::MAX);
    u16w(&mut tail, 0);
    VirtualZip {
        len: directory_at + tail.len() as u64,
        segments: vec![(0, beginning), (body_end, suffix), (directory_at, tail)],
        body: body_at..body_end,
        body_reads: AtomicU64::new(0),
    }
}
#[test]
fn zip64_u32_boundary_uses_checked_offsets_and_only_metadata_reads() {
    let source = Arc::new(virtual_large());
    let input = AssetRange::new(source.clone(), 0, source.len).unwrap();
    let metrics = StreamMetrics::default();
    let index = ArchiveIndex::read(&input, &TransferCancel::default(), &metrics).unwrap();
    assert!(index.identity().directory_offset > u32::MAX as u64);
    assert_eq!(index.entries().len(), 3);
    let last = index.entries().last().unwrap();
    assert!(last.local_offset > u32::MAX as u64);
    assert_eq!(last.data.byte_len(), 1);
    assert_eq!(index.entries().iter().find(|e| matches!(e.name, EntryName::Asset(_))).unwrap().data.byte_len(), u32::MAX as u64 + 1);
    assert_eq!(source.body_reads.load(Ordering::Relaxed), 0);
    assert!(metrics.stats().source_bytes < 4096);
    // Locator's record offset must be exact; overflow/truncated/extended layouts refuse.
    let mut bad = virtual_large();
    let tail = &mut bad.segments.last_mut().unwrap().1;
    let locator = tail.len() - 42;
    tail[locator + 8..locator + 16].copy_from_slice(&u64::MAX.to_le_bytes());
    let bad = Arc::new(bad);
    let input = AssetRange::new(bad.clone(), 0, bad.len).unwrap();
    assert!(ArchiveIndex::read(&input, &TransferCancel::default(), &StreamMetrics::default()).is_err());
}

#[test]
fn zip64_threshold_sentinels_and_extra_fields_are_not_truncated_or_guessed() {
    for length in [u32::MAX as u64 - 1, u32::MAX as u64, u32::MAX as u64 + 1] {
        let source = Arc::new(virtual_with_length(length));
        let input = AssetRange::new(source.clone(), 0, source.len).unwrap();
        let index = ArchiveIndex::read(&input, &TransferCancel::default(), &StreamMetrics::default()).unwrap();
        assert_eq!(index.entries().get(1).unwrap().data.byte_len(), length);
        assert_eq!(source.body_reads.load(Ordering::Relaxed), 0);
    }
    let original = encoded(2, true, b"fixture");
    let index = ArchiveIndex::read(&range(original.clone()), &TransferCancel::default(), &StreamMetrics::default()).unwrap();
    let central_asset = index.identity().directory_offset as usize + 46 + b"project.json".len();
    let extra = central_asset + 46 + 71;
    for (at, value) in [(extra, 2u8), (extra + 2, 8), (central_asset + 42, 0xff)] {
        let mut bad = original.clone();
        bad[at] = value;
        assert!(ArchiveIndex::read(&range(bad), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    }
    let asset = index.entries().get(1).unwrap();
    let mut bad_crc = original.clone();
    bad_crc[asset.local_offset as usize + 14] ^= 1;
    assert!(ArchiveIndex::read(&range(bad_crc), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    let mut symlink = original.clone();
    symlink[central_asset + 38..central_asset + 42].copy_from_slice(&0xa0000000u32.to_le_bytes());
    assert!(ArchiveIndex::read(&range(symlink), &TransferCancel::default(), &StreamMetrics::default()).is_err());
    let mut duplicate = virtual_large();
    let duplicate_name = format!("assets/{}", "0".repeat(64));
    duplicate.segments.get_mut(1).unwrap().1[30..101].copy_from_slice(duplicate_name.as_bytes());
    let third_header = 46 + b"project.json".len() + 46 + 71 + 20;
    duplicate.segments.last_mut().unwrap().1[third_header + 46..third_header + 46 + 71].copy_from_slice(duplicate_name.as_bytes());
    let duplicate = Arc::new(duplicate);
    let input = AssetRange::new(duplicate.clone(), 0, duplicate.len).unwrap();
    assert!(ArchiveIndex::read(&input, &TransferCancel::default(), &StreamMetrics::default()).is_err());
}
