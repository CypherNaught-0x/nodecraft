//! Bounded, platform-neutral sources. A range describes bytes; it is NOT a content-verification proof.
use nodecraft_types::{Blake3, Error, ErrorKind};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(any(test, feature = "validation-metrics"))]
use std::sync::atomic::AtomicU64;

/// Seekable bounded archive output; native atomic stages implement this through safe std I/O.
pub trait WriteSeek: std::io::Write + std::io::Seek {}
impl<T: std::io::Write + std::io::Seek + ?Sized> WriteSeek for T {}

pub const TRANSFER_BYTES: usize = 64 * 1024;
pub const MAX_LARGE_ASSET_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_LARGE_TOTAL_ASSET_BYTES: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_LARGE_ARCHIVE_BYTES: u64 = 65 * 1024 * 1024 * 1024;
pub const MAX_SMALL_READ_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_STREAM_EXTENT_BYTES: u64 = 132 * 1024 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
pub trait SourceSafety: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> SourceSafety for T {}
#[cfg(target_arch = "wasm32")]
pub trait SourceSafety {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> SourceSafety for T {}

/// Trusted Rust I/O boundary, not authority to associate bytes with a content hash.
/// Callers must not assume File::try_clone creates an independent seek position.
pub trait AssetSource: SourceSafety {
    fn byte_len(&self) -> u64;
    /// Return at most buffer.len() bytes, zero only at EOF. Never include paths/data in errors.
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error>;
}

#[derive(Clone)]
pub struct MemorySource(Arc<[u8]>);
impl MemorySource {
    pub fn new(bytes: Arc<[u8]>) -> Self {
        Self(bytes)
    }
}
impl AssetSource for MemorySource {
    fn byte_len(&self) -> u64 {
        self.0.len() as u64
    }
    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        let at = usize::try_from(offset).map_err(|_| invalid("asset offset cannot be represented"))?;
        let remaining = self.0.get(at..).ok_or_else(|| invalid("asset offset is outside its source"))?;
        let n = remaining.len().min(buffer.len());
        buffer
            .get_mut(..n)
            .ok_or_else(|| invalid("asset read buffer is too short"))?
            .copy_from_slice(remaining.get(..n).ok_or_else(|| invalid("asset source is truncated"))?);
        Ok(n)
    }
}

/// A structurally admitted extent. Content hash/CRC/metadata remain unverified.
#[derive(Clone)]
pub struct AssetRange {
    source: Arc<dyn AssetSource>,
    start: u64,
    length: u64,
}
impl std::fmt::Debug for AssetRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetRange").field("length", &self.length).finish_non_exhaustive()
    }
}
impl AssetRange {
    pub fn new(source: Arc<dyn AssetSource>, start: u64, length: u64) -> Result<Self, Error> {
        if start.checked_add(length).is_none_or(|end| end > source.byte_len()) {
            return Err(invalid("asset range is outside its source"));
        }
        Ok(Self { source, start, length })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn same_backing(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.source, &other.source)
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn backing_references(&self) -> usize {
        Arc::strong_count(&self.source)
    }
    pub fn byte_len(&self) -> u64 {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    pub fn slice(&self, start: u64, length: u64) -> Result<Self, Error> {
        if start.checked_add(length).is_none_or(|end| end > self.length) {
            return Err(invalid("asset subrange is outside its parent"));
        }
        Self::new(Arc::clone(&self.source), self.start.checked_add(start).ok_or_else(|| invalid("asset range overflow"))?, length)
    }
    pub fn reader(&self, metrics: StreamMetrics) -> AssetReader {
        AssetReader { range: self.clone(), at: 0, metrics }
    }
    pub fn read_exact_at(&self, at: u64, bytes: &mut [u8], metrics: &StreamMetrics) -> Result<(), Error> {
        let end = at.checked_add(bytes.len() as u64).ok_or_else(|| invalid("asset read extent overflow"))?;
        if end > self.length {
            return Err(invalid("asset read is outside its range"));
        }
        let mut reader = self.reader(metrics.clone());
        reader.at = at;
        reader.read_exact(bytes).map_err(|_| io_error("asset source is truncated or unreadable"))
    }
    /// Explicit bounded convenience; this is not the large-source path.
    pub fn read_small(&self, cap: usize, metrics: &StreamMetrics) -> Result<Vec<u8>, Error> {
        if cap > MAX_SMALL_READ_BYTES || self.length > cap as u64 {
            return Err(Error::new(ErrorKind::Unsupported, "asset needs a streaming reader; small materialization limit exceeded"));
        }
        let n = usize::try_from(self.length).map_err(|_| invalid("asset length cannot be represented"))?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(n).map_err(|_| io_error("cannot allocate bounded asset bytes"))?;
        bytes.resize(n, 0);
        self.read_exact_at(0, &mut bytes, metrics)?;
        Ok(bytes)
    }
}

pub struct AssetReader {
    range: AssetRange,
    at: u64,
    metrics: StreamMetrics,
}
impl Read for AssetReader {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.range.length.checked_sub(self.at).ok_or_else(|| std::io::Error::other("invalid asset cursor"))?;
        let n = bytes.len().min(TRANSFER_BYTES).min(usize::try_from(remaining).unwrap_or(usize::MAX));
        if n == 0 {
            return Ok(0);
        }
        let offset = self.range.start.checked_add(self.at).ok_or_else(|| std::io::Error::other("asset cursor overflow"))?;
        let buffer = bytes.get_mut(..n).ok_or_else(|| std::io::Error::other("invalid asset read buffer"))?;
        let actual = self.range.source.read_at(offset, buffer).map_err(|_| std::io::Error::other("asset source read failed"))?;
        if actual == 0 || actual > n {
            return Err(std::io::Error::other("asset source returned an invalid read extent"));
        }
        self.at = self.at.checked_add(actual as u64).ok_or_else(|| std::io::Error::other("asset cursor overflow"))?;
        self.metrics.read(actual);
        Ok(actual)
    }
}
impl Seek for AssetReader {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let next = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(n) => add_signed(self.at, n),
            SeekFrom::End(n) => add_signed(self.range.length, n),
        }
        .filter(|n| *n <= self.range.length)
        .ok_or_else(|| std::io::Error::other("asset seek is outside its range"))?;
        self.at = next;
        self.metrics.seek();
        Ok(next)
    }
}
fn add_signed(base: u64, n: i64) -> Option<u64> {
    if n < 0 { base.checked_sub(n.unsigned_abs()) } else { base.checked_add(n as u64) }
}

const CANCELLED: &str = "asset transfer was cancelled before publication";

#[derive(Clone, Default)]
pub struct TransferCancel(Arc<AtomicBool>);
impl TransferCancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn check(&self) -> Result<(), Error> {
        if self.0.load(Ordering::Acquire) { Err(Error::new(ErrorKind::Conflict, CANCELLED)) } else { Ok(()) }
    }
    /// Whether `error` is the one [`Self::check`] returns, as opposed to a real failure that
    /// happened to arrive after a cancel was asked for.
    pub fn is_cancellation(error: &Error) -> bool {
        error.kind == ErrorKind::Conflict && error.message == CANCELLED
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub source_bytes: u64,
    pub maximum_read: u64,
    pub seeks: u64,
    pub live_transfer_bytes: u64,
    pub peak_transfer_bytes: u64,
}
#[cfg(any(test, feature = "validation-metrics"))]
#[derive(Default)]
struct Counters {
    reads: AtomicU64,
    maximum: AtomicU64,
    seeks: AtomicU64,
    live: AtomicU64,
    peak: AtomicU64,
}
/// Deterministic operation counters, disabled in normal builds.
#[derive(Clone, Default)]
pub struct StreamMetrics {
    #[cfg(any(test, feature = "validation-metrics"))]
    inner: Arc<Counters>,
}
impl StreamMetrics {
    pub fn stats(&self) -> StreamStats {
        #[cfg(any(test, feature = "validation-metrics"))]
        {
            StreamStats {
                source_bytes: self.inner.reads.load(Ordering::Relaxed),
                maximum_read: self.inner.maximum.load(Ordering::Relaxed),
                seeks: self.inner.seeks.load(Ordering::Relaxed),
                live_transfer_bytes: self.inner.live.load(Ordering::Relaxed),
                peak_transfer_bytes: self.inner.peak.load(Ordering::Relaxed),
            }
        }
        #[cfg(not(any(test, feature = "validation-metrics")))]
        {
            StreamStats::default()
        }
    }
    pub(crate) fn read(&self, _bytes: usize) {
        #[cfg(any(test, feature = "validation-metrics"))]
        {
            let _ = self.inner.reads.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(_bytes as u64)));
            self.inner.maximum.fetch_max(_bytes as u64, Ordering::Relaxed);
        }
    }
    fn seek(&self) {
        #[cfg(any(test, feature = "validation-metrics"))]
        {
            let _ = self.inner.seeks.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(1)));
        }
    }
    fn acquire(&self) {
        #[cfg(any(test, feature = "validation-metrics"))]
        {
            let live = match self.inner.live.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_add(TRANSFER_BYTES as u64))) {
                Ok(n) => n.saturating_add(TRANSFER_BYTES as u64),
                Err(n) => n,
            };
            self.inner.peak.fetch_max(live, Ordering::Relaxed);
        }
    }
    fn release(&self) {
        #[cfg(any(test, feature = "validation-metrics"))]
        {
            let _ = self.inner.live.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(n.saturating_sub(TRANSFER_BYTES as u64)));
        }
    }
}
pub(crate) struct TransferBuffer {
    bytes: Vec<u8>,
    metrics: StreamMetrics,
}
impl TransferBuffer {
    pub(crate) fn new(metrics: &StreamMetrics) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(TRANSFER_BYTES).map_err(|_| io_error("cannot allocate asset transfer buffer"))?;
        bytes.resize(TRANSFER_BYTES, 0);
        metrics.acquire();
        Ok(Self { bytes, metrics: metrics.clone() })
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}
impl Drop for TransferBuffer {
    fn drop(&mut self) {
        self.metrics.release();
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamDigest {
    pub hash: Blake3,
    pub crc32: u32,
    pub bytes: u64,
}
/// Copy one exact, already bounded extent. The destination is still staged/unpublished.
pub fn copy_and_hash(range: &AssetRange, output: &mut dyn Write, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<StreamDigest, Error> {
    cancel.check()?;
    if range.byte_len() > MAX_STREAM_EXTENT_BYTES {
        return Err(invalid("asset transfer exceeds its operation limit"));
    }
    let mut buffer = TransferBuffer::new(metrics)?;
    let mut reader = range.reader(metrics.clone());
    let mut digest = blake3::Hasher::new();
    let mut crc = crc32fast::Hasher::new();
    let mut length = 0u64;
    loop {
        cancel.check()?;
        let n = reader.read(&mut buffer.bytes).map_err(|_| io_error("cannot read complete asset transfer"))?;
        if n == 0 {
            break;
        }
        let bytes = buffer.bytes.get(..n).ok_or_else(|| invalid("invalid asset transfer extent"))?;
        output.write_all(bytes).map_err(|_| io_error("cannot write complete asset transfer"))?;
        digest.update(bytes);
        crc.update(bytes);
        length = length.checked_add(n as u64).ok_or_else(|| invalid("asset transfer length overflow"))?;
    }
    cancel.check()?;
    if length != range.byte_len() {
        return Err(invalid("asset transfer length changed"));
    }
    Ok(StreamDigest { hash: Blake3::from_bytes(*digest.finalize().as_bytes()), crc32: crc.finalize(), bytes: length })
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn io_error(message: &'static str) -> Error {
    Error::new(ErrorKind::Conflict, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Virtual {
        len: u64,
        largest: AtomicU64,
        reads: AtomicU64,
    }
    impl AssetSource for Virtual {
        fn byte_len(&self) -> u64 {
            self.len
        }
        fn read_at(&self, at: u64, out: &mut [u8]) -> Result<usize, Error> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.largest.fetch_max(out.len() as u64, Ordering::Relaxed);
            let n = out.len().min(usize::try_from(self.len.saturating_sub(at)).unwrap_or(usize::MAX));
            out[..n].fill(42);
            Ok(n)
        }
    }
    #[test]
    fn indexing_ranges_and_independent_cursors_do_not_load_large_bodies() {
        let source = Arc::new(Virtual { len: u64::from(u32::MAX) + 100, largest: AtomicU64::new(0), reads: AtomicU64::new(0) });
        let range = AssetRange::new(source.clone(), 0, source.len).unwrap();
        let metrics = StreamMetrics::default();
        let mut a = range.reader(metrics.clone());
        let mut b = range.reader(metrics.clone());
        assert_eq!(source.reads.load(Ordering::Relaxed), 0);
        a.seek(SeekFrom::End(-1)).unwrap();
        let mut one = [0];
        a.read_exact(&mut one).unwrap();
        b.read_exact(&mut one).unwrap();
        assert_eq!(a.stream_position().unwrap(), source.len);
        assert_eq!(b.stream_position().unwrap(), 1);
        assert!(a.seek(SeekFrom::Current(i64::MIN)).is_err());
        assert!(range.slice(u64::MAX, 2).is_err());
        assert!(range.read_small(MAX_SMALL_READ_BYTES, &metrics).is_err());
        assert_eq!(source.reads.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn many_mebibytes_stream_through_one_bounded_buffer() {
        let source = Arc::new(Virtual { len: 20 * 1024 * 1024 + 1, largest: AtomicU64::new(0), reads: AtomicU64::new(0) });
        let range = AssetRange::new(source.clone(), 0, source.len).unwrap();
        let metrics = StreamMetrics::default();
        let digest = copy_and_hash(&range, &mut std::io::sink(), &TransferCancel::default(), &metrics).unwrap();
        assert_eq!(digest.bytes, source.len);
        assert!(source.largest.load(Ordering::Relaxed) <= TRANSFER_BYTES as u64);
        assert_eq!(metrics.stats().source_bytes, source.len);
        assert_eq!(metrics.stats().peak_transfer_bytes, TRANSFER_BYTES as u64);
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
    }
    #[test]
    fn short_source_write_failure_and_cancel_release_the_buffer() {
        struct Short;
        impl AssetSource for Short {
            fn byte_len(&self) -> u64 {
                100
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, Error> {
                Ok(0)
            }
        }
        let range = AssetRange::new(Arc::new(Short), 0, 100).unwrap();
        let metrics = StreamMetrics::default();
        assert!(copy_and_hash(&range, &mut std::io::sink(), &TransferCancel::default(), &metrics).is_err());
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
        let cancel = TransferCancel::default();
        cancel.cancel();
        assert!(copy_and_hash(&range, &mut std::io::sink(), &cancel, &metrics).is_err());
        assert_eq!(metrics.stats().source_bytes, 0);
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("full"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(b"hello".as_slice()))), 0, 5).unwrap();
        assert!(copy_and_hash(&range, &mut Full, &TransferCancel::default(), &metrics).is_err());
        assert_eq!(metrics.stats().live_transfer_bytes, 0);
    }
}
