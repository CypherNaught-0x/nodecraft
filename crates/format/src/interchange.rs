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
/// The report's entry name.
pub const REPORT: &str = "report.txt";

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

    /// Writes the archive to `out` and hands it back. A failure part way leaves `out` partly
    /// written: callers write into a staged file they publish only on success.
    pub fn write_to<W: Write + Seek>(&self, assets: &AssetStore, out: W, cancel: &TransferCancel) -> Result<W, Error> {
        use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};
        self.check()?;
        let failed = |_| Error::new(ErrorKind::Internal, "the archive couldn't be written");
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let mut zip = ZipWriter::new(out);
        for (name, text) in [(self.document.as_str(), &self.xml), (REPORT, &self.report)] {
            zip.start_file(name, options).map_err(failed)?;
            zip.write_all(text.as_bytes()).map_err(|_| Error::new(ErrorKind::Internal, "the archive couldn't be written"))?;
        }
        if let Some((name, text)) = &self.subtitles {
            cancel.check()?;
            zip.start_file(name, options).map_err(failed)?;
            zip.write_all(text.as_bytes()).map_err(|_| Error::new(ErrorKind::Internal, "the archive couldn't be written"))?;
        }
        for (name, asset) in &self.media {
            cancel.check()?;
            let length = assets.open_verified(asset, cancel, &StreamMetrics::default())?.byte_len();
            zip.start_file(name.as_str(), options.large_file(length >= u64::from(u32::MAX))).map_err(failed)?;
            let copied = assets.copy_verified_to(asset, &mut zip, cancel, &StreamMetrics::default())?;
            if copied != length {
                return Err(Error::new(ErrorKind::Internal, "a media file changed while it was exported"));
            }
        }
        zip.finish().map_err(failed)
    }
}

#[cfg(test)]
mod tests;
