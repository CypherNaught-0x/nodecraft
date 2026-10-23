//! Where the desktop keeps an asset coming in parts (#700): a file of its own in the system's
//! temporary folder, gone once the asset has been imported or given up. Each file is locked
//! while its spool has it, so a later spool clears away the files of one that ended without
//! cleaning up (#842).
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nodecraft_engine::collab::Spool;
use nodecraft_format::asset_source::{AssetRange, AssetSource};
use nodecraft_sync::MAX_SHARED_ASSET_BYTES;
use nodecraft_types::{Blake3, Error, ErrorKind};

/// The most the spool keeps on disk at once.
const BUDGET_BYTES: u64 = 16 << 30;

/// Room left free for everything else on the disk.
const RESERVE_BYTES: u64 = 1 << 30;

/// How the spool's files are named: `nodecraft-share-<process>-<n>.part`.
const PREFIX: &str = "nodecraft-share-";
const SUFFIX: &str = ".part";

/// A file left this long unchanged is old enough to clear, if no spool holds it; a younger one
/// may be another run's, just made and not yet locked.
const STALE: Duration = Duration::from_secs(60);

/// The most files of the temporary folder one sweep looks at.
const MAX_SWEPT: usize = 100_000;

/// Tells apart the files of spools in one run.
static NEXT: AtomicU64 = AtomicU64::new(0);

/// A spool of temporary files.
pub(super) struct FileSpool {
    dir: PathBuf,
    open: BTreeMap<Blake3, Part>,
    /// The bytes in this spool's files, open or finished and not yet let go.
    written: Arc<AtomicU64>,
}

/// A file being written.
struct Part {
    file: File,
    path: Temporary,
}

/// A temporary file, deleted when dropped.
struct Temporary {
    path: PathBuf,
    bytes: u64,
    written: Arc<AtomicU64>,
}

impl Temporary {
    fn grew(&mut self, n: u64) {
        self.bytes = self.bytes.saturating_add(n);
        // The bytes of files on one disk: far from overflowing.
        self.written.fetch_add(n, Ordering::Relaxed);
    }
}

impl Drop for Temporary {
    fn drop(&mut self) {
        // Already gone is as good.
        let _ = std::fs::remove_file(&self.path);
        // Takes back exactly what this file added.
        self.written.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

impl FileSpool {
    /// A spool in the system's temporary folder, once what spools of earlier runs left there is
    /// cleared away. It reads the folder, so it is made away from the window.
    pub(super) fn new() -> Self {
        Self::in_dir(std::env::temp_dir())
    }

    fn in_dir(dir: PathBuf) -> Self {
        sweep(&dir, STALE);
        Self { dir, open: BTreeMap::new(), written: Arc::new(AtomicU64::new(0)) }
    }
}

/// Deletes the spool files in `dir` that no spool holds and that haven't changed for `stale`.
fn sweep(dir: &Path, stale: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.take(MAX_SWEPT).flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) {
            continue;
        }
        let path = entry.path();
        let old = entry.metadata().and_then(|meta| meta.modified()).ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age >= stale);
        if !old {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(&path) else { continue };
        // Locked, it is a running spool's.
        if file.try_lock().is_ok() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

impl Spool for FileSpool {
    fn limit(&self) -> u64 {
        MAX_SHARED_ASSET_BYTES
    }

    /// The disk's free room counts too: an asset that came is copied again into the project's
    /// storage, so the spool takes at most half of what is free beyond a reserve.
    fn budget(&self) -> u64 {
        let written = self.written.load(Ordering::Relaxed);
        match fs4::available_space(&self.dir) {
            Ok(free) => BUDGET_BYTES.min((free.saturating_sub(RESERVE_BYTES) / 2).saturating_add(written)),
            Err(_) => BUDGET_BYTES,
        }
    }

    fn begin(&mut self, hash: Blake3, _total: u64) -> Result<(), Error> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = self.dir.join(format!("{PREFIX}{}-{n}{SUFFIX}", std::process::id()));
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(&path).map_err(|_| io("can't make a file for an asset coming in"))?;
        let path = Temporary { path, bytes: 0, written: Arc::clone(&self.written) };
        // Held as long as the file is open, so no other spool's sweep deletes it.
        file.try_lock().map_err(|_| io("can't lock a file for an asset coming in"))?;
        self.open.insert(hash, Part { file, path });
        Ok(())
    }

    fn append(&mut self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
        let part = self.open.get_mut(hash).ok_or_else(|| Error::new(ErrorKind::Internal, "an asset's parts came before it was begun"))?;
        part.file.write_all(bytes).map_err(|_| io("can't write an asset coming in"))?;
        part.path.grew(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
        Ok(())
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

    /// A spool in a folder of its own, so tests don't see each other's files.
    fn spool() -> (FileSpool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (FileSpool::in_dir(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn an_asset_is_kept_in_a_file_until_it_is_let_go() {
        let (mut spool, _dir) = spool();
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
        let (mut spool, _dir) = spool();
        let hash = Blake3::from_bytes([8; 32]);
        spool.begin(hash, 3).unwrap();
        let path = spool.open.get(&hash).unwrap().path.path.clone();
        assert!(path.exists());
        spool.discard(&hash);
        assert!(!path.exists());
        assert!(spool.append(&hash, b"x").is_err());
    }

    #[test]
    fn what_an_ended_run_left_is_cleared_and_a_running_spools_files_are_kept() {
        let (mut spool, dir) = spool();
        let hash = Blake3::from_bytes([9; 32]);
        spool.begin(hash, 3).unwrap();
        spool.append(&hash, b"abc").unwrap();
        let held = spool.open.get(&hash).unwrap().path.path.clone();
        let left = dir.path().join(format!("{PREFIX}1-1{SUFFIX}"));
        std::fs::write(&left, b"left behind").unwrap();
        let other = dir.path().join("someone-else.part");
        std::fs::write(&other, b"not ours").unwrap();
        sweep(dir.path(), Duration::ZERO);
        assert!(!left.exists());
        assert!(held.exists());
        assert!(other.exists());
        // A file just made may be another run's, not yet locked.
        std::fs::write(&left, b"just made").unwrap();
        sweep(dir.path(), STALE);
        assert!(left.exists());
    }

    #[test]
    fn the_budget_counts_what_the_spool_holds_and_never_passes_its_cap() {
        let (mut spool, _dir) = spool();
        assert!(spool.budget() <= BUDGET_BYTES);
        let hash = Blake3::from_bytes([10; 32]);
        spool.begin(hash, 4).unwrap();
        spool.append(&hash, b"abcd").unwrap();
        assert_eq!(spool.written.load(Ordering::Relaxed), 4);
        let range = spool.finish(&hash).unwrap();
        assert_eq!(spool.written.load(Ordering::Relaxed), 4);
        drop(range);
        assert_eq!(spool.written.load(Ordering::Relaxed), 0);
    }
}
