//! Writes one asset's bytes (`history.export`, #557) or a timeline interchange archive
//! (`timeline.export_interchange`, #166) to a file the caller names: never over an existing file
//! unless asked, staged so a failed write leaves nothing behind.
use nodecraft_format::AssetStore;
use nodecraft_format::interchange::Bundle;
use nodecraft_types::{AssetRef, Error, ErrorKind};
use std::path::Path;

/// What an export writes.
#[derive(Clone, Debug, PartialEq)]
pub enum Exported {
    /// One asset's bytes, as they are stored.
    Asset(AssetRef),
    /// An archive of a document and the media it names.
    Bundle(Box<Bundle>),
}

/// Longest export path, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;

/// Refuses a path that can't name a new file: relative, a folder, or too long. Messages never
/// repeat the path.
pub(crate) fn check(path: &Path) -> Result<(), Error> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() || bytes.len() > MAX_PATH_BYTES || bytes.contains(&0) {
        return Err(Error::new(ErrorKind::InvalidInput, "the export path is empty, too long or malformed"));
    }
    if !path.is_absolute() {
        return Err(Error::new(ErrorKind::InvalidInput, "the export path must be absolute"));
    }
    if path.file_name().is_none() {
        return Err(Error::new(ErrorKind::InvalidInput, "the export path must end in a file name"));
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn write(_: &AssetStore, _: &Exported, _: &Path, _: bool) -> Result<u64, Error> {
    Err(Error::new(ErrorKind::Unsupported, "exporting to a file needs the desktop app"))
}

/// Streams `what` to `path`; returns how many bytes were written.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write(assets: &AssetStore, what: &Exported, path: &Path, overwrite: bool) -> Result<u64, Error> {
    use nodecraft_format::asset_source::{StreamMetrics, TransferCancel};
    use std::io::Seek;
    check(path)?;
    if path.is_dir() {
        return Err(Error::new(ErrorKind::InvalidInput, "the export path is a folder; name a file in it"));
    }
    if !path.parent().is_some_and(Path::is_dir) {
        return Err(Error::new(ErrorKind::NotFound, "the export folder doesn't exist"));
    }
    let cancel = TransferCancel::default();
    // Checked before the file appears. The archive checks each asset as it copies it, into a
    // staged file, so a missing or damaged asset leaves nothing behind either way.
    match what {
        Exported::Asset(asset) => drop(assets.open_verified(asset, &cancel, &StreamMetrics::default())?),
        Exported::Bundle(bundle) => bundle.check()?,
    }
    let mut copied: Result<u64, Error> = Ok(0);
    let mut failed = None;
    let fill = |file: &mut std::fs::File| {
        let mut out = Recording { file, failed: &mut failed };
        copied = match what {
            Exported::Asset(asset) => assets.copy_verified_to(asset, &mut out, &cancel, &StreamMetrics::default()),
            Exported::Bundle(bundle) => bundle
                .write_to(assets, &mut out, &cancel)
                .and_then(|out| out.stream_position().map_err(|_| Error::new(ErrorKind::Internal, "the archive couldn't be written"))),
        };
        match &copied {
            Ok(_) => Ok(()),
            Err(_) => Err(std::io::Error::other("the export couldn't be written")),
        }
    };
    let written = if overwrite { craft_atomic::atomic_write_with(path, fill) } else { craft_atomic::write_new_with(path, fill) };
    match written {
        Ok(()) => copied,
        // A write that failed inside the copy says why the file did; else the copy's own error.
        Err(error) => Err(match (failed, copied) {
            (Some(kind), _) => io_error(kind),
            (None, Err(copy)) => copy,
            (None, Ok(_)) => io_error(error.kind()),
        }),
    }
}

/// A file writer that remembers why a write failed, which the copy's own error doesn't carry.
#[cfg(not(target_arch = "wasm32"))]
struct Recording<'a> {
    file: &'a mut std::fs::File,
    failed: &'a mut Option<std::io::ErrorKind>,
}
#[cfg(not(target_arch = "wasm32"))]
impl std::io::Write for Recording<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.file.write(bytes);
        record(self.failed, &written);
        written
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let flushed = self.file.flush();
        record(self.failed, &flushed);
        flushed
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl std::io::Seek for Recording<'_> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let sought = self.file.seek(to);
        record(self.failed, &sought);
        sought
    }
}

/// Keeps the first real failure; `write_all` retries an interrupted write, so that isn't one.
#[cfg(not(target_arch = "wasm32"))]
fn record<T>(failed: &mut Option<std::io::ErrorKind>, result: &std::io::Result<T>) {
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::Interrupted
        && failed.is_none()
    {
        *failed = Some(error.kind());
    }
}

/// What a failed write means to the person exporting, without the path.
#[cfg(not(target_arch = "wasm32"))]
fn io_error(kind: std::io::ErrorKind) -> Error {
    use std::io::ErrorKind as Io;
    match kind {
        Io::AlreadyExists => Error::new(ErrorKind::Conflict, "a file with that name is already there; export with overwrite to replace it"),
        Io::StorageFull | Io::QuotaExceeded | Io::FileTooLarge => Error::new(ErrorKind::Internal, "the disk is full; free some space and export again"),
        Io::PermissionDenied | Io::ReadOnlyFilesystem => Error::new(ErrorKind::InvalidInput, "NodeCraft can't write there; choose another folder"),
        Io::NotFound => Error::new(ErrorKind::NotFound, "the export folder doesn't exist"),
        Io::IsADirectory => Error::new(ErrorKind::InvalidInput, "the export path is a folder; name a file in it"),
        _ => Error::new(ErrorKind::Internal, "the file couldn't be written"),
    }
}

#[cfg(test)]
mod tests;
