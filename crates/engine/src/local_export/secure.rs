//! Unix folder-grant implementation: never resolve an output through a mutable pathname.
//! All directory traversal and publication use descriptors held under the originally opened root.
use nodecraft_format::AssetStore;
use nodecraft_types::{AssetRef, Error, ErrorKind};
use rustix::fs::{self, AtFlags, Mode, OFlags};
use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Component, Path};
use std::sync::Arc;

const CHANGED: &str = "the export folder changed during export; choose the folder again";
const LINK: &str = "an export folder or file name is a symbolic link; choose another name";
/// The mode a new export file is created with. `openat` applies the process umask to it, so a
/// batch file gets the same permissions `history.export` gives a file.
const PUBLIC_MODE: Mode = Mode::from_bits_truncate(0o666);

fn error(error: rustix::io::Errno) -> Error {
    if error == rustix::io::Errno::NOSYS || error == rustix::io::Errno::OPNOTSUPP {
        return Error::new(ErrorKind::Unsupported, "this filesystem lacks secure descriptor-relative export operations; choose another folder");
    }
    crate::export::io_error(std::io::Error::from(error).kind())
}

fn safe_component(dir: impl AsFd, name: &std::ffi::OsStr) -> Result<OwnedFd, Error> {
    // Stat without following links for an actionable refusal, then open with O_NOFOLLOW
    // independently: an attacker changing the component between these calls cannot win.
    let stat = fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW).map_err(error)?;
    if fs::FileType::from_raw_mode(stat.st_mode).is_symlink() {
        return Err(Error::new(ErrorKind::InvalidInput, LINK));
    }
    fs::openat(&dir, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()).map_err(|err| {
        if err == rustix::io::Errno::NOSYS || err == rustix::io::Errno::OPNOTSUPP { error(err) } else { Error::new(ErrorKind::InvalidInput, CHANGED) }
    })
}

/// Open a canonical absolute root once. The host keeps this descriptor for the entire grant.
/// Canonicalization of an app-picked root is intentional; its *descendants* never follow links.
pub(super) fn open_root(root: &Path) -> Result<Arc<File>, Error> {
    let mut current = fs::open("/", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).map_err(error)?;
    for part in root.components() {
        match part {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => current = safe_component(&current, name)?,
            _ => return Err(Error::new(ErrorKind::InvalidInput, CHANGED)),
        }
    }
    Ok(Arc::new(File::from(current)))
}

fn relative(root: &File, remainder: &Path) -> Result<File, Error> {
    let mut current = fs::openat(root, ".", OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()).map_err(error)?;
    for part in remainder.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => current = safe_component(&current, name)?,
            _ => return Err(Error::new(ErrorKind::InvalidInput, CHANGED)),
        }
    }
    Ok(File::from(current))
}

/// Pinned destination; compare its inode to the folder reachable from the granted root
/// immediately before each publication. A replaced component is never traversed as a link.
pub(super) struct Destination<'a> {
    root: &'a File,
    root_path: &'a Path,
    remainder: &'a Path,
    parent: File,
}

impl<'a> Destination<'a> {
    pub(super) fn open(root: &'a File, root_path: &'a Path, remainder: &'a Path) -> Result<Self, Error> {
        let parent = relative(root, remainder)?;
        Ok(Self { root, root_path, remainder, parent })
    }

    fn verify(&self) -> Result<(), Error> {
        let root_now = open_root(self.root_path)?;
        let granted = fs::fstat(self.root).map_err(error)?;
        let live = fs::fstat(root_now.as_ref()).map_err(error)?;
        if granted.st_dev != live.st_dev || granted.st_ino != live.st_ino {
            return Err(Error::new(ErrorKind::InvalidInput, CHANGED));
        }
        let reachable = relative(self.root, self.remainder)?;
        let expected = fs::fstat(&self.parent).map_err(error)?;
        let actual = fs::fstat(&reachable).map_err(error)?;
        if expected.st_dev != actual.st_dev || expected.st_ino != actual.st_ino {
            return Err(Error::new(ErrorKind::InvalidInput, CHANGED));
        }
        Ok(())
    }

    pub(super) fn taken(&self, name: &str) -> Result<bool, Error> {
        self.verify()?;
        match fs::statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(meta) if fs::FileType::from_raw_mode(meta.st_mode).is_symlink() => Err(Error::new(ErrorKind::InvalidInput, LINK)),
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(err) => Err(error(err)),
        }
    }

    /// Exclusive final creation for new names, staged rename within the pinned parent for
    /// overwrite. No path is ever re-opened by its spelling to write the contents.
    pub(super) fn write(&self, assets: &AssetStore, asset: &AssetRef, name: &str, overwrite: bool) -> Result<u64, Error> {
        self.write_with_hook(assets, asset, name, overwrite, || {})
    }

    pub(super) fn write_with_hook(
        &self,
        assets: &AssetStore,
        asset: &AssetRef,
        name: &str,
        overwrite: bool,
        before_publish: impl FnOnce(),
    ) -> Result<u64, Error> {
        self.verify()?;
        if name.is_empty() || name.len() > nodecraft_format::export::MAX_NAME_BYTES || name.contains(['/', '\\', '\0']) || name.contains("..") {
            return Err(Error::new(ErrorKind::InvalidInput, "the export file name is invalid"));
        }
        if overwrite {
            // The replacement keeps the existing file's permission bits, as the single-file writer does.
            let existing = match fs::statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(meta) if fs::FileType::from_raw_mode(meta.st_mode).is_symlink() => return Err(Error::new(ErrorKind::InvalidInput, LINK)),
                Ok(meta) => Some(Mode::from_raw_mode(meta.st_mode & 0o7777)),
                Err(rustix::io::Errno::NOENT) => None,
                Err(err) => return Err(error(err)),
            };
            // A random, exclusively-created staging name cannot alias an existing user file.
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce).map_err(|_| Error::new(ErrorKind::Internal, "could not generate an export staging name"))?;
            let staging = format!(".nodecraft-export-{}", hex_name(&nonce));
            let fd =
                fs::openat(&self.parent, staging.as_str(), OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, PUBLIC_MODE)
                    .map_err(error)?;
            let mut file = File::from(fd);
            let kept = existing.map_or(Ok(()), |mode| fs::fchmod(&file, mode).map_err(error));
            let result = kept.and_then(|()| crate::export::copy_asset_to(assets, asset, &mut file));
            let result = result.and_then(|bytes| {
                file.sync_all().map_err(|err| crate::export::io_error(err.kind()))?;
                before_publish();
                self.verify()?;
                // renameat replaces a symlink *itself* (never its target). Still refuse links
                // here so the person's existing link is not silently destroyed.
                // `taken` refuses a link, including a dangling one; renameat itself never
                // follows a link swapped in after that lookup (it replaces the link inode).
                let _ = self.taken(name)?;
                fs::renameat(&self.parent, staging.as_str(), &self.parent, name).map_err(error)?;
                Ok(bytes)
            });
            drop(file);
            if result.is_err() {
                let _ = fs::unlinkat(&self.parent, staging.as_str(), AtFlags::empty());
            }
            result
        } else {
            // Never silently use a path-based fallback when link/rename primitives are missing.
            // An exclusive final descriptor rules out dangling and existing symlinks as targets.
            let fd = fs::openat(&self.parent, name, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, PUBLIC_MODE)
                .map_err(error)?;
            let mut file = File::from(fd);
            let result = crate::export::copy_asset_to(assets, asset, &mut file).and_then(|bytes| {
                file.sync_all().map_err(|err| crate::export::io_error(err.kind()))?;
                before_publish();
                self.verify()?;
                Ok(bytes)
            });
            if result.is_err() {
                // Only unlink the inode we created, never a concurrent process's replacement.
                if let (Ok(created), Ok(current)) = (fs::fstat(&file), fs::statat(&self.parent, name, AtFlags::SYMLINK_NOFOLLOW))
                    && created.st_dev == current.st_dev
                    && created.st_ino == current.st_ino
                {
                    let _ = fs::unlinkat(&self.parent, name, AtFlags::empty());
                }
            }
            result
        }
    }
}

fn hex_name(bytes: &[u8; 16]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(32);
    for byte in bytes {
        let _ = write!(result, "{byte:02x}");
    }
    result
}
