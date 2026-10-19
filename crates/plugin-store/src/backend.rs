//! Store backends: in memory (tests, and the browser build, where packages last for the session),
//! and a folder on disk (native).
use crate::store::StoreBackend;
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

    /// Largest stored file read back (an archive is at most 32 MiB; the index 4 MiB).
    const MAX_READ: u64 = 64 * 1024 * 1024;

    impl FolderBackend {
        pub fn new(root: PathBuf) -> Result<Self, Error> {
            std::fs::create_dir_all(root.join("objects")).map_err(io("the plugin store folder can't be created"))?;
            Ok(Self { root })
        }

        fn object(&self, digest: &PackageDigest) -> PathBuf {
            let name = digest.to_string().replace(':', "-");
            self.root.join("objects").join(format!("{name}.ncraft-plugin"))
        }

        fn replace(path: &Path, bytes: &[u8]) -> Result<(), Error> {
            let aside = path.with_extension("new");
            let mut file = std::fs::File::create(&aside).map_err(io("the plugin store couldn't be written"))?;
            file.write_all(bytes).and_then(|()| file.sync_all()).map_err(io("the plugin store couldn't be written (is the disk full?)"))?;
            drop(file);
            std::fs::rename(&aside, path).map_err(io("the plugin store couldn't be written"))?;
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
    }
}
