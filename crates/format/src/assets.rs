use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

#[cfg(test)]
thread_local! {
    pub(crate) static ASSET_NODE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub const MAX_ASSETS: usize = 1024;
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TOTAL_ASSET_BYTES: usize = 48 * 1024 * 1024;

/// Immutable, verified content. Cloning a store shares the bytes, never copies media buffers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetStore {
    assets: BTreeMap<Blake3, Arc<[u8]>>,
    bytes: usize,
}

impl AssetStore {
    pub fn insert(&mut self, bytes: &[u8]) -> Result<Blake3, Error> {
        if bytes.len() > MAX_ASSET_BYTES {
            return Err(crate::invalid("asset exceeds 16 MiB limit"));
        }
        let digest = crate::hash(bytes);
        if !self.assets.contains_key(&digest) {
            let total = self.bytes.saturating_add(bytes.len());
            if self.assets.len() >= MAX_ASSETS || total > MAX_TOTAL_ASSET_BYTES {
                return Err(crate::invalid("asset store exceeds count or 48 MiB total limit"));
            }
            self.assets.insert(digest, Arc::from(bytes));
            self.bytes = total;
        }
        Ok(digest)
    }

    pub fn retain(&mut self, roots: &BTreeSet<Blake3>) {
        self.assets.retain(|hash, _| roots.contains(hash));
        self.bytes = self.assets.values().fold(0_usize, |total, bytes| total.saturating_add(bytes.len()));
    }

    pub fn get(&self, hash: &Blake3) -> Option<&[u8]> {
        self.assets.get(hash).map(AsRef::as_ref)
    }
    pub fn len(&self) -> usize {
        self.assets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }
    pub fn byte_len(&self) -> usize {
        self.bytes
    }
    pub fn iter(&self) -> impl Iterator<Item = (&Blake3, &[u8])> {
        self.assets.iter().map(|(hash, bytes)| (hash, bytes.as_ref()))
    }
    pub(crate) fn referenced_snapshot(&self, hashes: &BTreeSet<Blake3>) -> Result<Self, Error> {
        let mut snapshot = Self::default();
        for hash in hashes {
            let bytes = self.assets.get(hash).ok_or_else(|| crate::invalid("journal replay references a missing retained asset"))?;
            snapshot.bytes = snapshot.bytes.saturating_add(bytes.len());
            snapshot.assets.insert(*hash, Arc::clone(bytes));
        }
        Ok(snapshot)
    }

    fn validate_value_assets(&self, value: &Value) -> Result<(), Error> {
        match value {
            Value::Media(asset) => {
                let bytes = self.get(&asset.hash).ok_or_else(|| crate::invalid("project references a missing asset"))?;
                #[cfg(any(test, feature = "validation-metrics"))]
                crate::validation_metrics::record(|metrics| metrics.sniffs = metrics.sniffs.saturating_add(1));
                let Some(sniffed) = nodecraft_media::sniff(bytes) else {
                    return Ok(());
                };
                let expected = asset.mime.essence();
                let actual = sniffed.mime.split(';').next().unwrap_or_default();
                let generic_text = actual == "text/plain"
                    && (expected.starts_with("text/") || expected == "application/json" || expected.ends_with("+json") || expected.ends_with("+xml"));
                if actual != expected && !generic_text {
                    return Err(crate::invalid("asset MIME does not match its stored bytes"));
                }
                if matches!(actual, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff")
                    && (asset.meta.width.is_some() || asset.meta.height.is_some())
                {
                    #[cfg(any(test, feature = "validation-metrics"))]
                    crate::validation_metrics::record(|metrics| metrics.image_probes = metrics.image_probes.saturating_add(1));
                    let meta = nodecraft_media::probe_image(bytes, &nodecraft_media::Limits::default())
                        .map_err(|_| crate::invalid("cannot verify asset image dimensions"))?;
                    if asset.meta.width.is_some_and(|width| width != meta.width) || asset.meta.height.is_some_and(|height| height != meta.height) {
                        return Err(crate::invalid("asset dimensions do not match its stored bytes"));
                    }
                }
            }
            Value::List(values) => {
                for value in values {
                    self.validate_value_assets(value)?;
                }
            }
            Value::Text(_) | Value::Bool(_) | Value::Number(_) | Value::Integer(_) | Value::Unsigned(_) => {}
        }
        Ok(())
    }

    fn validate_node_assets(&self, node: &nodecraft_graph::Node) -> Result<(), Error> {
        #[cfg(any(test, feature = "validation-metrics"))]
        crate::validation_metrics::record(|metrics| metrics.asset_node_visits = metrics.asset_node_visits.saturating_add(1));
        #[cfg(test)]
        ASSET_NODE_VISITS.with(|visits| visits.set(visits.get().saturating_add(1)));
        for value in node.params.values().chain(node.config.values()).chain(node.history.iter().flat_map(|generation| &generation.outputs)) {
            self.validate_value_assets(value)?;
        }
        Ok(())
    }

    /// Partial validation: the caller must have validated the root and staged/applied
    /// these changes through the graph. This is not admission for an arbitrary project.
    /// Recovery has validated the checkpoint, and `changes.apply` has validated this
    /// frame's final graph. Immutable asset bytes and unchanged node references retain
    /// that proof. Check surviving nodes whose asset-bearing data could have changed;
    /// do this after EVERY frame, so a later removal cannot hide an invalid reference.
    pub(crate) fn validate_changed_assets(&self, project: &Project, changes: &nodecraft_graph::Changeset) -> Result<(), Error> {
        use nodecraft_graph::Change;
        let mut touched = BTreeMap::<nodecraft_types::CanvasId, BTreeSet<nodecraft_types::NodeId>>::new();
        for change in &changes.changes {
            match change {
                Change::AddCanvas { canvas } => touched.entry(canvas.id()).or_default().extend(canvas.nodes().keys().copied()),
                Change::AddNode { canvas, node } | Change::SetNode { canvas, node } | Change::RestoreNode { canvas, node, .. } => {
                    touched.entry(*canvas).or_default().insert(node.id);
                }
                Change::SetParam { canvas, id, .. } | Change::AddGeneration { canvas, id, .. } => {
                    touched.entry(*canvas).or_default().insert(*id);
                }
                Change::SetProjectTitle { .. }
                | Change::RemoveCanvas { .. }
                | Change::SetCanvasTitle { .. }
                | Change::RemoveNode { .. }
                | Change::MoveNode { .. }
                | Change::SetNodeTitle { .. }
                | Change::RemoveGeneration { .. }
                | Change::SetActiveGeneration { .. }
                | Change::Connect { .. }
                | Change::Disconnect { .. }
                | Change::SetEdge { .. }
                | Change::PutField { .. }
                | Change::RemoveField { .. }
                | Change::PutTag { .. }
                | Change::RemoveTag { .. }
                | Change::PutComment { .. }
                | Change::RemoveComment { .. } => {}
            }
        }
        for (id, nodes) in touched {
            if let Some(canvas) = project.canvases().get(&id) {
                for id in nodes {
                    if let Some(node) = canvas.nodes().get(&id) {
                        self.validate_node_assets(node)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Validates the graph first, which bounds all list traversal including generation history.
    pub fn validate_project(&self, project: &Project) -> Result<BTreeSet<Blake3>, Error> {
        #[cfg(any(test, feature = "validation-metrics"))]
        crate::validation_metrics::record(|metrics| metrics.full_asset_checks = metrics.full_asset_checks.saturating_add(1));
        project.validate()?;
        let mut hashes = BTreeSet::new();
        for canvas in project.canvases().values() {
            for node in canvas.nodes().values() {
                self.validate_node_assets(node)?;
                collect_node(node, &mut hashes);
            }
        }
        if hashes.iter().any(|hash| !self.assets.contains_key(hash)) {
            return Err(crate::invalid("project references a missing asset"));
        }
        Ok(hashes)
    }
}

// Project::validate has already bounded depth to 32 and items per value to 4096.
fn collect(value: &Value, hashes: &mut BTreeSet<Blake3>) {
    match value {
        Value::Media(asset) => {
            hashes.insert(asset.hash);
        }
        Value::List(values) => {
            for value in values {
                collect(value, hashes);
            }
        }
        _ => {}
    }
}

pub(crate) fn project_hashes(project: &Project) -> BTreeSet<Blake3> {
    let mut hashes = BTreeSet::new();
    for canvas in project.canvases().values() {
        for node in canvas.nodes().values() {
            collect_node(node, &mut hashes);
        }
    }
    hashes
}
fn collect_node(node: &nodecraft_graph::Node, hashes: &mut BTreeSet<Blake3>) {
    for value in node.params.values().chain(node.config.values()).chain(node.history.iter().flat_map(|generation| &generation.outputs)) {
        collect(value, hashes);
    }
}
/// References retained in bounded undo/redo payloads. Does not require their target entities to
/// exist in the current document: removed nodes/generations must remain restorable.
pub fn assets_in_changes(changes: &nodecraft_graph::Changeset) -> Result<BTreeSet<Blake3>, Error> {
    use nodecraft_graph::Change;
    changes.validate_payload()?;
    let mut hashes = BTreeSet::new();
    for change in &changes.changes {
        match change {
            Change::AddCanvas { canvas } => {
                for node in canvas.nodes().values() {
                    collect_node(node, &mut hashes);
                }
            }
            Change::AddNode { node, .. } | Change::SetNode { node, .. } | Change::RestoreNode { node, .. } => collect_node(node, &mut hashes),
            Change::AddGeneration { generation, .. } => {
                for value in &generation.outputs {
                    collect(value, &mut hashes);
                }
            }
            Change::SetParam { value: Some(value), .. } => collect(value, &mut hashes),
            _ => {}
        }
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Changeset, Node};
    use nodecraft_types::{AssetRef, CanvasId, MediaMeta, NodeId, ProjectId};

    #[test]
    fn incremental_validation_covers_config_and_preserves_valid_final_batch_semantics() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Fixture").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(id, "source.text");
        node.config.insert(
            "asset".into(),
            Value::List(vec![Value::Media(AssetRef { hash: Blake3::from_bytes([9; 32]), mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() })]),
        );
        let changes = Changeset::new(vec![Change::AddNode { canvas, node }]);
        changes.apply(&mut project).unwrap();
        let assets = AssetStore::default();
        assert!(assets.validate_changed_assets(&project, &changes).is_err());
        // A final replacement in the same atomic batch is permitted just as with full
        // validation. A distinct invalid replay frame cannot be repaired by a later one.
        let final_changes = Changeset::new(vec![Change::SetNode { canvas, node: Node::new(id, "source.text") }]);
        final_changes.apply(&mut project).unwrap();
        assert!(assets.validate_changed_assets(&project, &changes).is_ok());
    }
}
