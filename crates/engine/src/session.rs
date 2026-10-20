use std::{
    collections::VecDeque,
    io::Write,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use nodecraft_format::{AssetStore, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Change, Changeset, Project, StagedChanges};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::Value;

use crate::commands::{Context, Effect, Phase, Registry, Resources, Services, invalid, validate_input_json, validate_json};

/// One completed actual graph stage, with no document/error ownership.
struct StageObservation {
    start: u64,
    end: u64,
    refused: bool,
}
impl StageObservation {
    fn record(self, recorder: &mut crate::perf::Recorder) {
        let metric = if self.refused { crate::perf::Metric::GraphStageRefused } else { crate::perf::Metric::GraphStage };
        recorder.record_between(metric, self.start, self.end);
    }
}

/// Step ceiling, subject to the byte budget below. The UI regression proves a 200-node graph
/// built with 599 create/edit/move steps undoes to its original document (#65).
pub const MAX_HISTORY_STEPS: usize = 4096;
pub const MAX_HISTORY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_EVENTS: usize = 256;

mod imports;
pub use imports::PreparedImports;
mod file_lifecycle;
pub use file_lifecycle::DocumentLineage;

/// Notification log entry. Each consumer keeps its own revision cursor and reads the current
/// document after a notification. A lagging consumer receives a resync error, not partial history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocChanged {
    pub revision: u64,
    pub command: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MoveKey {
    canvas: CanvasId,
    node: NodeId,
    gesture: String,
}

struct Entry {
    inverse: Changeset,
    bytes: usize,
    state: u64,
}

#[derive(Default)]
struct History {
    entries: VecDeque<Entry>,
    bytes: usize,
}

impl History {
    fn push(&mut self, entry: Entry) {
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.entries.push_back(entry);
        while self.entries.len() > MAX_HISTORY_STEPS || self.bytes > MAX_HISTORY_BYTES {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.bytes);
            } else {
                break;
            }
        }
    }
    fn pop(&mut self) {
        if let Some(old) = self.entries.pop_back() {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
    }
}

enum Direction {
    Edit,
    Undo,
    Redo,
    /// A derived result (#88): replaces the top undo entry with one that also undoes it.
    Amend,
}
struct Prepared {
    document: Project,
    inverse: Entry,
    direction: Direction,
    gesture: Option<MoveKey>,
    result: Value,
    admission: Option<PersistentAdmission>,
    source: SourceStamp,
    state: u64,
    next_execution_generation: u64,
    assets: AssetStore,
    next_identity: Arc<()>,
}
/// Borrowed authority for ordinary staging or Cut's private prospective document.
struct PreparationSource<'a> {
    document: &'a Project,
    revision: u64,
    state: u64,
    execution: &'a crate::execution_identity::ExecutionSource,
    assets: &'a AssetStore,
    identity: &'a Arc<()>,
}
struct SourceStamp {
    identity: Arc<()>,
    revision: u64,
}
impl SourceStamp {
    fn matches(&self, session: &Session) -> bool {
        self.revision == session.revision && Arc::ptr_eq(&self.identity, &session.admission_identity)
    }
}
/// Only Session::prepare can construct this private document-admission capability.
/// The owning Prepared bundle pairs it with the exact graph-staged prospective document.
pub(crate) struct PersistentAdmission {
    replay: nodecraft_format::journal::AdmittedReplay,
}
impl PersistentAdmission {
    pub(crate) fn into_replay(self) -> nodecraft_format::journal::AdmittedReplay {
        self.replay
    }
}

/// A background save's session-side completion: the assets it retains and its execution step.
struct PendingSave {
    roots: std::collections::BTreeSet<nodecraft_types::Blake3>,
    next: u64,
}
/// A background open's session-side completion: the revision and authority it publishes.
struct PendingOpen {
    revision: u64,
    next: u64,
    identity: Arc<()>,
}
enum PendingFile {
    Save(PendingSave),
    Open(PendingOpen),
}
fn opening() -> Error {
    Error::new(ErrorKind::Busy, "a project is still opening; wait for it, or cancel it with file.cancel")
}

/// Owns the document and is its only command mutation path. Read access is immutable.
/// The panic guard stages edits before publication; native unwinding preserves the live state.
/// Abort-only targets (including the default wasm target) still rely on fallible validation.
pub struct Session {
    view_identity: std::sync::Arc<()>,
    file_lifecycle: file_lifecycle::FileLifecycle,
    routing: crate::provider_routing::RoutingAuthority,
    document: Project,
    registry: Registry,
    kinds: nodecraft_nodes::Registry,
    resources: Resources,
    services: Services,
    undo: History,
    redo: History,
    gesture: Option<MoveKey>,
    revision: u64,
    events: VecDeque<DocChanged>,
    state: u64,
    assets: AssetStore,
    persistence: Option<crate::persistence::Persistence>,
    save_runner: Option<Arc<dyn crate::save_runner::SaveRunner>>,
    /// A background save or open still running: what to finish once it has.
    pending: Option<PendingFile>,
    /// The outcome of a background save that an edit waited for, until the host polls it.
    settled: Option<Result<Value, Error>>,
    status_revision: u64,
    admission_identity: Arc<()>,
    #[cfg(test)]
    replay_limit: usize,
    execution: crate::execution_identity::ExecutionSource,
    perf: crate::perf::Recorder,
}

impl Session {
    pub fn new(document: Project) -> Result<Self, Error> {
        Self::with_registry(document, Registry::builtins()?)
    }

    pub fn with_registry(document: Project, registry: Registry) -> Result<Self, Error> {
        let (kinds, generation) = crate::generation_catalog::default_registry()?;
        let mut session = Self::with_registries(document, registry, kinds)?;
        generation.install(&mut session);
        Ok(session)
    }

    pub fn with_registries(document: Project, registry: Registry, kinds: nodecraft_nodes::Registry) -> Result<Self, Error> {
        document.validate()?;
        let execution = crate::execution_identity::ExecutionSource::new()?;
        Ok(Self::from_validated(document, registry, kinds, execution))
    }
    fn from_validated(document: Project, registry: Registry, kinds: nodecraft_nodes::Registry, execution: crate::execution_identity::ExecutionSource) -> Self {
        Self::from_validated_identity(document, registry, kinds, execution, Arc::new(()))
    }
    fn from_validated_identity(
        document: Project,
        registry: Registry,
        kinds: nodecraft_nodes::Registry,
        execution: crate::execution_identity::ExecutionSource,
        identity: Arc<()>,
    ) -> Self {
        Self {
            view_identity: std::sync::Arc::new(()),
            file_lifecycle: file_lifecycle::FileLifecycle::new(),
            document,
            registry,
            kinds,
            resources: Resources::default(),
            routing: Default::default(),
            services: Services::default(),
            undo: History::default(),
            redo: History::default(),
            gesture: None,
            revision: 0,
            events: VecDeque::new(),
            state: 0,
            assets: AssetStore::default(),
            persistence: None,
            save_runner: None,
            pending: None,
            settled: None,
            status_revision: 0,
            admission_identity: identity,
            #[cfg(test)]
            replay_limit: MAX_HISTORY_BYTES,
            execution,
            perf: crate::perf::Recorder::default(),
        }
    }

    /// The performance recorder (#151): off until `perf.start`; the host gives it a clock.
    pub fn perf(&self) -> &crate::perf::Recorder {
        &self.perf
    }
    pub fn perf_mut(&mut self) -> &mut crate::perf::Recorder {
        &mut self.perf
    }

    /// Non-persistent host identity; retained tokens prevent address reuse across Session replacement.
    pub fn view_identity(&self) -> std::sync::Arc<()> {
        std::sync::Arc::clone(&self.view_identity)
    }

    pub fn document(&self) -> &Project {
        &self.document
    }
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
    pub fn kinds(&self) -> &nodecraft_nodes::Registry {
        &self.kinds
    }
    #[cfg(test)]
    pub(crate) fn exhaust_execution_generation(&mut self) {
        self.execution.exhaust();
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.entries.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.entries.is_empty()
    }

    /// Install endpoint authority atomically. Generic resources cannot change this nonce.
    pub fn set_provider_routing(&mut self, routing: crate::provider_routing::ProviderRouting) -> Result<(), Error> {
        let nonce = crate::execution_identity::nonce()?;
        self.routing = crate::provider_routing::RoutingAuthority { routing, nonce: Some(nonce) };
        Ok(())
    }

    /// Borrow the current paired generation definitions. UI draft stamps must retain
    /// generation_catalog_stamp in addition to Session identity and document revision.
    pub fn generation_catalog(&self) -> Result<&crate::generation_catalog::GenerationCatalog, Error> {
        crate::generation_catalog::GenerationCatalog::current(&self.resources)
    }
    pub fn generation_catalog_stamp(&self) -> Result<crate::generation_catalog::GenerationCatalogStamp, Error> {
        Ok(self.generation_catalog()?.stamp())
    }

    /// Replace a host-owned capability without discarding other injected resources.
    /// A host may scope a live view snapshot to one enabled/dispatch call and restore it after.
    pub fn swap_resource<T: std::any::Any>(&mut self, value: Option<T>) -> Option<T> {
        self.resources.swap(value)
    }
    /// Add a host callback without replacing services supplied by another frontend.
    pub fn register_host_service(&mut self, id: &'static str, handler: impl FnMut(&Value) -> Result<Value, Error> + 'static) -> Result<(), Error> {
        self.services.register(id, handler)
    }
    /// Receives/enqueues typed work. Authorize outside dispatch before scheduling.
    pub fn register_run_host(&mut self, handler: impl FnMut(crate::run_cmds::PreparedRun) -> Result<Value, Error> + 'static) -> Result<(), Error> {
        self.services.register_run_host(handler)
    }
    pub fn authorize_prepared_run(
        &self,
        prepared: crate::run_cmds::PreparedRun,
        options: nodecraft_exec::run::RunOptions,
    ) -> Result<(crate::run_cmds::AuthorizedRun, nodecraft_exec::run::Progress), Error> {
        guard(|| crate::run_cmds::authorize(&self.context(Phase::Dispatch), prepared, options))
    }
    pub fn with_storage(snapshot: Snapshot, storage: Box<dyn ProjectStorage>) -> Result<Self, Error> {
        Self::with_store(snapshot, crate::persistence::Store::Local(storage))
    }
    /// [`Self::with_storage`] over a storage a [`crate::save_runner::SaveRunner`] can write
    /// through on another thread, so `file.save` with `background` keeps the UI responsive.
    pub fn with_sendable_storage(snapshot: Snapshot, storage: Box<dyn ProjectStorage + Send>) -> Result<Self, Error> {
        Self::with_store(snapshot, crate::persistence::Store::Sendable(storage))
    }
    fn with_store(snapshot: Snapshot, storage: crate::persistence::Store) -> Result<Self, Error> {
        // All constructor failures, including entropy, precede durable checkpointing.
        snapshot.project.validate()?;
        let registry = Registry::builtins()?;
        let (kinds, generation) = crate::generation_catalog::default_registry()?;
        let execution = crate::execution_identity::ExecutionSource::new()?;
        let identity = Arc::new(());
        let (persistence, snapshot) = crate::persistence::Persistence::new(storage, snapshot)?;
        let mut session = Self::from_validated_identity(snapshot.project, registry, kinds, execution, identity);
        generation.install(&mut session);
        session.assets = snapshot.assets;
        session.persistence = Some(persistence);
        Ok(session)
    }
    pub fn assets(&self) -> &AssetStore {
        &self.assets
    }
    /// Lets `file.save`, `file.save_as` and `file.open` with `background` run on `runner` (#461,
    /// #367).
    pub fn set_save_runner(&mut self, runner: Arc<dyn crate::save_runner::SaveRunner>) {
        self.save_runner = Some(runner);
    }
    /// The latest background save's or open's outcome once it has finished, the same value
    /// `file.save` or `file.open` returns inline; `None` while it is still running or when
    /// there was none.
    pub fn poll_file(&mut self) -> Option<Result<Value, Error>> {
        self.latest_file(false)
    }
    /// [`Self::poll_file`], waiting for a background operation to finish (before quitting, say).
    pub fn finish_file(&mut self) -> Option<Result<Value, Error>> {
        self.latest_file(true)
    }
    fn latest_file(&mut self, wait: bool) -> Option<Result<Value, Error>> {
        // An open settling now supersedes an earlier save's stashed outcome: the open needed
        // `discard_dirty` to start, so that save's result no longer describes the project.
        let settled = self.settled.take();
        self.settle_file(wait).or(settled)
    }
    fn settle_file(&mut self, wait: bool) -> Option<Result<Value, Error>> {
        let before = self.file_status();
        let outcome = self.persistence.as_mut()?.settle(wait)?;
        let pending = self.pending.take();
        let result = outcome.and_then(|settled| match (settled, pending) {
            (crate::persistence::Settled::Saved(ready), pending) => {
                if let Some(PendingFile::Save(pending)) = pending {
                    self.saved(pending);
                }
                Ok(serde_json::json!({"saved":true,"recovery_ready":ready,"read_only":self.file_status().read_only}))
            }
            (crate::persistence::Settled::Opened(snapshot), Some(PendingFile::Open(pending))) => Ok(self.install(snapshot, pending)),
            (crate::persistence::Settled::Opened(_), _) => Err(Error::new(ErrorKind::Internal, "an open finished without the session waiting for it")),
        });
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        Some(result)
    }
    /// Every change to the document, its assets or its storage first waits for a background
    /// save, so the save publishes exactly the state it snapshotted. Keeps the outcome for
    /// [`Self::poll_file`]; a later save's outcome replaces an earlier one, since it describes
    /// the file as it now is. During a background open it refuses instead: the document is
    /// about to be replaced, so the change would be lost or land on the wrong project.
    pub(crate) fn settle_for_edit(&mut self) -> Result<(), Error> {
        match self.pending {
            None => Ok(()),
            Some(PendingFile::Open(_)) => Err(opening()),
            Some(PendingFile::Save(_)) => {
                match self.settle_file(true) {
                    Some(outcome) => self.settled = Some(outcome),
                    // Nothing was pending after all: never wait (or re-dispatch) for it again.
                    None => self.pending = None,
                }
                Ok(())
            }
        }
    }
    /// Publishes an opened project: the new document, fresh history and the open's authority.
    fn install(&mut self, snapshot: Snapshot, pending: PendingOpen) -> Value {
        self.document = snapshot.project;
        self.assets = snapshot.assets;
        self.undo = History::default();
        self.redo = History::default();
        self.gesture = None;
        self.state = pending.revision;
        // Adoption, not dispatch, replaces the lineage: a background open may still fail.
        self.file_lifecycle.replaced(Arc::clone(&pending.identity));
        self.publish(pending.revision, "file.open", pending.next, pending.identity);
        serde_json::json!({"opened":true,"read_only":self.file_status().read_only})
    }
    /// A save's successful completion, inline or settled from the background (#450): only now
    /// may the host treat this document's recovery copy as superseded.
    fn saved(&mut self, pending: PendingSave) {
        self.file_lifecycle.saved();
        self.assets.retain(&pending.roots);
        self.admission_identity = Arc::new(());
        self.execution.advance(pending.next);
        self.gesture = None;
    }
    pub fn validate_asset(&self, asset: &nodecraft_types::AssetRef) -> Result<(), Error> {
        self.assets.validate_reference(asset)
    }
    pub fn import_asset(&mut self, bytes: &[u8]) -> Result<nodecraft_types::Blake3, Error> {
        self.settle_for_edit()?;
        self.import_small(bytes)
    }
    /// Host housekeeping: remove logical assets absent from the document and all undo/redo
    /// entries. This does not change document history, dirty state or the recovery ledger.
    /// A true result invalidates prepared source authority, but does not promise immediate
    /// physical reclamation: readers and preparations retain their pins. Ordinary source Drop
    /// may perform owned cleanup and retain debt; no explicit fallible step follows publication.
    pub fn collect_assets(&mut self) -> Result<bool, Error> {
        self.settle_for_edit()?;
        guard(|| {
            let roots = self.retained_assets()?;
            let changed = self.assets.hashes().any(|hash| !roots.contains(hash));
            let authority = if changed { Some((self.execution.next()?, Arc::new(()))) } else { None };
            // Keep the complete live map until all explicit fallible maintenance succeeds.
            self.assets.collect_garbage()?;
            if let Some((next, identity)) = authority {
                self.assets.retain(&roots);
                self.admission_identity = identity;
                self.execution.advance(next);
            }
            Ok(changed)
        })
    }
    /// Compatibility name for explicit logical housekeeping, not import rollback.
    /// See `collect_assets`: unchanged maps preserve authority and pinned bytes remain charged.
    pub fn prune_assets(&mut self) -> Result<(), Error> {
        self.collect_assets().map(|_| ())
    }
    pub fn file_status(&self) -> crate::file_cmds::FileStatus {
        self.persistence.as_ref().map(|p| p.status(self.state)).unwrap_or(crate::file_cmds::FileStatus { dirty: true, ..Default::default() })
    }
    /// Host observation of this adopted document; not inferred from ids, paths or revisions.
    pub fn document_lineage(&self) -> DocumentLineage {
        self.file_lifecycle.current()
    }
    pub fn is_current_document(&self, lineage: &DocumentLineage) -> bool {
        self.file_lifecycle.is_current(lineage)
    }
    /// True only after this document's destination was successfully saved. A later failed
    /// recovery checkpoint does not undo that save. Replacing the document invalidates it.
    pub fn has_saved_document(&self, lineage: &DocumentLineage) -> bool {
        self.file_lifecycle.has_saved(lineage)
    }
    pub fn status_revision(&self) -> u64 {
        self.status_revision
    }
    /// Host lifecycle boundary (#408): replaces the document with work recovered from an earlier
    /// session's journal, checkpointing it at once. It is unsaved at `location` (the earlier
    /// session's file, or untitled), history starts fresh, and the change is published like an
    /// open (`file.recover`, a synthetic lifecycle event id, not a registered command).
    /// Refused once this session has edits of its own: they'd be lost.
    pub fn recover(&mut self, mut snapshot: Snapshot, location: Option<StorageLocation>) -> Result<(), Error> {
        self.settle_for_edit()?;
        if self.can_undo() || self.can_redo() {
            return Err(Error::new(ErrorKind::Conflict, "recovering would discard this session's changes"));
        }
        if !self.file_status().available {
            return Err(Error::new(ErrorKind::Unsupported, "project storage is not available"));
        }
        snapshot.project.validate()?;
        snapshot.assets = snapshot.assets.in_context(
            self.assets.context().clone(),
            &nodecraft_format::asset_source::TransferCancel::default(),
            &nodecraft_format::asset_source::StreamMetrics::default(),
        )?;
        let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
        let next = self.execution.next()?;
        let identity = Arc::new(());
        self.reserve_event()?;
        let before = self.file_status();
        let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?;
        storage.adopt_recovered(&snapshot, location)?;
        self.document = snapshot.project;
        self.assets = snapshot.assets;
        self.undo = History::default();
        self.redo = History::default();
        self.gesture = None;
        self.state = revision;
        self.file_lifecycle.replaced(Arc::clone(&identity));
        self.publish(revision, "file.recover", next, identity);
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        Ok(())
    }
    /// Host boundary for live recompute (#88): records results derived from the document as it
    /// is at `base` (the revision the results were computed from), importing the images they
    /// name from `staged`. The record joins the undo step of the edit that made the results
    /// stale, so one undo restores that edit's old values and old results together, and a
    /// result never becomes an undo step of its own. Refused (`Conflict`, nothing changed) when
    /// the document has moved on since `base`, or while there is something to redo.
    pub fn record_derived(
        &mut self,
        canvas: CanvasId,
        results: Vec<(NodeId, nodecraft_graph::Generation)>,
        staged: &AssetStore,
        base: u64,
    ) -> Result<(), Error> {
        self.record_derived_with(canvas, results, &[], staged, base)
    }

    /// [`Self::record_derived`], also applying cached results (`hits`, #91) in the same step.
    pub fn record_derived_with(
        &mut self,
        canvas: CanvasId,
        results: Vec<(NodeId, nodecraft_graph::Generation)>,
        hits: &[crate::cache::Hit],
        staged: &AssetStore,
        base: u64,
    ) -> Result<(), Error> {
        self.settle_for_edit()?;
        if self.revision != base {
            return Err(Error::new(ErrorKind::Conflict, "the results are out of date"));
        }
        if !self.redo.entries.is_empty() {
            return Err(Error::new(ErrorKind::Conflict, "derived results wait until there is nothing to redo"));
        }
        let count = results.len().saturating_add(hits.len());
        if count == 0 || count > crate::run_cmds::MAX_RESULTS {
            return Err(invalid("record 1 to 1000 derived results"));
        }
        // Validate every result before touching the assets.
        let live = self.document.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
        let mut changes = crate::cache::changes(canvas, live, hits)?;
        let mut seen: std::collections::BTreeSet<NodeId> = hits.iter().map(crate::cache::Hit::node).collect();
        if seen.len() != hits.len() {
            return Err(invalid("a node has two derived results"));
        }
        for (node, generation) in &results {
            if !seen.insert(*node) {
                return Err(invalid("a node has two derived results"));
            }
            let current = live.nodes().get(node).ok_or_else(|| Error::new(ErrorKind::NotFound, "a derived result's node is gone"))?;
            changes.extend(nodecraft_exec::run::record_changes(canvas, current, generation.clone())?);
        }
        let mut needed = Vec::new();
        for (_, generation) in &results {
            media(&generation.outputs, &mut needed);
        }
        for hash in needed {
            if !self.assets.contains(&hash) {
                let bytes = staged.get(&hash).ok_or_else(|| Error::new(ErrorKind::NotFound, "a derived result names an image that wasn't staged"))?.to_vec();
                self.import_asset(&bytes)?;
            }
        }
        let mut prepared = guard(|| self.prepare(Effect::Edit { changes: Changeset::new(changes), result: Value::Null, gesture: None }))?;
        // One undo entry: first undo the result, then the edit below it (if any).
        // When the combined step would exceed the history or replay budgets, the results get a
        // step of their own instead: never evict history or make an undo unreplayable.
        if let Some(below) = self.undo.entries.back() {
            let mut combined = prepared.inverse.inverse.changes.clone();
            combined.extend(below.inverse.changes.iter().cloned());
            let inverse = Changeset::new(combined);
            let fits = inverse.validate_replay_budget(MAX_HISTORY_BYTES.saturating_sub(12)).is_ok();
            if let (true, Ok(bytes)) = (fits, history_bytes(&inverse)) {
                let total = self.undo.bytes.saturating_sub(below.bytes).saturating_add(bytes);
                if total <= MAX_HISTORY_BYTES {
                    prepared.inverse = Entry { inverse, bytes, state: below.state };
                    prepared.direction = Direction::Amend;
                }
            }
        }
        self.commit_prepared(prepared, "run.derived").map(|_| ())
    }
    /// Host lifecycle boundary: flush and discard recovery after a clean close or explicit discard.
    pub fn close_recovery(&mut self, discard_dirty: bool) -> Result<(), Error> {
        self.settle_for_edit()?;
        let before = self.file_status();
        let result =
            self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?.close(self.state, discard_dirty);
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        result
    }
    fn retained_assets(&self) -> Result<std::collections::BTreeSet<nodecraft_types::Blake3>, Error> {
        let mut roots = self.assets.validate_project_presence(&self.document)?;
        for entry in self.undo.entries.iter().chain(&self.redo.entries) {
            roots.extend(nodecraft_format::assets_in_changes(&entry.inverse)?);
        }
        if roots.iter().any(|hash| !self.assets.contains(hash)) {
            return Err(Error::new(ErrorKind::InvalidInput, "history references a missing retained asset"));
        }
        Ok(roots)
    }

    pub fn with_resources(mut self, resources: Resources) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_services(mut self, services: Services) -> Self {
        self.services = services;
        self
    }

    pub(crate) fn context(&self, phase: Phase) -> Context<'_> {
        Context {
            assets: &self.assets,
            routing: &self.routing,
            document: &self.document,
            revision: self.revision,
            kinds: &self.kinds,
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            resources: &self.resources,
            services: &self.services,
            phase,
            file: self.file_status(),
            execution: &self.execution,
            perf: &self.perf,
        }
    }

    /// Returns an actionable reason when disabled; never mutates the session.
    pub fn enabled(&self, id: &str, params: &Value) -> Result<(), Error> {
        guard(|| {
            validate_input_json(params)?;
            let spec = self.registry.get(id)?;
            let context = self.context(Phase::Preview);
            (spec.enabled)(&context, params)?;
            // Handlers describe effects without I/O. Preview custom edits through the same
            // admission boundary as dispatch; services and file I/O are never invoked here.
            match (spec.run)(&context, params)? {
                Effect::None { .. } | Effect::Perf { .. } | Effect::Service { .. } | Effect::CancelFile => Ok(()),
                // Dispatch refuses everything else until the open settles.
                _ if matches!(self.pending, Some(PendingFile::Open(_))) => Err(opening()),
                effect @ (Effect::Edit { .. } | Effect::Undo | Effect::Redo) => self.prepare(effect).map(|_| ()),
                _ => Ok(()),
            }
        })
    }

    pub fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        let before = self.file_status();
        // Capture commands don't time themselves.
        let start = if id.starts_with("perf.") { None } else { self.perf.now() };
        let result = self.dispatch_inner(id, params);
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        // Only commands that ran count: a refusal is fast and would pull the latencies down.
        if let Some(start) = start.filter(|_| result.is_ok()) {
            self.perf.record_since(crate::perf::Metric::Dispatch, start);
            if let Some(metric) = crate::perf::Metric::of_command(id) {
                self.perf.record_since(metric, start);
            }
        }
        result
    }
    /// Cut's delete is published only after the real registered paste admits its roundtrip.
    /// Handlers/kinds retain the ordinary trusted-pure description contract; this does not
    /// sandbox interior-mutating native extensions or acknowledge OS clipboard delivery.
    pub fn dispatch_clipboard_cut(&mut self, delete: &Value, paste: &Value) -> Result<Value, Error> {
        let before = self.file_status();
        let start = self.perf.now();
        let result = guard(|| self.clipboard_cut_inner(delete, paste));
        if self.file_status() != before {
            self.status_revision = self.status_revision.saturating_add(1);
        }
        if let Some(start) = start.filter(|_| result.is_ok()) {
            self.perf.record_since(crate::perf::Metric::Dispatch, start);
            if let Some(metric) = crate::perf::Metric::of_command("canvas.delete") {
                self.perf.record_since(metric, start);
            }
        }
        result
    }
    fn describe(spec: &crate::commands::CommandSpec, context: &Context<'_>, params: &Value) -> Result<Effect, Error> {
        guard(|| {
            validate_input_json(params)?;
            (spec.enabled)(context, params)?;
            (spec.run)(context, params)
        })
    }
    fn clipboard_cut_inner(&mut self, delete: &Value, paste: &Value) -> Result<Value, Error> {
        // Like any edit, a cut first waits for a background save and refuses during an open.
        self.settle_for_edit()?;
        let delete_spec = *self.registry.get("canvas.delete")?;
        let paste_spec = *self.registry.get("canvas.paste")?;
        validate_input_json(paste)?;
        let effect = Self::describe(&delete_spec, &self.context(Phase::Dispatch), delete)?;
        if !matches!(&effect, Effect::Edit { .. }) {
            return Err(invalid("cut requires a document delete effect"));
        }
        let mut stage = None;
        let prepared = guard(|| self.prepare_observed(effect, Some(&mut stage)));
        if let Some(stage) = stage {
            stage.record(&mut self.perf);
        }
        let prepared = prepared?;
        let future_execution = self.execution.prospective(prepared.next_execution_generation);
        let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
        let mut stage = None;
        let hypothetical = guard(|| {
            let mut context = self.context(Phase::Dispatch);
            context.document = &prepared.document;
            context.assets = &prepared.assets;
            context.revision = revision;
            context.can_undo = true;
            context.can_redo = false;
            context.file.dirty = true;
            context.execution = &future_execution;
            let effect = Self::describe(&paste_spec, &context, paste)?;
            if !matches!(&effect, Effect::Edit { .. }) {
                return Err(invalid("cut roundtrip requires a document paste effect"));
            }
            // No service/file effect is executed. Drop the hypothetical graph/replay before
            // the real append, retaining the same candidate asset authority throughout.
            self.prepare_source_observed(
                effect,
                PreparationSource {
                    document: &prepared.document,
                    revision,
                    state: prepared.state,
                    execution: &future_execution,
                    assets: &prepared.assets,
                    identity: &prepared.next_identity,
                },
                false,
                Some(&mut stage),
            )
            .map(|_| ())
        });
        if let Some(stage) = stage {
            stage.record(&mut self.perf);
        }
        hypothetical?;
        self.commit_prepared(prepared, delete_spec.id)
    }
    fn dispatch_inner(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        let spec = *self.registry.get(id)?;
        let effect = Self::describe(&spec, &self.context(Phase::Dispatch), params)?;
        let reads = matches!(effect, Effect::None { .. } | Effect::Perf { .. } | Effect::Service { .. } | Effect::PreparedRun { .. } | Effect::CancelFile);
        // A run would start on the project being replaced, so only pure reads go past an open.
        if matches!(self.pending, Some(PendingFile::Open(_)))
            && !matches!(effect, Effect::None { .. } | Effect::Perf { .. } | Effect::Service { .. } | Effect::CancelFile)
        {
            return Err(opening());
        }
        if !reads && self.pending.is_some() {
            // The save's outcome can change what this command may do: decide again after it.
            self.settle_for_edit()?;
            return self.dispatch_inner(id, params);
        }
        let effect = match effect {
            Effect::None { result } => {
                validate_json(&result)?;
                return Ok(result);
            }
            Effect::PreparedRun { prepared } => {
                return guard(|| {
                    let result = self.services.invoke_run_host(*prepared)?;
                    validate_json(&result)?;
                    Ok(result)
                });
            }
            Effect::CancelFile => {
                let cancelling = self.persistence.as_ref().is_some_and(|persistence| persistence.cancel());
                if cancelling {
                    self.status_revision = self.status_revision.saturating_add(1);
                }
                return Ok(serde_json::json!({"cancelling":cancelling}));
            }
            Effect::Perf { action } => {
                match action {
                    crate::perf_cmds::PerfAction::Start => self.perf.set_enabled(true),
                    crate::perf_cmds::PerfAction::Stop => self.perf.set_enabled(false),
                    crate::perf_cmds::PerfAction::Clear => self.perf.clear(),
                }
                return Ok(crate::perf_cmds::applied(action, self.perf.enabled()));
            }
            Effect::Service { capability, request } => {
                validate_json(&request)?;
                return guard(|| {
                    let result = self.services.invoke(capability, &request)?;
                    validate_json(&result)?;
                    Ok(result)
                });
            }
            Effect::Save { location, background } => {
                let next = self.execution.next()?;
                let roots = self.retained_assets()?;
                let runner = if background { self.save_runner.clone() } else { None };
                let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?;
                let saving = storage.save(&self.document, &self.assets, &roots, self.state, location, runner.as_deref())?;
                let pending = PendingSave { roots, next };
                let read_only = self.file_status().read_only;
                return Ok(match saving {
                    crate::persistence::Started::Done(ready) => {
                        self.saved(pending);
                        serde_json::json!({"saved":true,"recovery_ready":ready,"read_only":read_only})
                    }
                    crate::persistence::Started::Started => {
                        self.pending = Some(PendingFile::Save(pending));
                        serde_json::json!({"saved":false,"saving":true,"recovery_ready":false,"read_only":read_only})
                    }
                });
            }
            Effect::Open { location, discard_dirty, background } => {
                if self.file_status().dirty && !discard_dirty {
                    return Err(Error::new(ErrorKind::Conflict, "opening would discard unsaved changes"));
                }
                let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
                let next = self.execution.next()?;
                let pending = PendingOpen { revision, next, identity: Arc::new(()) };
                self.reserve_event()?;
                let runner = if background { self.save_runner.clone() } else { None };
                let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "project storage is not configured"))?;
                return Ok(match storage.open(location, revision, runner.as_deref())? {
                    crate::persistence::Started::Done(snapshot) => self.install(snapshot, pending),
                    crate::persistence::Started::Started => {
                        self.pending = Some(PendingFile::Open(pending));
                        serde_json::json!({"opened":false,"opening":true})
                    }
                });
            }
            document_effect => document_effect,
        };
        let mut stage = None;
        let prepared = guard(|| self.prepare_observed(effect, Some(&mut stage)));
        // Preserve completed staging observations even if later admission fails. A stage
        // that unwinds cannot finish its observation and contributes no fabricated sample.
        if let Some(stage) = stage {
            stage.record(&mut self.perf);
        }
        self.commit_prepared(prepared?, spec.id)
    }
    fn commit_prepared(&mut self, mut prepared: Prepared, command: &'static str) -> Result<Value, Error> {
        if !prepared.source.matches(self) {
            return Err(Error::new(ErrorKind::Conflict, "prepared edit no longer matches the session document, assets or revision"));
        }
        let revision = self.revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?;
        // Reserve bounded publication bookkeeping before the potentially durable append.
        self.reserve_event()?;
        let history = match prepared.direction {
            Direction::Edit | Direction::Redo | Direction::Amend => &mut self.undo,
            Direction::Undo => &mut self.redo,
        };
        history.entries.try_reserve(1).map_err(|_| Error::new(ErrorKind::Internal, "cannot reserve undo history publication"))?;
        if let Some(admission) = prepared.admission.take() {
            let storage = self.persistence.as_mut().ok_or_else(|| Error::new(ErrorKind::Conflict, "prepared persistent edit requires its original storage"))?;
            if storage.capability() == nodecraft_format::StorageCapability::StreamingV2 {
                admission.replay.verify_changed_assets(
                    &prepared.document,
                    &prepared.assets,
                    &nodecraft_format::asset_source::TransferCancel::default(),
                    &storage.metrics(),
                )?;
            }
            storage.append(admission)?;
        }
        // No handler or fallible graph operation runs after this point.
        match prepared.direction {
            Direction::Edit => {
                if prepared.gesture.is_none() || prepared.gesture != self.gesture || self.undo.entries.is_empty() {
                    self.undo.push(prepared.inverse);
                }
                self.redo = History::default();
                self.gesture = prepared.gesture;
            }
            Direction::Undo => {
                self.undo.pop();
                self.redo.push(prepared.inverse);
                self.gesture = None;
            }
            Direction::Redo => {
                self.redo.pop();
                self.undo.push(prepared.inverse);
                self.gesture = None;
            }
            // The edit below keeps its gesture: a drag that a result joined stays one step.
            Direction::Amend => {
                self.undo.pop();
                self.undo.push(prepared.inverse);
            }
        }
        self.document = prepared.document;
        self.assets = prepared.assets;
        self.state = prepared.state;
        self.publish(revision, command, prepared.next_execution_generation, prepared.next_identity);
        Ok(prepared.result)
    }
    fn reserve_event(&mut self) -> Result<(), Error> {
        self.events.try_reserve(1).map_err(|_| Error::new(ErrorKind::Internal, "cannot reserve document event publication"))
    }
    fn publish(&mut self, revision: u64, command: &'static str, next_execution_generation: u64, identity: Arc<()>) {
        self.revision = revision;
        self.execution.advance(next_execution_generation);
        self.admission_identity = identity;
        self.events.push_back(DocChanged { revision, command });
        if self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    fn stage(&self, source: &Project, changes: &Changeset, observation: &mut Option<&mut Option<StageObservation>>) -> Result<StagedChanges, Error> {
        let start = observation.as_ref().and_then(|_| self.perf.now());
        let result = changes.stage_certified(source);
        let end = observation.as_ref().and_then(|_| self.perf.now());
        if let (Some(slot), Some(start), Some(end)) = (observation.as_deref_mut(), start, end) {
            *slot = Some(StageObservation { start, end, refused: result.is_err() });
        }
        result
    }
    fn replay_limit(&self) -> usize {
        #[cfg(test)]
        {
            self.replay_limit
        }
        #[cfg(not(test))]
        {
            MAX_HISTORY_BYTES
        }
    }
    fn validate_actual_redo(&self, redo: Option<&Changeset>, assets: &AssetStore) -> Result<(), Error> {
        if let Some(redo) = redo {
            redo.validate_replay_budget(self.replay_limit().saturating_sub(12))?;
            history_bytes(redo)?;
            if self.persistence.is_some() {
                nodecraft_format::journal::validate_replay(redo, assets)?;
            }
        }
        Ok(())
    }

    fn prepare(&self, effect: Effect) -> Result<Prepared, Error> {
        self.prepare_observed(effect, None)
    }
    fn prepare_observed(&self, effect: Effect, observation: Option<&mut Option<StageObservation>>) -> Result<Prepared, Error> {
        self.prepare_with_assets_observed(effect, &self.assets, false, observation)
    }
    fn prepare_with_assets(&self, effect: Effect, assets: &AssetStore, imported: bool) -> Result<Prepared, Error> {
        self.prepare_with_assets_observed(effect, assets, imported, None)
    }
    fn prepare_with_assets_observed(
        &self,
        effect: Effect,
        assets: &AssetStore,
        imported: bool,
        observation: Option<&mut Option<StageObservation>>,
    ) -> Result<Prepared, Error> {
        self.prepare_source_observed(
            effect,
            PreparationSource {
                document: &self.document,
                revision: self.revision,
                state: self.state,
                execution: &self.execution,
                assets,
                identity: &self.admission_identity,
            },
            imported,
            observation,
        )
    }
    fn prepare_source_observed(
        &self,
        effect: Effect,
        source: PreparationSource<'_>,
        imported: bool,
        mut observation: Option<&mut Option<StageObservation>>,
    ) -> Result<Prepared, Error> {
        let PreparationSource { document: source, revision, state: source_state, execution, assets, identity } = source;
        let next_execution_generation = execution.next()?;
        self.context(Phase::Dispatch).editable()?;
        let (document, inverse, forward, state, direction, gesture, result) = match effect {
            Effect::Edit { changes, result, gesture } => {
                validate_json(&result)?;
                if changes.changes.is_empty() {
                    return Err(invalid("command must return a nonempty changeset"));
                }
                let gesture = move_key(&changes, gesture)?;
                let staged = self.stage(source, &changes, &mut observation)?;
                self.validate_actual_redo(staged.actual_redo.as_ref(), assets)?;
                let (document, inverse) = (staged.document, staged.inverse);
                // Graph canonical sizing includes the operation array. History stores the full
                // {"changes": ...} envelope too (12 bytes), including reserved AddNode replay.
                changes.validate_replay_budget(MAX_HISTORY_BYTES.saturating_sub(12))?;
                (
                    document,
                    inverse,
                    changes,
                    revision.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "document revision overflow"))?,
                    Direction::Edit,
                    gesture,
                    result,
                )
            }
            Effect::Undo => {
                let entry = self.undo.entries.back().ok_or_else(|| Error::new(ErrorKind::Conflict, "nothing to undo"))?;
                let staged = self.stage(source, &entry.inverse, &mut observation)?;
                self.validate_actual_redo(staged.actual_redo.as_ref(), assets)?;
                let (document, inverse) = (staged.document, staged.inverse);
                (document, inverse, entry.inverse.clone(), entry.state, Direction::Undo, None, Value::Null)
            }
            Effect::Redo => {
                let entry = self.redo.entries.back().ok_or_else(|| Error::new(ErrorKind::Conflict, "nothing to redo"))?;
                let staged = self.stage(source, &entry.inverse, &mut observation)?;
                self.validate_actual_redo(staged.actual_redo.as_ref(), assets)?;
                let (document, inverse) = (staged.document, staged.inverse);
                (document, inverse, entry.inverse.clone(), entry.state, Direction::Redo, None, Value::Null)
            }
            Effect::None { .. }
            | Effect::PreparedRun { .. }
            | Effect::Service { .. }
            | Effect::Open { .. }
            | Effect::Save { .. }
            | Effect::Perf { .. }
            | Effect::CancelFile => {
                return Err(invalid("non-document effects must bypass history staging"));
            }
        };
        let bytes = history_bytes(&inverse)?;
        if imported && self.persistence.is_none() {
            // Memory-only imports still authenticate their actual references, without
            // imposing the frozen persistent graph-role format on memory documents.
            assets.verify_changed_references(
                &document,
                &forward,
                &nodecraft_format::asset_source::TransferCancel::default(),
                &nodecraft_format::asset_source::StreamMetrics::default(),
            )?;
        }
        let admission = if self.persistence.is_some() {
            // The checkpoint admitted the v1 root. Complete immutable forward/inverse
            // payload guards preserve that representation without another project walk.
            let replay = nodecraft_format::journal::AdmittedReplay::new(forward, assets)?;
            nodecraft_format::journal::validate_replay(&inverse, assets)?;
            if self.persistence.as_ref().is_some_and(|p| p.capability() == nodecraft_format::StorageCapability::StreamingV2) {
                replay.validate_changed_presence(&document, assets)?;
            } else {
                replay.validate_changed_assets(&document, assets)?;
            }
            Some(PersistentAdmission { replay })
        } else {
            None
        };
        Ok(Prepared {
            document,
            inverse: Entry { inverse, bytes, state: source_state },
            direction,
            gesture,
            result,
            admission,
            source: SourceStamp { identity: Arc::clone(identity), revision },
            next_execution_generation,
            state,
            assets: assets.clone(),
            next_identity: Arc::new(()),
        })
    }

    /// Each frontend/subscriber owns a cursor. On `Conflict`, reread `document()` and reset
    /// the cursor to `revision()`. Cursor zero subscribes from session creation.
    pub fn events_since(&self, cursor: u64) -> Result<Vec<DocChanged>, Error> {
        if cursor > self.revision {
            return Err(invalid("event cursor is ahead of the session"));
        }
        if self.events.front().is_some_and(|first| cursor < first.revision.saturating_sub(1)) {
            return Err(Error::new(ErrorKind::Conflict, "event history expired; reload the document and reset the revision cursor"));
        }
        Ok(self.events.iter().filter(|event| event.revision > cursor).cloned().collect())
    }
}

pub(crate) fn validate_gesture(gesture: Option<&str>) -> Result<(), Error> {
    if gesture.is_some_and(|token| token.is_empty() || token.len() > 128) {
        return Err(invalid("gesture must contain 1 to 128 bytes"));
    }
    Ok(())
}

fn move_key(changes: &Changeset, gesture: Option<String>) -> Result<Option<MoveKey>, Error> {
    let Some(gesture) = gesture else { return Ok(None) };
    validate_gesture(Some(&gesture))?;
    match changes.changes.as_slice() {
        [Change::MoveNode { canvas, id, .. }] => Ok(Some(MoveKey { canvas: *canvas, node: *id, gesture })),
        _ => Err(invalid("gesture coalescing requires exactly one node move")),
    }
}

pub(crate) fn guard<T>(operation: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    catch_unwind(AssertUnwindSafe(operation))
        .map_err(|_| Error::new(ErrorKind::Internal, "command failed unexpectedly; document and undo history were preserved"))?
}

/// The asset hashes `values` name (lists included), in order.
fn media(values: &[nodecraft_types::Value], out: &mut Vec<nodecraft_types::Blake3>) {
    for value in values {
        match value {
            nodecraft_types::Value::Media(asset) => out.push(asset.hash),
            nodecraft_types::Value::List(items) => media(items, out),
            _ => {}
        }
    }
}

fn history_bytes(changes: &Changeset) -> Result<usize, Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_HISTORY_BYTES {
                return Err(std::io::Error::other("undo step exceeds history budget"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, changes).map_err(|_| Error::new(ErrorKind::InvalidInput, "undo step exceeds history budget"))?;
    Ok(counter.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Generation, Node};
    use nodecraft_types::{GenerationId, ProjectId};
    use serde_json::json;

    #[test]
    fn actual_pin_redo_over_history_limit_preserves_session_and_journal() {
        use nodecraft_format::{MemoryStorage, ProjectStorage, StorageLocation};
        use nodecraft_graph::{Comment, Position};
        use nodecraft_types::CommentId;
        use std::{cell::RefCell, rc::Rc};
        #[derive(Clone, Default)]
        struct Shared(Rc<RefCell<MemoryStorage>>);
        impl ProjectStorage for Shared {
            fn load(&mut self, p: &StorageLocation) -> Result<Vec<u8>, Error> {
                self.0.borrow_mut().load(p)
            }
            fn save(&mut self, p: &StorageLocation, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().save(p, b)
            }
            fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().checkpoint(b)
            }
            fn append(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().append(b)
            }
        }
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = CommentId::from_bytes([3; 16]);
        let mut project = Project::new(ProjectId::from_bytes([0; 16]), "Budget").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        Change::PutComment { canvas, comment: Comment::new(id, "Root", Position::default()) }.apply(&mut project).unwrap();
        let node = Node::new(NodeId::from_bytes([2; 16]), "source.text");
        let forward =
            Changeset::new(vec![Change::SetCommentPin { canvas, id, node: Some(node.id), pos: Position { x: 1.0, y: 2.0 } }, Change::AddNode { canvas, node }]);
        let staged = forward.stage_certified(&project).unwrap();
        let redo = staged.actual_redo.unwrap();
        let cap = history_bytes(&redo).unwrap() - 1;
        forward.validate_replay_budget(cap - 12).unwrap();
        assert!(history_bytes(&staged.inverse).unwrap() <= cap);
        let storage = Shared::default();
        let mut session =
            Session::with_storage(Snapshot { project: project.clone(), assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.pin_budget",
                label: "Pin budget",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "{}",
                enabled: |_, _| Ok(()),
                run: |c, _| Ok(Effect::Edit { changes: c.resources.require::<Changeset>()?.clone(), result: Value::Null, gesture: None }),
            })
            .unwrap();
        session.resources.insert(forward);
        let ticks = std::rc::Rc::new(std::cell::Cell::new(0u64));
        let clock = ticks.clone();
        session.perf_mut().set_clock(Some(Box::new(move || {
            let next = clock.get() + 10;
            clock.set(next);
            next
        })));
        session.perf_mut().set_enabled(true);
        session.replay_limit = cap;
        let journal = storage.0.borrow().recovery.clone();
        assert!(session.dispatch("test.pin_budget", &json!({})).is_err());
        assert_eq!(session.document(), &project);
        assert_eq!(session.revision(), 0);
        assert!(!session.can_undo() && !session.can_redo());
        assert!(session.events.is_empty());
        assert_eq!(storage.0.borrow().recovery, journal);
        // Certification completed; the subsequent actual-redo admission refusal is outside
        // its timing boundary and must not turn that completed work into a refused stage.
        let report = session.perf().report();
        let samples = |metric| report.metrics.iter().find(|row| row.metric == metric).unwrap().samples;
        assert_eq!(samples(crate::perf::Metric::GraphStage), 1);
        assert_eq!(samples(crate::perf::Metric::GraphStageRefused), 0);
        assert_eq!(samples(crate::perf::Metric::Dispatch), 0);
        session.replay_limit = cap + 1;
        session.dispatch("test.pin_budget", &json!({})).unwrap();
        assert_eq!(session.revision(), 1);
    }

    #[test]
    fn non_document_effect_works_at_exhausted_document_revision() {
        fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn service(_: &Context<'_>, _: &Value) -> Result<Effect, Error> {
            Ok(Effect::None { result: json!("ok") })
        }
        let mut session = Session::new(Project::new(ProjectId::from_bytes([1; 16]), "Service").unwrap()).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.service",
                label: "Service",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "{}",
                enabled,
                run: service,
            })
            .unwrap();
        session.revision = u64::MAX;
        assert_eq!(session.dispatch("test.service", &Value::Null).unwrap(), json!("ok"));
        assert_eq!(session.revision(), u64::MAX);
        assert!(!session.can_undo());
        assert!(session.events.is_empty());
    }

    #[test]
    fn canonical_redo_budget_includes_full_changeset_envelope() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let node = Node::new(NodeId::from_bytes([2; 16]), "test.text");
        let changes = Changeset::new(vec![Change::AddNode { canvas, node: node.clone() }]);
        let restore = Changeset::new(vec![Change::RestoreNode { canvas, node, edges: vec![], pins: vec![] }]);
        let full = history_bytes(&restore).unwrap();
        assert!(changes.validate_replay_budget(full - 12).is_ok());
        assert!(changes.validate_replay_budget(full - 13).is_err());
        // The original AddNode spelling is smaller; counting it alone loses the redo boundary.
        assert!(history_bytes(&changes).unwrap() < full);
    }

    #[test]
    fn history_byte_counter_accepts_exact_budget_and_rejects_next_byte() {
        let mut changes = Changeset::new(vec![Change::SetProjectTitle { title: String::new() }]);
        let overhead = history_bytes(&changes).unwrap();
        changes.changes = vec![Change::SetProjectTitle { title: "x".repeat(MAX_HISTORY_BYTES - overhead) }];
        assert_eq!(history_bytes(&changes).unwrap(), MAX_HISTORY_BYTES);
        changes.changes = vec![Change::SetProjectTitle { title: "x".repeat(MAX_HISTORY_BYTES - overhead + 1) }];
        assert!(history_bytes(&changes).is_err());
    }

    #[test]
    fn generation_history_records_only_affected_entry_in_both_directions() {
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let mut document = Project::new(ProjectId::from_bytes([3; 16]), "Compact history").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut document).unwrap();
        let mut node = Node::new(id, "test.text");
        for n in 0..6 {
            node.history.push(Generation {
                id: GenerationId::from_bytes([n; 16]),
                outputs: vec![nodecraft_types::Value::Text("x".repeat(512 * 1024))],
                input_hash: None,
                items: Vec::new(),
                settings: Vec::new(),
                incomplete: None,
            });
        }
        Change::AddNode { canvas, node }.apply(&mut document).unwrap();
        let session = Session::new(document).unwrap();
        let generation = Generation {
            id: GenerationId::from_bytes([9; 16]),
            outputs: vec![nodecraft_types::Value::Text("new".into())],
            input_hash: None,
            items: Vec::new(),
            settings: Vec::new(),
            incomplete: None,
        };
        let changes = Changeset::new(vec![Change::AddGeneration { canvas, id, index: 3, generation }]);
        let prepared = session.prepare(Effect::Edit { changes, result: json!(null), gesture: None }).unwrap();
        assert!(prepared.inverse.bytes < 512);
        let mut edited = prepared.document;
        let redo = prepared.inverse.inverse.apply(&mut edited).unwrap();
        assert!(history_bytes(&redo).unwrap() < 512);
        assert_eq!(edited, *session.document());
        let removal = session
            .prepare(Effect::Edit {
                changes: Changeset::new(vec![Change::RemoveGeneration { canvas, id, generation: GenerationId::from_bytes([2; 16]) }]),
                result: json!(null),
                gesture: None,
            })
            .unwrap();
        assert!(removal.inverse.bytes < 513 * 1024);
    }
    #[test]
    fn generation_transactions_recover_identically_through_undo_redo_and_atomic_failures() {
        use nodecraft_format::{MemoryStorage, StorageLocation, journal};
        use std::{cell::RefCell, rc::Rc};
        #[derive(Clone)]
        struct Shared(Rc<RefCell<MemoryStorage>>);
        impl ProjectStorage for Shared {
            fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
                self.0.borrow_mut().load(l)
            }
            fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().save(l, b)
            }
            fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().checkpoint(b)
            }
            fn append(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().append(b)
            }
        }
        fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn edit(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
            Ok(Effect::Edit { changes: crate::commands::params(value)?, result: Value::Null, gesture: None })
        }
        let canvas = CanvasId::from_bytes([1; 16]);
        let id = NodeId::from_bytes([2; 16]);
        let mut project = Project::new(ProjectId::from_bytes([3; 16]), "Generations").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        let mut node = Node::new(id, "test.text");
        for n in [5, 7] {
            node.history.push(Generation {
                id: GenerationId::from_bytes([n; 16]),
                outputs: vec![nodecraft_types::Value::Text(format!("output{n}"))],
                input_hash: None,
                items: Vec::new(),
                settings: Vec::new(),
                incomplete: None,
            });
        }
        node.active = Some(GenerationId::from_bytes([5; 16]));
        Change::AddNode { canvas, node }.apply(&mut project).unwrap();
        let shared = Shared(Rc::new(RefCell::new(MemoryStorage::default())));
        let mut session = Session::with_storage(Snapshot { project: project.clone(), assets: Default::default(), epoch: 0 }, Box::new(shared.clone())).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.generations",
                label: "Generation edits",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "Changeset",
                enabled,
                run: edit,
            })
            .unwrap();
        let hash = session.import_asset(b"generation asset").unwrap();
        let generation = Generation {
            input_hash: None,
            items: Vec::new(),
            settings: Vec::new(),
            id: GenerationId::from_bytes([6; 16]),
            outputs: vec![nodecraft_types::Value::List(vec![nodecraft_types::Value::Media(nodecraft_types::AssetRef {
                hash,
                mime: "text/plain".parse().unwrap(),
                meta: Default::default(),
            })])],
            incomplete: None,
        };
        let changes = Changeset::new(vec![
            Change::AddGeneration { canvas, id, index: 1, generation },
            Change::SetActiveGeneration { canvas, id, generation: Some(GenerationId::from_bytes([6; 16])) },
            Change::RemoveGeneration { canvas, id, generation: GenerationId::from_bytes([5; 16]) },
        ]);
        session.dispatch("test.generations", &serde_json::to_value(changes).unwrap()).unwrap();
        let edited = session.document().clone();
        for command in ["edit.undo", "edit.redo"] {
            let recovered = journal::recover(&shared.0.borrow().recovery).unwrap();
            assert_eq!(&recovered.snapshot.project, session.document());
            assert_eq!(recovered.snapshot.assets.get(&hash), Some(b"generation asset".as_slice()));
            session.dispatch(command, &Value::Null).unwrap();
            if command == "edit.undo" {
                assert_eq!(session.document(), &project);
            } else {
                assert_eq!(session.document(), &edited);
            }
        }
        assert_eq!(journal::recover(&shared.0.borrow().recovery).unwrap().snapshot.project, edited);
        let bytes = shared.0.borrow().recovery.clone();
        let events = session.events_since(0).unwrap();
        let revision = session.revision();
        let changes = Changeset::new(vec![
            Change::SetProjectTitle { title: "Never published".into() },
            Change::RemoveGeneration { canvas, id, generation: GenerationId::from_bytes([6; 16]) },
        ]);
        assert!(session.dispatch("test.generations", &serde_json::to_value(changes).unwrap()).is_err());
        assert_eq!(session.document(), &edited);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(shared.0.borrow().recovery, bytes);
        assert!(!session.file_status().recovery_required);
    }
    #[test]
    fn incremental_large_graph_delete_reserves_replay_after_empty_checkpoint() {
        use nodecraft_format::{MemoryStorage, StorageLocation, journal};
        use nodecraft_graph::Comment;
        use nodecraft_types::{AssetRef, CommentId, MediaMeta};
        use std::{cell::RefCell, rc::Rc};
        #[derive(Clone)]
        struct Shared(Rc<RefCell<MemoryStorage>>);
        impl ProjectStorage for Shared {
            fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
                self.0.borrow_mut().load(l)
            }
            fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().save(l, b)
            }
            fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().checkpoint(b)
            }
            fn append(&mut self, b: &[u8]) -> Result<(), Error> {
                self.0.borrow_mut().append(b)
            }
        }
        fn enabled(_: &Context<'_>, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn edit(_: &Context<'_>, value: &Value) -> Result<Effect, Error> {
            let canvas = CanvasId::from_bytes([1; 16]);
            let change = match value["op"].as_str().unwrap() {
                "comment" => Change::PutComment {
                    canvas,
                    comment: Comment {
                        id: CommentId::from_bytes([value["n"].as_u64().unwrap() as u8; 16]),
                        text: "x".repeat(usize::try_from(value["len"].as_u64().unwrap()).unwrap()),
                        pos: Default::default(),
                        node: None,
                        replies: vec![],
                        resolved: false,
                    },
                },
                "node" => {
                    let mut node = Node::new(NodeId::from_bytes([value["n"].as_u64().unwrap() as u8; 16]), "test.media");
                    node.params.insert(
                        "asset".into(),
                        nodecraft_types::Value::Media(AssetRef {
                            hash: serde_json::from_value(value["hash"].clone()).unwrap(),
                            mime: "text/plain".parse().unwrap(),
                            meta: MediaMeta::default(),
                        }),
                    );
                    Change::AddNode { canvas, node }
                }
                "delete" => Change::RemoveCanvas { id: canvas },
                _ => panic!("unexpected fixture operation"),
            };
            Ok(Effect::Edit { changes: Changeset::new(vec![change]), result: Value::Null, gesture: None })
        }
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Large boundary").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Large") }.apply(&mut project).unwrap();
        let storage = Shared(Rc::new(RefCell::new(MemoryStorage::default())));
        let mut session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
        session
            .registry
            .register(crate::commands::CommandSpec {
                id: "test.large",
                label: "Boundary edits",
                shortcut: None,
                alternate_shortcuts: &[],
                input_schema: || json!({}),
                params_doc: "fixture",
                enabled,
                run: edit,
            })
            .unwrap();
        for n in 0..3_u8 {
            let hash = session.import_asset(&vec![b'a' + n; nodecraft_format::MAX_ASSET_BYTES]).unwrap();
            session.dispatch("test.large", &json!({"op":"node","n":10+n,"hash":hash})).unwrap();
        }
        for n in 0..16_u8 {
            session.dispatch("test.large", &json!({"op":"comment","n":50+n,"len":if n==15 {0} else {1024*1024}})).unwrap();
        }
        let delete = Changeset::new(vec![Change::RemoveCanvas { id: canvas }]);
        let base_size = {
            let (_, inverse) = delete.stage(session.document()).unwrap();
            history_bytes(&inverse).unwrap()
        };
        let last_len = MAX_HISTORY_BYTES.checked_sub(64).unwrap().checked_sub(base_size).unwrap();
        assert!(last_len < 1024 * 1024);
        session.dispatch("test.large", &json!({"op":"comment","n":65,"len":last_len})).unwrap();
        let revision = session.revision();
        let events = session.events_since(0).unwrap();
        let recovery_len = storage.0.borrow().recovery.len();
        let undo = session.undo.entries.len();
        // The changeset fits engine history, but max-sequence Record+48MiB assets+headers do not.
        assert!(session.dispatch("test.large", &json!({"op":"delete"})).is_err());
        assert_eq!(session.revision(), revision);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(storage.0.borrow().recovery.len(), recovery_len);
        assert_eq!(session.undo.entries.len(), undo);
        assert!(!session.file_status().recovery_required);
        // Move safely below both record and binary-frame limits, then exercise the actual future
        // checkpoint shape that omits every retained asset and must re-add them during undo.
        session.dispatch("test.large", &json!({"op":"comment","n":65,"len":last_len-4096})).unwrap();
        session.dispatch("test.large", &json!({"op":"delete"})).unwrap();
        assert!(session.document().canvases().is_empty());
        session.dispatch("file.save_as", &json!({"location":"empty.ncraft"})).unwrap();
        assert_eq!(session.assets().byte_len(), nodecraft_format::MAX_TOTAL_ASSET_BYTES as u64);
        session.dispatch("edit.undo", &Value::Null).unwrap();
        assert_eq!(session.document().canvases().get(&canvas).unwrap().nodes().len(), 3);
        let recovered = journal::recover(&storage.0.borrow().recovery).unwrap();
        assert_eq!(&recovered.snapshot.project, session.document());
        assert_eq!(recovered.snapshot.assets.byte_len(), nodecraft_format::MAX_TOTAL_ASSET_BYTES as u64);
        session.dispatch("edit.redo", &Value::Null).unwrap();
        assert!(session.document().canvases().is_empty());
    }
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod persistence_tests;

#[cfg(test)]
#[path = "admission_tests.rs"]
mod admission_tests;

#[cfg(test)]
mod clipboard_tests;
