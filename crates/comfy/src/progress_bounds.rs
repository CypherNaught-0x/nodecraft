//! Conservative storage charges for retained ComfyUI progress and extracted file metadata.
use crate::{ComfyError, FileRef};

/// Maximum workflow node IDs retained by a progress tracker.
pub const MAX_PROGRESS_NODES: usize = 10_000;
/// Maximum distinct output associations or extracted files.
pub const MAX_OUTPUT_FILES: usize = 10_000;
/// Aggregate metadata storage budget, including conservative container/identifier overhead.
pub const MAX_RETAINED_METADATA_BYTES: usize = 8 * 1024 * 1024;
/// Maximum entries examined per event, constructor or aggregate extraction.
pub const MAX_OUTPUT_ITEMS: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    pub nodes: usize,
    pub files: usize,
    pub bytes: usize,
    pub scanned: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self { nodes: MAX_PROGRESS_NODES, files: MAX_OUTPUT_FILES, bytes: MAX_RETAINED_METADATA_BYTES, scanned: MAX_OUTPUT_ITEMS }
    }
}

pub(crate) fn exceeded() -> ComfyError {
    ComfyError::Malformed { what: "ComfyUI progress/output metadata", message: "metadata count or storage limit exceeded".into() }
}

pub(crate) fn sum(parts: &[usize]) -> usize {
    parts.iter().fold(0usize, |total, part| total.saturating_add(*part))
}
pub(crate) fn file_bytes(file: &FileRef) -> usize {
    sum(&[file.filename.len(), file.subfolder.len(), file.kind.len()])
}
pub(crate) fn association_bytes(node: &str, file: &FileRef) -> usize {
    // Output Vec plus dedup map/set copies, including spare hash-table/Vec capacity.
    1024usize.saturating_add(node.len().saturating_add(file_bytes(file)).saturating_mul(2))
}

#[derive(Default)]
pub(crate) struct FileBudget {
    files: usize,
    bytes: usize,
    scanned: usize,
    limits: Limits,
}
impl FileBudget {
    pub(crate) fn scan(&mut self, count: usize) -> Result<(), ComfyError> {
        self.scanned = self.scanned.saturating_add(count);
        if self.scanned > self.limits.scanned {
            return Err(exceeded());
        }
        Ok(())
    }
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), ComfyError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > self.limits.bytes {
            return Err(exceeded());
        }
        Ok(())
    }
    pub(crate) fn file(&mut self, bytes: usize) -> Result<(), ComfyError> {
        self.files = self.files.saturating_add(1);
        if self.files > self.limits.files {
            return Err(exceeded());
        }
        self.charge(bytes.saturating_add(256))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extraction_budgets_accept_exact_boundaries_then_reject() {
        let mut budget = FileBudget { limits: Limits { files: 1, bytes: 260, scanned: 2, ..Limits::default() }, ..FileBudget::default() };
        budget.scan(2).unwrap();
        budget.file(4).unwrap();
        assert!(budget.scan(1).is_err());
        assert!(budget.file(0).is_err());
        let mut budget = FileBudget { limits: Limits { bytes: 3, ..Limits::default() }, ..FileBudget::default() };
        budget.charge(3).unwrap();
        assert!(budget.charge(1).is_err());
    }
}
