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
use nodecraft_format::AssetContext;
use nodecraft_format::asset_source::{AssetRange, AssetSource, StreamMetrics, TransferCancel};
use nodecraft_format::zip_index::ArchiveIndex;
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
    // Checked first, so a typo reads as one rather than as a format error.
    if !path.is_file() {
        return Err(Error::new(ErrorKind::NotFound, format!("no project file at {}", path.display())));
    }
    let file = File::open(path).map_err(|e| Error::new(ErrorKind::NotFound, format!("can't open {}: {e}", path.display())))?;
    let len = file.metadata().map_err(|e| Error::new(ErrorKind::Internal, format!("can't read {}: {e}", path.display())))?.len();
    let source: Arc<dyn AssetSource> = Arc::new(FileSource { file: Mutex::new(file), len });
    let range = AssetRange::new(source, 0, len)?;
    let index = ArchiveIndex::read(&range, &TransferCancel::default(), &StreamMetrics::default())?;
    nodecraft_format::decode_indexed(&index, AssetContext::default())
}

#[cfg(test)]
#[path = "project/identity879_tests.rs"]
mod identity879_tests;
