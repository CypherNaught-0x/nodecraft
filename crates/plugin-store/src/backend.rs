//! Store backends: in memory (tests, and the browser build, where packages last for the session),
//! and a folder on disk (native).
use crate::store::StoreBackend;
#[cfg(not(target_arch = "wasm32"))]
use crate::store::StoreLock;
use nodecraft_plugin::PackageDigest;
use nodecraft_types::{Error, ErrorKind};
use std::collections::BTreeMap;

/// Keeps everything in memory; nothing survives the session.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    pub index: Option<Vec<u8>>,
    pub objects: BTreeMap<PackageDigest, Vec<u8>>,
    /// Fails the next index write (tests: a full disk).
    pub fail_next_write: bool,
}

impl StoreBackend for MemoryBackend {
    fn read_index(&self) -> Result<Option<Vec<u8>>, Error> {
        Ok(self.index.clone())
    }
    fn write_index(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if std::mem::take(&mut self.fail_next_write) {
            return Err(Error::new(ErrorKind::Internal, "the plugin store couldn't be written (disk full)"));
        }
        self.index = Some(bytes.to_vec());
        Ok(())
    }
    fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, Error> {
        self.objects.get(digest).cloned().ok_or_else(|| Error::new(ErrorKind::NotFound, "a stored package is missing; import it again"))
    }
    fn write_object(&mut self, digest: &PackageDigest, bytes: &[u8]) -> Result<(), Error> {
        self.objects.insert(*digest, bytes.to_vec());
        Ok(())
    }
    fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), Error> {
        self.objects.remove(digest);
        Ok(())
    }
    fn list_objects(&self) -> Result<Vec<PackageDigest>, Error> {
        Ok(self.objects.keys().copied().collect())
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::FolderBackend;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// A folder holding `index.json` and `objects/<digest>.ncraft-plugin`. Files are written
    /// aside, flushed and renamed over, so a crash or a full disk leaves the old file whole.
    #[derive(Debug)]
    pub struct FolderBackend {
        root: PathBuf,
    }

    fn io(detail: &'static str) -> impl Fn(std::io::Error) -> Error {
        move |_| Error::new(ErrorKind::Internal, detail)
    }

    /// How old a stored file must be before an unindexed one is swept.
    const SETTLED: std::time::Duration = std::time::Duration::from_secs(10 * 60);

    /// Largest stored file read back (an archive is at most 32 MiB; the index 4 MiB).
    const MAX_READ: u64 = 64 * 1024 * 1024;

    impl FolderBackend {
        pub fn new(root: PathBuf) -> Result<Self, Error> {
            std::fs::create_dir_all(root.join("objects")).map_err(io("the plugin store folder can't be created"))?;
            Ok(Self { root })
        }

        fn sweep_temporary_indexes(&self) {
            // Index writes a crash interrupted.
            if let Ok(entries) = std::fs::read_dir(&self.root) {
                for entry in entries.flatten().take(1024) {
                    let settled = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= SETTLED);
                    if settled && entry.file_name().to_str().is_some_and(|n| n.starts_with("index.json.") && n.ends_with(".new")) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }

        fn object(&self, digest: &PackageDigest) -> PathBuf {
            let name = digest.to_string().replace(':', "-");
            self.root.join("objects").join(format!("{name}.ncraft-plugin"))
        }

        fn replace(path: &Path, bytes: &[u8]) -> Result<(), Error> {
            // Unique per process and write: two writers never share (and rename) one half-written
            // file. A failed write removes its file; `open` sweeps any left by a crash.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut aside = path.as_os_str().to_owned();
            aside.push(format!(".{}-{n}.new", std::process::id()));
            let aside = PathBuf::from(aside);
            let written = std::fs::File::create(&aside)
                .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
                .and_then(|()| std::fs::rename(&aside, path));
            if written.is_err() {
                let _ = std::fs::remove_file(&aside);
            }
            written.map_err(io("the plugin store couldn't be written (is the disk full?)"))?;
            // The rename itself is durable once the folder is synced (not possible on Windows).
            if let Some(folder) = path.parent()
                && let Ok(dir) = std::fs::File::open(folder)
            {
                let _ = dir.sync_all();
            }
            Ok(())
        }

        fn read(path: &Path) -> Result<Option<Vec<u8>>, Error> {
            use std::io::Read;
            let file = match std::fs::File::open(path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(Error::new(ErrorKind::Internal, "the plugin store can't be read")),
            };
            let mut bytes = Vec::new();
            file.take(MAX_READ + 1).read_to_end(&mut bytes).map_err(io("the plugin store can't be read"))?;
            if bytes.len() as u64 > MAX_READ {
                return Err(Error::new(ErrorKind::InvalidInput, "a plugin store file is too large"));
            }
            Ok(Some(bytes))
        }
    }

    impl StoreBackend for FolderBackend {
        fn transaction(&self) -> Result<Box<dyn StoreLock>, Error> {
            // Never unlink or replace this inode. Separate opens also exclude other threads
            // in this process; a process-global gate would unnecessarily block other stores.
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.root.join(".lock"))
                .map_err(io("the plugin store lock can't be opened"))?;
            match file.try_lock() {
                Ok(()) => {}
                Err(std::fs::TryLockError::WouldBlock) => return Err(Error::new(ErrorKind::Busy, "the plugin store is busy in another request; try again")),
                Err(_) => return Err(Error::new(ErrorKind::Unsupported, "the plugin store filesystem doesn't support transaction locking")),
            }
            Ok(Box::new(file))
        }
        fn read_index(&self) -> Result<Option<Vec<u8>>, Error> {
            Self::read(&self.root.join("index.json"))
        }
        fn write_index(&mut self, bytes: &[u8]) -> Result<(), Error> {
            Self::replace(&self.root.join("index.json"), bytes)
        }
        fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, Error> {
            Self::read(&self.object(digest))?.ok_or_else(|| Error::new(ErrorKind::NotFound, "a stored package is missing; import it again"))
        }
        fn write_object(&mut self, digest: &PackageDigest, bytes: &[u8]) -> Result<(), Error> {
            Self::replace(&self.object(digest), bytes)
        }
        fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), Error> {
            match std::fs::remove_file(self.object(digest)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::new(ErrorKind::Internal, "a stored package couldn't be removed")),
                _ => Ok(()),
            }
        }
        fn list_objects(&self) -> Result<Vec<PackageDigest>, Error> {
            // Store::open calls this only after decoding the current index, under its guard.
            // A refused stale mutation acquires the guard but performs no maintenance sweep.
            self.sweep_temporary_indexes();
            let entries = std::fs::read_dir(self.root.join("objects")).map_err(io("the plugin store can't be read"))?;
            let mut digests = Vec::new();
            for entry in entries.flatten().take(4 * crate::store::MAX_OBJECTS) {
                // Another window or the CLI may be importing right now: its archive is written
                // before its index. Only files settled for a while are candidates.
                let settled = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age >= SETTLED);
                if !settled {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.ends_with(".new") {
                    // Left by a write a crash interrupted.
                    let _ = std::fs::remove_file(entry.path());
                } else if let Some(digest) = name.strip_suffix(".ncraft-plugin").and_then(|n| PackageDigest::parse(&n.replacen('-', ":", 1)).ok()) {
                    digests.push(digest);
                }
            }
            Ok(digests)
        }
    }
}
