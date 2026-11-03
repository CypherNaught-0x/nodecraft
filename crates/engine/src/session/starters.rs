//! Starter graphs and their assets enter through the same atomic import/edit boundary as files.
use super::{Session, guard};
use crate::commands::Effect;
use nodecraft_format::asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::Error;
use serde_json::{Value, json};
use std::sync::Arc;
impl Session {
    pub(super) fn add_template(&mut self, command: &'static str, template: crate::templates::PreparedTemplate) -> Result<Value, Error> {
        let crate::templates::PreparedTemplate { entry, snapshot } = template;
        let changes = Changeset::new(snapshot.project.canvases().values().cloned().map(|canvas| Change::AddCanvas { canvas }).collect());
        let edit = Effect::Edit { changes, result: json!({"canvas":entry.canvas,"template":entry.id,"version":entry.version}), gesture: None };
        let sources = snapshot
            .assets
            .iter()
            .map(|(_, bytes)| AssetRange::new(Arc::new(MemorySource::new(Arc::from(bytes))), 0, bytes.len() as u64))
            .collect::<Result<Vec<_>, _>>()?;
        let prepared = if sources.is_empty() {
            guard(|| self.prepare(edit))?
        } else {
            let imports = self.prepare_imports(&sources, &TransferCancel::default(), &StreamMetrics::default())?;
            guard(|| self.prepare_imported_edit(edit, imports))?
        };
        self.commit_prepared(prepared, command)
    }
}
