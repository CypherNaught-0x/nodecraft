//! Retain admitted provider bytes through the existing stream-import boundary when needed.
//! The classic convenience API deliberately stays capped at16MiB; native result recording,
//! resumed recording and held-result actions share the streamed path without copying the Arc.
use crate::Session;
use nodecraft_format::asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_types::Blake3;
use std::sync::Arc;

pub(super) fn import(session: &mut Session, expected: Blake3, bytes: Arc<[u8]>) -> Result<(), String> {
    if bytes.len() > crate::generate::MAX_INLINE_OUTPUT_BYTES {
        return Err("a generated asset exceeds the output limit".into());
    }
    let imported = if bytes.len() > nodecraft_format::MAX_ASSET_BYTES && session.assets().context().supports_large_assets() {
        let length = u64::try_from(bytes.len()).map_err(|_| "a generated asset size is out of range")?;
        let range = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, length).map_err(|error| error.message)?;
        session.import_asset_from(&range, &TransferCancel::default(), &StreamMetrics::default()).map_err(|error| error.message)?
    } else {
        session.import_asset(&bytes).map_err(|error| error.message)?
    };
    if imported != expected {
        return Err("a generated asset changed while it was imported".into());
    }
    Ok(())
}
