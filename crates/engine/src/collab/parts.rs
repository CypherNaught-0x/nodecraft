//! Assets too large for one frame (#700): they come in parts, a window at a time, into a
//! [`Spool`], and go out read from the session's store a part at a time.
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use nodecraft_format::asset_source::{AssetRange, AssetSource, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_sync::{Frame, MAX_ASSET_BYTES, MAX_SHARED_ASSET_BYTES, PART_BYTES, WINDOW_BYTES};
use nodecraft_types::{Blake3, Error, ErrorKind};

use crate::session::Session;

/// Where the parts of an asset are kept until it is whole. A host that can write files keeps
/// them on disk; the default keeps them in memory, and so takes only small ones.
pub trait Spool: Send {
    /// The largest asset this spool takes.
    fn limit(&self) -> u64;
    /// The most it holds at once, of all the assets coming in and waiting to be taken.
    fn budget(&self) -> u64;
    /// Starts keeping an asset of `total` bytes.
    fn begin(&mut self, hash: Blake3, total: u64) -> Result<(), Error>;
    /// Adds the next part of an asset begun.
    fn append(&mut self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error>;
    /// The asset whole, to be imported; the spool lets go of it.
    fn finish(&mut self, hash: &Blake3) -> Result<AssetRange, Error>;
    /// Forgets an asset begun.
    fn discard(&mut self, hash: &Blake3);
}

/// A spool in memory, for assets up to `limit` bytes.
#[derive(Debug)]
pub struct MemorySpool {
    limit: u64,
    kept: BTreeMap<Blake3, Vec<u8>>,
}

impl MemorySpool {
    pub fn new(limit: u64) -> Self {
        Self { limit: limit.min(MAX_SHARED_ASSET_BYTES), kept: BTreeMap::new() }
    }
}

impl Default for MemorySpool {
    /// As large as an asset sent whole: the most a host without files imports.
    fn default() -> Self {
        Self::new(MAX_ASSET_BYTES as u64)
    }
}

impl Spool for MemorySpool {
    fn limit(&self) -> u64 {
        self.limit
    }

    fn budget(&self) -> u64 {
        super::MAX_ARRIVED_BYTES as u64
    }

    fn begin(&mut self, hash: Blake3, total: u64) -> Result<(), Error> {
        let total = usize::try_from(total).ok().filter(|total| *total as u64 <= self.limit).ok_or_else(too_large)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(total).map_err(|_| too_large())?;
        self.kept.insert(hash, bytes);
        Ok(())
    }

    fn append(&mut self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
        let kept = self.kept.get_mut(hash).ok_or_else(not_begun)?;
        if (kept.len() as u64).saturating_add(bytes.len() as u64) > self.limit {
            return Err(too_large());
        }
        kept.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self, hash: &Blake3) -> Result<AssetRange, Error> {
        let bytes: Arc<[u8]> = self.kept.remove(hash).ok_or_else(not_begun)?.into();
        let len = bytes.len() as u64;
        let source: Arc<dyn AssetSource> = Arc::new(MemorySource::new(bytes));
        AssetRange::new(source, 0, len)
    }

    fn discard(&mut self, hash: &Blake3) {
        self.kept.remove(hash);
    }
}

fn too_large() -> Error {
    Error::new(ErrorKind::Unsupported, "this asset is too large to take here")
}

fn not_begun() -> Error {
    Error::new(ErrorKind::Internal, "an asset's parts came before it was begun")
}

/// An asset that arrived before the edit that needs it.
#[derive(Debug)]
pub enum Arrival {
    /// Sent whole; its hash is checked when it is taken.
    Bytes(Vec<u8>),
    /// Put together from parts, whose hash was checked as the last arrived.
    Range(AssetRange),
}

impl Arrival {
    pub(super) fn byte_len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Range(range) => range.byte_len(),
        }
    }

    /// Imports it into `session`, which checks the bytes are what `hash` names.
    pub(super) fn import(&self, hash: &Blake3, session: &mut Session) -> Result<(), Error> {
        let made = match self {
            Self::Bytes(bytes) => session.import_asset(bytes)?,
            Self::Range(range) => session.import_asset_from(range, &TransferCancel::default(), &StreamMetrics::default())?,
        };
        if made != *hash {
            return Err(Error::new(ErrorKind::Internal, "an asset that arrived isn't the one it was checked as"));
        }
        Ok(())
    }
}

/// An asset coming in parts.
pub(super) struct Incoming {
    pub(super) total: u64,
    pub(super) received: u64,
    /// The end of what has been asked for.
    pub(super) asked_end: u64,
    hasher: blake3::Hasher,
}

impl Incoming {
    pub(super) fn new(total: u64) -> Self {
        Self { total, received: 0, asked_end: WINDOW_BYTES.min(total), hasher: blake3::Hasher::new() }
    }

    /// Counts the next part in.
    pub(super) fn take(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.received = self.received.saturating_add(bytes.len() as u64);
    }

    pub(super) fn whole(&self) -> bool {
        self.received >= self.total
    }

    /// Whether what came in is what `hash` names.
    pub(super) fn checks(&self, hash: &Blake3) -> bool {
        self.hasher.finalize().as_bytes() == hash.as_bytes()
    }

    /// The next stretch to ask for, once less than half a window is on its way.
    pub(super) fn more(&mut self, hash: &Blake3) -> Option<Frame> {
        let ahead = self.asked_end.saturating_sub(self.received);
        if ahead > WINDOW_BYTES / 2 || self.asked_end >= self.total {
            return None;
        }
        let len = (WINDOW_BYTES / 2).min(self.total.saturating_sub(self.asked_end));
        let ask = Frame::AssetFrom { hash: *hash.as_bytes(), offset: self.asked_end, len };
        self.asked_end = self.asked_end.saturating_add(len);
        Some(ask)
    }

    /// Asks for the rest again, on a new connection.
    pub(super) fn resume(&mut self, hash: &Blake3) -> Frame {
        let len = WINDOW_BYTES.min(self.total.saturating_sub(self.received)).max(1);
        self.asked_end = self.received.saturating_add(len);
        Frame::AssetFrom { hash: *hash.as_bytes(), offset: self.received, len }
    }
}

/// How much of an asset another asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stretch {
    /// All of a small one; the first window of a large one.
    Whole,
    /// `len` bytes from `offset`.
    From { offset: u64, len: u64 },
}

/// The frames that answer an ask for `stretch` of the asset `hash`: the asset whole when it
/// fits in one frame and was asked for whole, else its parts. None if the session can't read it.
pub(super) fn frames(session: &Session, hash: &Blake3, stretch: Stretch) -> Option<Vec<Frame>> {
    let total = session.assets().descriptor(hash)?.bytes;
    if total > MAX_SHARED_ASSET_BYTES {
        return None;
    }
    let cancel = TransferCancel::default();
    let metrics = StreamMetrics::default();
    let (offset, len) = match stretch {
        Stretch::Whole if total <= MAX_ASSET_BYTES as u64 => {
            let bytes = match session.assets().get(hash) {
                Some(bytes) => bytes.to_vec(),
                None => session.assets().verified_content(hash, &cancel, &metrics).ok()?.read_small(MAX_ASSET_BYTES, &metrics).ok()?,
            };
            return Some(vec![Frame::AssetPut { hash: *hash.as_bytes(), bytes }]);
        }
        Stretch::Whole => (0, WINDOW_BYTES),
        Stretch::From { offset, len } => (offset, len.min(WINDOW_BYTES)),
    };
    let end = offset.saturating_add(len).min(total);
    if offset >= end {
        return None;
    }
    let asset = session.assets().verified_content(hash, &cancel, &metrics).ok()?;
    let mut reader = asset.reader(metrics);
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut out = Vec::new();
    let mut at = offset;
    while at < end {
        let n = usize::try_from(end.saturating_sub(at)).unwrap_or(PART_BYTES).min(PART_BYTES);
        let mut bytes = vec![0; n];
        reader.read_exact(&mut bytes).ok()?;
        out.push(Frame::AssetPart { hash: *hash.as_bytes(), total, offset: at, bytes });
        at = at.saturating_add(n as u64);
    }
    Some(out)
}
