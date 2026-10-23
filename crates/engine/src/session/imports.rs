//! One Session-bound candidate store; no merging independently minted candidates.
use super::{Session, SourceStamp, guard};
use crate::commands::{Context, Effect, Phase, validate_json};
use nodecraft_format::{
    AssetStore,
    asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel, copy_and_hash},
};
use nodecraft_types::{Blake3, Error, ErrorKind};
use serde_json::Value;
use std::{collections::BTreeSet, sync::Arc};

mod readers;

/// Opaque non-Clone import authority. Hashes are metadata until the batch is consumed.
pub struct PreparedImports {
    source: SourceStamp,
    assets: AssetStore,
    hashes: Vec<Blake3>,
    next_generation: u64,
    next_identity: Arc<()>,
}
impl PreparedImports {
    pub fn hashes(&self) -> &[Blake3] {
        &self.hashes
    }
}
fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
impl Session {
    pub fn prepare_imports(&self, sources: &[AssetRange], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<PreparedImports, Error> {
        guard(|| self.prepare_imports_inner(sources, cancel, metrics))
    }
    fn prepare_imports_inner(&self, sources: &[AssetRange], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<PreparedImports, Error> {
        let next_generation = self.execution.next()?;
        if sources.is_empty() || sources.len() > nodecraft_format::MAX_ASSETS {
            return Err(invalid("import batch requires 1 to 1024 sources"));
        }
        let native = self.assets.context().supports_large_assets();
        let cap = if native { nodecraft_format::asset_source::MAX_LARGE_ASSET_BYTES } else { nodecraft_format::MAX_ASSET_BYTES as u64 };
        // Validate EVERY descriptor before touching the first source. Repeated content
        // is not an aggregate input quota; unique retained bytes are admitted below.
        for source in sources {
            if source.byte_len() > cap {
                return Err(invalid("import source exceeds this host's per-asset limit"));
            }
        }
        let mut hashes = Vec::new();
        hashes.try_reserve_exact(sources.len()).map_err(|_| invalid("cannot reserve import results"))?;
        let mut duplicates = Vec::new();
        duplicates.try_reserve_exact(sources.len()).map_err(|_| invalid("cannot reserve import duplicate set"))?;
        let mut assets = self.assets.clone();
        let source = SourceStamp { identity: Arc::clone(&self.admission_identity), revision: self.revision };
        let next_identity = Arc::new(());
        cancel.check()?;
        for input in sources {
            let digest = copy_and_hash(input, &mut std::io::sink(), cancel, metrics)?;
            if let Some(existing) = assets.descriptor(&digest.hash) {
                if existing.bytes != digest.bytes {
                    return Err(invalid("duplicate import has inconsistent content length"));
                }
                if !duplicates.contains(&digest.hash) {
                    duplicates.push(digest.hash);
                }
            } else {
                let hash = if native {
                    assets.import_stream(input, cancel, metrics)?
                } else {
                    let bytes = input.read_small(nodecraft_format::MAX_ASSET_BYTES, metrics)?;
                    cancel.check()?;
                    if Blake3::from_bytes(*blake3::hash(&bytes).as_bytes()) != digest.hash || bytes.len() as u64 != digest.bytes {
                        return Err(invalid("import source changed between verification passes"));
                    }
                    assets.insert(&bytes)?
                };
                if hash != digest.hash || assets.descriptor(&hash).is_none_or(|entry| entry.bytes != digest.bytes) {
                    return Err(invalid("import source changed between verification passes"));
                }
            }
            hashes.push(digest.hash);
        }
        // One final call, not per-item compaction. Each uncached duplicate may still
        // verify/pack internally; caller metrics include that bounded additional work.
        let duplicates = duplicates.into_iter().collect::<BTreeSet<_>>();
        assets.verify_retained(&duplicates, cancel, metrics)?;
        cancel.check()?;
        Ok(PreparedImports { source, assets, hashes, next_generation, next_identity })
    }
    fn commit_imports(&mut self, prepared: PreparedImports) -> Result<Vec<Blake3>, Error> {
        if !prepared.source.matches(self) {
            return Err(Error::new(ErrorKind::Conflict, "prepared imports no longer match this Session"));
        }
        self.assets = prepared.assets;
        self.admission_identity = prepared.next_identity;
        self.execution.advance(prepared.next_generation);
        Ok(prepared.hashes)
    }
    /// Import-only publication: no document event or undo step is fabricated.
    pub fn import_asset_from(&mut self, source: &AssetRange, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Blake3, Error> {
        self.settle_for_edit()?;
        let prepared = self.prepare_imports(std::slice::from_ref(source), cancel, metrics)?;
        let hash = prepared.hashes.first().copied().ok_or_else(|| invalid("import batch has no result"))?;
        self.commit_imports(prepared)?;
        Ok(hash)
    }
    pub(super) fn import_small(&mut self, bytes: &[u8]) -> Result<Blake3, Error> {
        self.execution.next()?;
        if bytes.len() > nodecraft_format::MAX_ASSET_BYTES {
            return Err(invalid("asset exceeds 16 MiB convenience limit; use stream import"));
        }
        let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64)?;
        self.import_asset_from(&range, &TransferCancel::default(), &StreamMetrics::default())
    }
    /// Commits an edit that shows `media`, copying those the project lacks in from the Elements
    /// library first (#552): the copies land with the edit, or not at all.
    pub(super) fn place(&mut self, command: &'static str, media: Vec<Blake3>, changes: nodecraft_graph::Changeset, result: Value) -> Result<Value, Error> {
        let edit = Effect::Edit { changes, result, gesture: None };
        let missing: BTreeSet<Blake3> = media.into_iter().filter(|hash| !self.assets.contains(hash)).collect();
        if missing.is_empty() {
            let prepared = guard(|| self.prepare(edit))?;
            return self.commit_prepared(prepared, command);
        }
        let shelf = self.resources.require::<crate::element_library::ElementShelf>()?.clone();
        let mut sources = Vec::new();
        for hash in &missing {
            let bytes = shelf.media(hash)?;
            let len = bytes.len() as u64;
            sources.push(AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, len)?);
        }
        let imports = self.prepare_imports(&sources, &TransferCancel::default(), &StreamMetrics::default())?;
        if imports.hashes.iter().copied().collect::<BTreeSet<_>>() != missing {
            return Err(invalid("an image in the Elements library is damaged; save the Element again"));
        }
        let prepared = guard(|| self.prepare_with_assets(edit, &imports.assets, true))?;
        self.commit_prepared(prepared, command)
    }
    /// Dispatch one ordinary graph Edit using exactly this batch's candidate store.
    /// Service/file/history effects never execute through this import-only boundary.
    pub fn dispatch_with_imports(&mut self, command: &str, params: &Value, prepared: PreparedImports) -> Result<Value, Error> {
        self.settle_for_edit()?;
        let before = self.file_status();
        let result = guard(|| {
            if !prepared.source.matches(self) {
                return Err(Error::new(ErrorKind::Conflict, "prepared imports no longer match this Session"));
            }
            validate_json(params)?;
            let spec = *self.registry.get(command)?;
            let mut context: Context<'_> = self.context(Phase::Dispatch);
            // Clipboard reference admission must see this exact opaque candidate, not
            // the pre-import live map. No candidate is published until the Edit commits.
            context.assets = &prepared.assets;
            (spec.enabled)(&context, params)?;
            let effect = (spec.run)(&context, params)?;
            if !matches!(effect, Effect::Edit { .. }) {
                return Err(Error::new(ErrorKind::Unsupported, "import dispatch requires a graph Edit command"));
            }
            let staged = self.prepare_with_assets(effect, &prepared.assets, true)?;
            self.commit_prepared(staged, spec.id)
        });
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_format::asset_source::AssetSource;
    use nodecraft_types::ProjectId;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn exhausted_execution_rejects_batch_before_source_read_and_preserves_identity() {
        struct Count(Arc<AtomicUsize>);
        impl AssetSource for Count {
            fn byte_len(&self) -> u64 {
                1
            }
            fn read_at(&self, _: u64, _: &mut [u8]) -> Result<usize, Error> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Err(invalid("unexpected read"))
            }
        }
        let mut session = Session::new(nodecraft_graph::Project::new(ProjectId::from_bytes([4; 16]), "Overflow").unwrap()).unwrap();
        let identity = session.admission_identity.clone();
        session.exhaust_execution_generation();
        let reads = Arc::new(AtomicUsize::new(0));
        let range = AssetRange::new(Arc::new(Count(reads.clone())), 0, 1).unwrap();
        let error = session.prepare_imports(&[range], &TransferCancel::default(), &StreamMetrics::default()).err().unwrap();
        assert_eq!(error.kind, ErrorKind::Overflow);
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        assert!(Arc::ptr_eq(&identity, &session.admission_identity));
        assert!(session.assets().is_empty());
    }
    #[test]
    fn collection_preflights_changed_authority_but_no_change_allows_exhaustion() {
        let mut session = Session::new(nodecraft_graph::Project::new(ProjectId::from_bytes([4; 16]), "GC overflow").unwrap()).unwrap();
        let identity = session.admission_identity.clone();
        session.exhaust_execution_generation();
        session.prune_assets().unwrap();
        assert!(!session.collect_assets().unwrap());
        assert!(Arc::ptr_eq(&identity, &session.admission_identity));
        // Install a deliberately unreferenced resident entry through the private fixture seam.
        let hash = session.assets.insert(b"orphan").unwrap();
        assert_eq!(session.collect_assets().unwrap_err().kind, ErrorKind::Overflow);
        assert!(session.assets.contains(&hash));
        assert!(Arc::ptr_eq(&identity, &session.admission_identity));
    }
    #[test]
    fn collection_refuses_missing_history_root_before_retiring_other_entries() {
        let mut session = Session::new(nodecraft_graph::Project::new(ProjectId::from_bytes([4; 16]), "GC roots").unwrap()).unwrap();
        let orphan = session.import_asset(b"orphan").unwrap();
        let identity = session.admission_identity.clone();
        let mut node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([5; 16]), "test.media");
        node.params.insert(
            "asset".into(),
            nodecraft_types::Value::Media(nodecraft_types::AssetRef {
                hash: Blake3::from_bytes([9; 32]),
                mime: "text/plain".parse().unwrap(),
                meta: Default::default(),
            }),
        );
        session.redo.push(super::super::Entry {
            inverse: nodecraft_graph::Changeset::new(vec![nodecraft_graph::Change::AddNode { canvas: nodecraft_types::CanvasId::from_bytes([6; 16]), node }]),
            bytes: 1,
            state: 0,
            shared: false,
        });
        assert_eq!(session.collect_assets().unwrap_err().kind, ErrorKind::InvalidInput);
        assert!(session.assets.contains(&orphan));
        assert!(Arc::ptr_eq(&identity, &session.admission_identity));
        assert_eq!(session.redo.entries.len(), 1);
    }
}
