use nodecraft_graph::Project;
use nodecraft_types::{Blake3, Error, Value};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
thread_local! {
    pub(crate) static ASSET_NODE_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub const MAX_ASSETS: usize = 1024;
pub const MAX_ASSET_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TOTAL_ASSET_BYTES: usize = 48 * 1024 * 1024;

pub use crate::asset_store::AssetStore;

impl AssetStore {
    fn validate_value_assets(&self, value: &Value) -> Result<(), Error> {
        match value {
            Value::Media(asset) => {
                let bytes = match self.get(&asset.hash) {
                    Some(bytes) => bytes,
                    None => {
                        self.open_verified(asset, &crate::asset_source::TransferCancel::default(), &crate::asset_source::StreamMetrics::default())?;
                        return Ok(());
                    }
                };
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
        self.validate_changed(project, changes, true)
    }
    pub(crate) fn validate_changed_presence(&self, project: &Project, changes: &nodecraft_graph::Changeset) -> Result<(), Error> {
        self.validate_changed(project, changes, false)
    }
    /// Partial content validation after a caller has staged/validated the final graph.
    /// This checks surviving touched references, not an arbitrary root or wire format.
    pub fn verify_changed_references(
        &self,
        project: &Project,
        changes: &nodecraft_graph::Changeset,
        cancel: &crate::asset_source::TransferCancel,
        metrics: &crate::asset_source::StreamMetrics,
    ) -> Result<(), Error> {
        fn verify(
            store: &AssetStore,
            value: &Value,
            cancel: &crate::asset_source::TransferCancel,
            metrics: &crate::asset_source::StreamMetrics,
        ) -> Result<(), Error> {
            cancel.check()?;
            match value {
                Value::Media(asset) => {
                    store.open_verified(asset, cancel, metrics)?;
                }
                Value::List(values) => {
                    for value in values {
                        verify(store, value, cancel, metrics)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        self.validate_changed_presence(project, changes)?;
        self.visit_changed(project, changes, |node| {
            for value in node.params.values().chain(node.config.values()).chain(node.history.iter().flat_map(|generation| &generation.outputs)) {
                verify(self, value, cancel, metrics)?;
            }
            Ok(())
        })
    }
    fn validate_changed(&self, project: &Project, changes: &nodecraft_graph::Changeset, verify: bool) -> Result<(), Error> {
        self.visit_changed(project, changes, |node| if verify { self.validate_node_assets(node) } else { self.validate_node_presence(node) })
    }
    fn visit_changed(
        &self,
        project: &Project,
        changes: &nodecraft_graph::Changeset,
        mut visit: impl FnMut(&nodecraft_graph::Node) -> Result<(), Error>,
    ) -> Result<(), Error> {
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
                        visit(node)?;
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn verify_value_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        item: &Value,
        cancel: &crate::asset_source::TransferCancel,
        metrics: &crate::asset_source::StreamMetrics,
    ) -> Result<(), Error> {
        cancel.check()?;
        match item {
            Value::Media(asset) => self.verify_reference_in_stage(stage, asset, cancel, metrics),
            Value::List(items) => {
                for item in items {
                    self.verify_value_in_stage(stage, item, cancel, metrics)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn verify_node_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        node: &nodecraft_graph::Node,
        cancel: &crate::asset_source::TransferCancel,
        metrics: &crate::asset_source::StreamMetrics,
    ) -> Result<(), Error> {
        for item in node.params.values().chain(node.config.values()).chain(node.history.iter().flat_map(|generation| &generation.outputs)) {
            self.verify_value_in_stage(stage, item, cancel, metrics)?;
        }
        Ok(())
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn verify_changed_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        project: &Project,
        changes: &nodecraft_graph::Changeset,
        cancel: &crate::asset_source::TransferCancel,
        metrics: &crate::asset_source::StreamMetrics,
    ) -> Result<(), Error> {
        self.validate_changed_presence(project, changes)?;
        self.visit_changed(project, changes, |node| self.verify_node_in_stage(stage, node, cancel, metrics))
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn verify_project_in_stage(
        &self,
        stage: &crate::disk_quota::StageOperation<'_>,
        project: &Project,
        cancel: &crate::asset_source::TransferCancel,
        metrics: &crate::asset_source::StreamMetrics,
    ) -> Result<(), Error> {
        self.validate_project_presence(project)?;
        for canvas in project.canvases().values() {
            for node in canvas.nodes().values() {
                self.verify_node_in_stage(stage, node, cancel, metrics)?;
            }
        }
        Ok(())
    }
    fn validate_node_presence(&self, node: &nodecraft_graph::Node) -> Result<(), Error> {
        let mut hashes = BTreeSet::new();
        collect_node(node, &mut hashes);
        if hashes.iter().any(|hash| !self.contains(hash)) {
            return Err(crate::invalid("project references a missing asset"));
        }
        Ok(())
    }
    /// Graph and reference presence only. Does not claim hash, MIME or image validation.
    pub fn validate_project_presence(&self, project: &Project) -> Result<BTreeSet<Blake3>, Error> {
        project.validate()?;
        let hashes = project_hashes(project);
        if hashes.iter().any(|hash| !self.contains(hash)) {
            return Err(crate::invalid("project references a missing asset"));
        }
        Ok(hashes)
    }
    /// Native publication validation with explicit transfer ownership and cancellation.
    /// Presence-only open/preview must not invoke this content-consuming operation.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn verify_project_streams(
        &self,
        project: &Project,
        cancel: &crate::asset_source::TransferCancel,
        metrics: &crate::asset_source::StreamMetrics,
    ) -> Result<BTreeSet<Blake3>, Error> {
        fn verify(
            store: &AssetStore,
            value: &Value,
            cancel: &crate::asset_source::TransferCancel,
            metrics: &crate::asset_source::StreamMetrics,
        ) -> Result<(), Error> {
            cancel.check()?;
            match value {
                Value::Media(asset) => {
                    store.open_verified(asset, cancel, metrics)?;
                }
                Value::List(values) => {
                    for value in values {
                        verify(store, value, cancel, metrics)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        let hashes = self.validate_project_presence(project)?;
        for canvas in project.canvases().values() {
            for node in canvas.nodes().values() {
                for value in node.params.values().chain(node.config.values()).chain(node.history.iter().flat_map(|generation| &generation.outputs)) {
                    verify(self, value, cancel, metrics)?;
                }
            }
        }
        Ok(hashes)
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
        if hashes.iter().any(|hash| !self.contains(hash)) {
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
