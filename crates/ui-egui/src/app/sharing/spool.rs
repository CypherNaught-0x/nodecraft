//! Where the desktop keeps an asset coming in parts (#700): a file of its own in the system's
//! temporary folder, gone once the asset has been imported or given up.
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use nodecraft_engine::collab::Spool;
use nodecraft_format::asset_source::{AssetRange, AssetSource};
use nodecraft_sync::MAX_SHARED_ASSET_BYTES;
use nodecraft_types::{Blake3, Error, ErrorKind};

/// The most the spool keeps on disk at once.
const BUDGET_BYTES: u64 = 16 << 30;

/// Tells apart the files of spools in one run.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A spool of temporary files.
#[derive(Default)]
pub(super) struct FileSpool {
    open: BTreeMap<Blake3, Part>,
}

/// A file being written.
struct Part {
    file: File,
    path: Temporary,
}

/// A temporary file, deleted when dropped.
struct Temporary(PathBuf);

impl Drop for Temporary {
    fn drop(&mut self) {
        // Already gone is as good.
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Spool for FileSpool {
    fn limit(&self) -> u64 {
        MAX_SHARED_ASSET_BYTES
    }

    fn budget(&self) -> u64 {
        BUDGET_BYTES
    }

    fn begin(&mut self, hash: Blake3, _total: u64) -> Result<(), Error> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nodecraft-share-{}-{n}.part", std::process::id()));
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).map_err(|_| io("can't make a file for an asset coming in"))?;
        self.open.insert(hash, Part { file, path: Temporary(path) });
        Ok(())
    }

    fn append(&mut self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
        let part = self.open.get_mut(hash).ok_or_else(|| Error::new(ErrorKind::Internal, "an asset's parts came before it was begun"))?;
        part.file.write_all(bytes).map_err(|_| io("can't write an asset coming in"))
    }

    fn finish(&mut self, hash: &Blake3) -> Result<AssetRange, Error> {
        let Part { mut file, path } = self.open.remove(hash).ok_or_else(|| Error::new(ErrorKind::Internal, "an asset finished before it was begun"))?;
        file.flush().map_err(|_| io("can't write an asset coming in"))?;
        let len = file.metadata().map_err(|_| io("can't read an asset that came in"))?.len();
        let source: Arc<dyn AssetSource> = Arc::new(FileSource { file: Mutex::new(file), len, _path: path });
        AssetRange::new(source, 0, len)
    }

    fn discard(&mut self, hash: &Blake3) {
        self.open.remove(hash);
    }
}

/// A whole asset in its temporary file, until it is imported.
struct FileSource {
    file: Mutex<File>,
    len: u64,
    _path: Temporary,
}

impl AssetSource for FileSource {
    fn byte_len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        let mut file = self.file.lock().map_err(|_| io("an asset that came in can't be read"))?;
        file.seek(SeekFrom::Start(offset)).map_err(|_| io("can't read an asset that came in"))?;
        file.read(buffer).map_err(|_| io("can't read an asset that came in"))
    }
}

fn io(message: &str) -> Error {
    Error::new(ErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_asset_is_kept_in_a_file_until_it_is_let_go() {
        let mut spool = FileSpool::default();
        let hash = Blake3::from_bytes([7; 32]);
        spool.begin(hash, 6).unwrap();
        spool.append(&hash, b"abc").unwrap();
        spool.append(&hash, b"def").unwrap();
        let range = spool.finish(&hash).unwrap();
        assert_eq!(range.read_small(16, &Default::default()).unwrap(), b"abcdef");
        assert!(spool.open.is_empty());
    }

    #[test]
    fn a_discarded_asset_leaves_no_file() {
        let mut spool = FileSpool::default();
        let hash = Blake3::from_bytes([8; 32]);
        spool.begin(hash, 3).unwrap();
        let path = spool.open.get(&hash).unwrap().path.0.clone();
        assert!(path.exists());
        spool.discard(&hash);
        assert!(!path.exists());
        assert!(spool.append(&hash, b"x").is_err());
    }
}
