use super::*;
use std::io::Read;

impl Session {
    /// Prepare one native reader batch. Errors may consume reader prefixes, but never publish
    /// a partial batch. Readers must provide their own timeout for blocking I/O.
    pub fn prepare_reader_imports(&self, readers: &mut [&mut dyn Read], cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<PreparedImports, Error> {
        guard(|| {
            if readers.is_empty() || readers.len() > nodecraft_format::MAX_ASSETS {
                return Err(invalid("reader import batch requires 1 to 1024 inputs"));
            }
            if !self.assets.context().supports_large_assets() {
                return Err(Error::new(ErrorKind::Unsupported, "reader import requires native private asset storage"));
            }
            cancel.check()?;
            let source = SourceStamp { identity: Arc::clone(&self.admission_identity), revision: self.revision };
            let next_generation = self.execution.next()?;
            let next_identity = Arc::new(());
            let mut hashes = Vec::new();
            hashes.try_reserve_exact(readers.len()).map_err(|_| invalid("cannot reserve reader import results"))?;
            let mut unique = Vec::new();
            unique.try_reserve_exact(readers.len()).map_err(|_| invalid("cannot reserve reader import identities"))?;
            let mut assets = self.assets.clone();
            for reader in readers {
                let hash = assets.import_reader(*reader, cancel, metrics)?;
                hashes.push(hash);
                if !unique.contains(&hash) {
                    unique.push(hash);
                }
            }
            let unique = unique.into_iter().collect();
            assets.verify_retained(&unique, cancel, metrics)?;
            cancel.check()?;
            Ok(PreparedImports { source, assets, hashes, next_generation, next_identity })
        })
    }

    /// Import only, using the same opaque Session receipt and success-only authority change.
    pub fn import_asset_reader(&mut self, reader: &mut dyn Read, cancel: &TransferCancel, metrics: &StreamMetrics) -> Result<Blake3, Error> {
        self.settle_for_edit();
        let prepared = self.prepare_reader_imports(&mut [reader], cancel, metrics)?;
        let hash = prepared.hashes.first().copied().ok_or_else(|| invalid("reader import has no result"))?;
        self.commit_imports(prepared)?;
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::ProjectId;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(Arc<AtomicUsize>);
    impl Read for Count {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(0)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn unknown_reader_standalone_import_changes_authority_only_on_success() {
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("private reader detail"))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let project = nodecraft_graph::Project::new(ProjectId::from_bytes([4; 16]), "Reader authority").unwrap();
        let mut session = Session::with_storage(
            nodecraft_format::Snapshot { project, assets: AssetStore::default(), epoch: 0 },
            Box::new(nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap()),
        )
        .unwrap();
        let cancel = TransferCancel::default();
        let metrics = StreamMetrics::default();
        let batch = session.prepare_reader_imports(&mut [&mut std::io::Cursor::new(b"candidate")], &cancel, &metrics).unwrap();
        assert!(session.import_asset_reader(&mut Failed, &cancel, &metrics).is_err());
        assert!(batch.source.matches(&session));
        assert!(session.assets().is_empty());
        session.import_asset_reader(&mut std::io::Cursor::new(b"published"), &cancel, &metrics).unwrap();
        assert!(!batch.source.matches(&session));
        assert_eq!(session.assets().len(), 1);
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
    }
    #[test]
    fn unknown_reader_batch_preflights_count_host_cancel_and_execution_before_any_read() {
        let reads = Arc::new(AtomicUsize::new(0));
        let mut reader = Count(reads.clone());
        let mut memory = Session::new(nodecraft_graph::Project::new(ProjectId::from_bytes([4; 16]), "Reader bounds").unwrap()).unwrap();
        assert_eq!(
            memory.prepare_reader_imports(&mut [&mut reader], &TransferCancel::default(), &StreamMetrics::default()).err().unwrap().kind,
            ErrorKind::Unsupported
        );
        memory.import_asset(b"small remains supported").unwrap();
        assert_eq!(reads.load(Ordering::Relaxed), 0);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dir = tempfile::tempdir().unwrap();
            let snapshot = nodecraft_format::Snapshot { project: memory.document().clone(), assets: AssetStore::default(), epoch: 0 };
            let mut native = Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(dir.path().join("recovery")).unwrap())).unwrap();
            assert!(native.prepare_reader_imports(&mut [], &TransferCancel::default(), &StreamMetrics::default()).is_err());
            let mut many = (0..1025).map(|_| Count(reads.clone())).collect::<Vec<_>>();
            let mut borrowed = many.iter_mut().map(|r| r as &mut dyn Read).collect::<Vec<_>>();
            assert!(native.prepare_reader_imports(&mut borrowed, &TransferCancel::default(), &StreamMetrics::default()).is_err());
            let cancel = TransferCancel::default();
            cancel.cancel();
            assert!(native.prepare_reader_imports(&mut [&mut reader], &cancel, &StreamMetrics::default()).is_err());
            let identity = native.admission_identity.clone();
            native.exhaust_execution_generation();
            assert_eq!(
                native.prepare_reader_imports(&mut [&mut reader], &TransferCancel::default(), &StreamMetrics::default()).err().unwrap().kind,
                ErrorKind::Overflow
            );
            assert!(Arc::ptr_eq(&identity, &native.admission_identity));
            assert!(native.assets().is_empty());
        }
        assert_eq!(reads.load(Ordering::Relaxed), 0);
    }
}
