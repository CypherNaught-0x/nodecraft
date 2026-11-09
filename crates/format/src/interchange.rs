//! The portable archive a timeline interchange export writes (#166): one ZIP holding the
//! document, a plain-text report of what it leaves out, and exactly the media it names, each
//! once, under the relative paths the document uses. Entries are stored, not compressed (media
//! is compressed already), and every asset is checked against its hash as it is copied. The
//! archive carries no absolute paths, no account details and nothing the timeline doesn't use.
use crate::AssetStore;
use crate::asset_source::{StreamMetrics, TransferCancel};
use nodecraft_types::{AssetRef, Error, ErrorKind};
use std::collections::BTreeSet;
use std::io::{Seek, Write};

/// Longest entry name, in bytes.
pub const MAX_ENTRY_NAME_BYTES: usize = 255;
/// Most media files one archive holds.
pub const MAX_MEDIA: usize = 256;
/// Largest document or report, in bytes.
pub const MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// Media admitted by one archive, including repeated entries. Verification needs at most
/// one source's private scratch at a time, under the same native workspace quota.
pub const MAX_MEDIA_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// The report's entry name.
pub const REPORT: &str = "report.txt";

/// Content-free progress. Bytes count media only, after verification and while copying into
/// the staged ZIP; they never imply that a destination has already been published.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Progress {
    pub verifying: bool,
    pub completed_media: u64,
    pub total_media: u64,
    pub copied_bytes: u64,
    pub total_bytes: u64,
}

/// What an archive holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bundle {
    /// The document's entry name, e.g. `Cut.fcpxml`.
    pub document: String,
    pub xml: String,
    /// What the document leaves out, for a person to read.
    pub report: String,
    /// Optional UTF-8 SubRip companion: its relative entry name and text.
    pub subtitles: Option<(String, String)>,
    /// Each media file's entry name (as the document names it) and the asset it holds.
    pub media: Vec<(String, AssetRef)>,
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Whether `name` is a safe relative entry name: `/`-separated segments, none empty, `.` or
/// `..`, no backslash, colon or control character, and no longer than [`MAX_ENTRY_NAME_BYTES`].
pub fn safe_entry_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_ENTRY_NAME_BYTES
        && !name.chars().any(|c| c.is_control() || matches!(c, '\\' | ':'))
        && name.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

impl Bundle {
    /// Refuses unsafe, repeated or too many entry names, and oversized text.
    pub fn check(&self) -> Result<(), Error> {
        if self.media.len() > MAX_MEDIA {
            return Err(invalid("the timeline names too many media files to export"));
        }
        if self.xml.len() > MAX_TEXT_BYTES || self.report.len() > MAX_TEXT_BYTES || self.subtitles.as_ref().is_some_and(|(_, text)| text.len() > MAX_TEXT_BYTES)
        {
            return Err(invalid("the exported document is too large"));
        }
        let mut names = BTreeSet::new();
        for name in std::iter::once(self.document.as_str())
            .chain([REPORT])
            .chain(self.subtitles.iter().map(|(name, _)| name.as_str()))
            .chain(self.media.iter().map(|(name, _)| name.as_str()))
        {
            if !safe_entry_name(name) {
                return Err(invalid("an exported file name isn't a safe relative path"));
            }
            if !names.insert(name.to_lowercase()) {
                return Err(invalid("two exported files have the same name"));
            }
        }
        Ok(())
    }

    /// Descriptor-only admission: no body is read or authenticated here.
    pub fn check_assets(&self, assets: &AssetStore) -> Result<u64, Error> {
        self.check()?;
        let mut total = 0u64;
        for (_, asset) in &self.media {
            let descriptor = assets.descriptor(&asset.hash).ok_or_else(|| invalid("the timeline references missing media"))?;
            total = total
                .checked_add(descriptor.bytes)
                .filter(|bytes| *bytes <= MAX_MEDIA_BYTES)
                .ok_or_else(|| invalid("the timeline's media exceeds the 8 GiB export limit"))?;
        }
        Ok(total)
    }

    /// Writes the archive to `out` and hands it back. A failure part way leaves `out` partly
    /// written: callers write into a staged file they publish only on success.
    pub fn write_to<W: Write + Seek>(&self, assets: &AssetStore, out: W, cancel: &TransferCancel) -> Result<W, Error> {
        self.write_controlled(assets, out, cancel, &mut |_| {})
    }

    /// Worker-side streaming, with one independently verified lease per media file. Indexed
    /// sources are never all staged, and no live verified-cache entry is published.
    pub fn write_controlled<W: Write + Seek>(
        &self,
        assets: &AssetStore,
        out: W,
        cancel: &TransferCancel,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<W, Error> {
        use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
        cancel.check()?;
        let total_bytes = self.check_assets(assets)?;
        let mut status = Progress { total_media: self.media.len() as u64, total_bytes, ..Default::default() };
        progress(status);
        let failed = |_| Error::new(ErrorKind::Internal, "the archive couldn't be written");
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut zip = ZipWriter::new(out);
        for (name, text) in [(self.document.as_str(), &self.xml), (REPORT, &self.report)] {
            cancel.check()?;
            zip.start_file(name, options).map_err(failed)?;
            write_text(&mut zip, text, cancel)?;
        }
        if let Some((name, text)) = &self.subtitles {
            cancel.check()?;
            zip.start_file(name, options).map_err(failed)?;
            write_text(&mut zip, text, cancel)?;
        }
        for (name, asset) in &self.media {
            cancel.check()?;
            status.verifying = true;
            progress(status);
            let lease = open(assets, asset, cancel)?;
            let length = lease.byte_len();
            status.verifying = false;
            progress(status);
            zip.start_file(name.as_str(), options.large_file(length >= u64::from(u32::MAX))).map_err(failed)?;
            let mut output = MediaWriter { output: &mut zip, status: &mut status, progress };
            let copied = lease.copy_to(&mut output, cancel, &StreamMetrics::default())?;
            if copied != length {
                return Err(Error::new(ErrorKind::Internal, "a media file changed while it was exported"));
            }
            status.completed_media = status.completed_media.checked_add(1).ok_or_else(|| invalid("the media count overflowed"))?;
            progress(status);
        }
        cancel.check()?;
        let out = zip.finish().map_err(failed)?;
        cancel.check()?;
        Ok(out)
    }
}

fn write_text(out: &mut dyn Write, text: &str, cancel: &TransferCancel) -> Result<(), Error> {
    for chunk in text.as_bytes().chunks(crate::asset_source::TRANSFER_BYTES) {
        cancel.check()?;
        out.write_all(chunk).map_err(|_| Error::new(ErrorKind::Internal, "the archive couldn't be written"))?;
    }
    cancel.check()
}

fn open(assets: &AssetStore, asset: &AssetRef, cancel: &TransferCancel) -> Result<crate::asset_store::VerifiedAsset, Error> {
    #[cfg(target_arch = "wasm32")]
    {
        assets.open_verified(asset, cancel, &StreamMetrics::default())
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            cancel.check()?;
            match assets.open_export_verified(asset, cancel, &StreamMetrics::default()) {
                Err(error) if error.kind == ErrorKind::Busy && std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
                result => return result,
            }
        }
    }
}

struct MediaWriter<'a> {
    output: &'a mut dyn Write,
    status: &'a mut Progress,
    progress: &'a mut dyn FnMut(Progress),
}
impl Write for MediaWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let n = self.output.write(bytes)?;
        self.status.copied_bytes = self
            .status
            .copied_bytes
            .checked_add(n as u64)
            .filter(|bytes| *bytes <= self.status.total_bytes)
            .ok_or_else(|| std::io::Error::other("the media byte count changed"))?;
        (self.progress)(*self.status);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

#[cfg(test)]
mod tests;
