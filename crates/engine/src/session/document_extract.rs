//! Publish a PDF extraction through the same candidate asset and changeset boundary as imports.
use super::*;
use crate::document_cmds::{ExtractionRequest, Produced};
use nodecraft_format::asset_source::{AssetRange, MemorySource, StreamMetrics, TransferCancel};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{AssetRef, MediaMeta, ParamValues};
use serde_json::json;
use std::collections::BTreeMap;

impl Session {
    pub(super) fn prepare_document_extract(&self, request: ExtractionRequest) -> Result<Prepared, Error> {
        ExtractionRequest::check_access(&self.context(Phase::Dispatch))?;
        self.execution.next()?;
        let (extract, kind, parameter, value, imports) = match request.produce(self)? {
            Produced::Text { extract, text } => (extract, "source.text", "text", nodecraft_types::Value::Text(text), None),
            Produced::Page { extract, bytes } => {
                let info = nodecraft_media::probe_document(&bytes, &nodecraft_media::DocumentLimits::default())
                    .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("the extracted PDF can't be read: {e}")))?;
                if info.pages != 1 {
                    return Err(invalid("the extracted PDF must contain exactly one page"));
                }
                let length = u64::try_from(bytes.len()).map_err(|_| invalid("the extracted PDF size cannot be represented"))?;
                let source = Arc::new(MemorySource::new(Arc::from(bytes)));
                let range = AssetRange::new(source, 0, length)?;
                let batch = self.prepare_imports(&[range], &TransferCancel::default(), &StreamMetrics::default())?;
                let hash = batch.hashes().first().copied().ok_or_else(|| invalid("the extracted PDF has no imported identity"))?;
                let asset = AssetRef { hash, mime: "application/pdf".parse()?, meta: MediaMeta { page_count: Some(1), ..Default::default() } };
                (extract, "source.document", "asset", nodecraft_types::Value::Media(asset), Some(batch))
            }
        };
        let supplied: ParamValues = [(parameter.to_owned(), value)].into_iter().collect();
        let mut node = self.kinds.get(kind)?.instantiate_instance(extract.id, BTreeMap::new(), &supplied)?;
        node.pos = extract.at;
        let effect =
            Effect::Edit { changes: Changeset::new(vec![Change::AddNode { canvas: extract.canvas, node }]), result: json!({"id":extract.id}), gesture: None };
        match imports {
            Some(batch) => self.prepare_imported_edit(effect, batch),
            None => self.prepare(effect),
        }
    }
}
