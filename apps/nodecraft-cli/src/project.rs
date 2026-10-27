//! Opening a `.ncraft` project headlessly, read-only.
//!
//! The CLI decodes the file in place rather than opening it through the app's storage: storage
//! takes the per-user project lock and keeps a recovery journal, so it would refuse a project the
//! app has open and would write into the app's data folder. Decoding reads the file and writes
//! nothing anywhere.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use nodecraft_engine::Session;
use nodecraft_format::asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel};
use nodecraft_format::zip_index::ArchiveIndex;
use nodecraft_format::{AssetContext, ProjectStorage};
use nodecraft_types::{Error, ErrorKind};

/// The project file as a bounded source; reads seek a shared handle.
struct FileSource {
    file: Mutex<File>,
    len: u64,
}

impl AssetSource for FileSource {
    fn byte_len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        let unreadable = || Error::new(ErrorKind::Internal, "can't read the project file");
        let mut file = self.file.lock().map_err(|_| unreadable())?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| unreadable())?;
        file.read(buffer).map_err(|_| unreadable())
    }
}

/// Opens the project at `path` read-only: nothing is written, and the app's lock is not taken.
pub fn open(path: &Path) -> Result<Session, Error> {
    Session::new(read(path)?.project)
}

/// [`open`] with the project's assets, for runs and exports. Its storage is in memory, so
/// nothing the session does is written to the project file.
pub fn open_with_assets(path: &Path) -> Result<Session, Error> {
    Session::with_storage_origin(read(path)?, Box::new(nodecraft_format::MemoryStorage::default()), origin(path)?)
}

pub(crate) fn origin(path: &Path) -> Result<nodecraft_format::StorageLocation, Error> {
    let path = path.canonicalize().map_err(|_| Error::new(ErrorKind::NotFound, "cannot resolve the source project identity"))?;
    Ok(nodecraft_format::StorageLocation::Path(path))
}

/// The project at `path`, decoded: its document and assets, read-only.
pub fn snapshot(path: &Path) -> Result<nodecraft_format::Snapshot, Error> {
    read(path)
}

fn read(path: &Path) -> Result<nodecraft_format::Snapshot, Error> {
    snapshot_in_context(path, AssetContext::default())
}

/// A run/import owns private, quota-accounted streaming storage beside its output.
pub fn native_context(parent: &Path) -> Result<AssetContext, Error> {
    let workspace = nodecraft_format::native_assets::NativeAssetWorkspace::new(parent, nodecraft_format::disk_quota::DiskQuota::default())?;
    Ok(AssetContext::native(workspace))
}

/// A private native journal permits streaming edits; it is discarded on a normal CLI exit.
/// Its exact temporary path is owned independently of asset/reader leases. Cleanup never
/// recursively removes the parent or the asset workspace, which can outlive the session.
pub fn ephemeral(parent: &Path) -> Result<Box<dyn nodecraft_format::ProjectStorage>, Error> {
    let recovery = tempfile::Builder::new()
        .prefix(".nodecraft-cli-")
        .suffix(".recovery")
        .tempfile_in(parent)
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "the run folder is not writable"))?
        .into_temp_path();
    let mut lock = recovery.as_os_str().to_os_string();
    lock.push(".lock");
    let storage = nodecraft_format::NativeStorage::new(recovery.to_path_buf())?;
    Ok(Box::new(Ephemeral { storage: Some(storage), recovery: Some(recovery), lock: lock.into() }))
}
struct Ephemeral {
    storage: Option<nodecraft_format::NativeStorage>,
    recovery: Option<tempfile::TempPath>,
    lock: std::path::PathBuf,
}
impl Ephemeral {
    fn storage(&mut self) -> Result<&mut nodecraft_format::NativeStorage, Error> {
        self.storage.as_mut().ok_or_else(|| Error::new(ErrorKind::Internal, "CLI storage is closed"))
    }
}
impl nodecraft_format::ProjectStorage for Ephemeral {
    fn capability(&self) -> nodecraft_format::StorageCapability {
        nodecraft_format::StorageCapability::StreamingV2
    }
    fn asset_context(&self) -> Option<AssetContext> {
        self.storage.as_ref().and_then(|s| s.asset_context())
    }
    fn checkpoint_plan(&mut self, plan: &nodecraft_format::CheckpointPlan, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        self.storage()?.checkpoint_plan(plan, cancel, metrics)
    }
    fn append_plan(&mut self, plan: &nodecraft_format::FramePlan, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<(), Error> {
        self.storage()?.append_plan(plan, cancel, metrics)
    }
    fn load(&mut self, _: &nodecraft_format::StorageLocation) -> Result<Vec<u8>, Error> {
        Err(ephemeral_only())
    }
    fn save(&mut self, _: &nodecraft_format::StorageLocation, _: &[u8]) -> Result<(), Error> {
        Err(ephemeral_only())
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.storage()?.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.storage()?.append(bytes)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.storage()?.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        self.storage()?.discard()
    }
}
impl Drop for Ephemeral {
    fn drop(&mut self) {
        if let Some(mut storage) = self.storage.take() {
            if storage.discard().is_err() {
                // Retain a failed cleanup for inspection; never silently delete a journal
                // whose owner still reports retained bytes or an unsettled write.
                if let Some(path) = self.recovery.take() {
                    let _ = path.keep();
                }
                eprintln!("warning: CLI recovery cleanup failed; its private journal was retained");
                return;
            }
            drop(storage);
            if std::fs::remove_file(&self.lock).is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound) {
                eprintln!("warning: the CLI recovery lock could not be removed");
            }
        }
    }
}
fn ephemeral_only() -> Error {
    Error::new(ErrorKind::Unsupported, "CLI source projects are read-only; use an explicit output file")
}

pub fn snapshot_in_context(path: &Path, context: AssetContext) -> Result<nodecraft_format::Snapshot, Error> {
    // Checked first, so a typo reads as one rather than as a format error.
    if !path.is_file() {
        return Err(Error::new(ErrorKind::NotFound, format!("no project file at {}", path.display())));
    }
    let file = File::open(path).map_err(|e| Error::new(ErrorKind::NotFound, format!("can't open {}: {e}", path.display())))?;
    let len = file.metadata().map_err(|e| Error::new(ErrorKind::Internal, format!("can't read {}: {e}", path.display())))?.len();
    if !context.supports_large_assets() {
        // The frozen byte reader verifies and retains bounded resident assets. Indexed assets
        // require a native private-verification host, even when the individual bodies are small.
        let cap = nodecraft_format::MAX_ARCHIVE_BYTES;
        if len > cap as u64 {
            return Err(Error::new(ErrorKind::Unsupported, "this read-only command requires a bounded project archive"));
        }
        let mut bytes = Vec::new();
        file.take((cap as u64).saturating_add(1)).read_to_end(&mut bytes).map_err(|_| Error::new(ErrorKind::Internal, "can't read the project file"))?;
        if bytes.len() > cap {
            return Err(Error::new(ErrorKind::Overflow, "project exceeds the read-only archive limit"));
        }
        return nodecraft_format::decode(&bytes);
    }
    let source: Arc<dyn AssetSource> = Arc::new(FileSource { file: Mutex::new(file), len });
    let range = AssetRange::new(source, 0, len)?;
    let index = ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default())?;
    nodecraft_format::decode_indexed(&index, context)
}

#[cfg(test)]
#[path = "project/identity879_tests.rs"]
mod identity879_tests;
