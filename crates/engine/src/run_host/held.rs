//! Paid results a run couldn't keep (#369). When a finished model job's result can't be converted,
//! imported, fitted to its node's ports or recorded, the exact result the provider returned is
//! held here instead of being dropped, with what its step needed. A person (or agent) then
//! retries recording it, puts its outputs on the canvas as source nodes, or discards it; none of
//! these calls the provider again or charges again. A fanned or multi-job step's results are held
//! as one, outputs in job order, and can only be placed or discarded (#611).
//!
//! With a store ([`crate::run_host::RunHost::open_held`], #614) each held result is also saved as
//! one record, removed once it is placed, recorded or discarded, so it survives a restart; one
//! that doesn't fit the store's byte cap, or can't be written, is held in memory only and says so.
//! Without a store, held results are lost when NodeCraft quits. At most [`MAX_HELD`] are held:
//! once that many wait, every new job is refused before it is sent (see the run host's
//! write-ahead), so a full store never drops a result. Jobs already in flight may still add theirs.
use super::lock;
use crate::Session;
use crate::generate;
use crate::held_cmds::{HELD_SERVICE, MAX_HELD};
use crate::held_store::{HeldRecord, HeldStore, MAX_HELD_STORE_BYTES};
use crate::history_cmds::source_kind;
use nodecraft_exec::run::ResumeRecipe;
use nodecraft_graph::{Node, Position};
use nodecraft_nodes::GenerateOutput;
use nodecraft_providers::Completed;
use nodecraft_types::{CanvasId, Error, ErrorKind, Modality, NodeId, ParamValues, ProjectId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// How far right of its node a placed result goes, and how far apart placed results stack.
const PLACE_OFFSET: (f32, f32) = (360.0, 220.0);

/// What a held result is waiting to have done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldAction {
    Retry,
    Place,
}

/// A held result, as the queue panel shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldView {
    pub id: u64,
    pub canvas: CanvasId,
    pub node: NodeId,
    pub provider: String,
    pub model: String,
    /// Why it couldn't be kept.
    pub reason: String,
    /// How many outputs the provider returned.
    pub outputs: usize,
    /// Whether recording it can be retried: its step needed only its result.
    pub retry: bool,
    /// What it is waiting to have done, if asked.
    pub pending: Option<HeldAction>,
    /// Whether it is saved, so it survives a restart (#614).
    pub saved: bool,
}

/// A paid result and what recording or placing it needs.
#[derive(Clone)]
pub(super) struct HeldResult {
    pub(super) project: ProjectId,
    pub(super) canvas: CanvasId,
    pub(super) node: NodeId,
    pub(super) provider: String,
    pub(super) model: String,
    pub(super) reason: String,
    pub(super) completed: Completed,
    pub(super) routes: Vec<GenerateOutput>,
    /// `None` when its step needs other jobs' results too: it can only be placed or discarded.
    pub(super) recipe: Option<ResumeRecipe>,
}

impl HeldResult {
    fn record(&self) -> HeldRecord {
        HeldRecord {
            project: self.project,
            canvas: self.canvas,
            node: self.node,
            provider: self.provider.clone(),
            model: self.model.clone(),
            reason: self.reason.clone(),
            completed: self.completed.clone(),
            routes: self.routes.clone(),
            recipe: self.recipe.clone(),
        }
    }
}

impl From<HeldRecord> for HeldResult {
    fn from(r: HeldRecord) -> Self {
        Self {
            project: r.project,
            canvas: r.canvas,
            node: r.node,
            provider: r.provider,
            model: r.model,
            reason: r.reason,
            completed: r.completed,
            routes: r.routes,
            recipe: r.recipe,
        }
    }
}

/// A saved record: its number in the store, and its size.
#[derive(Clone, Copy)]
struct Saved {
    record: u64,
    bytes: u64,
}

struct Entry {
    id: u64,
    result: HeldResult,
    pending: Option<HeldAction>,
    saved: Option<Saved>,
}

/// The held results, shared by the run host and its service.
#[derive(Default)]
pub(super) struct Held {
    next: u64,
    entries: Vec<Entry>,
    store: Option<Arc<dyn HeldStore>>,
    /// The bytes of every saved record, and of records being written.
    stored: u64,
    /// The highest record number in the store, used or set aside.
    last_record: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Op {
    List,
    Retry,
    Place,
    Discard,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: Op,
    #[serde(default)]
    id: Option<u64>,
}

/// Holds `result` in `held`, saving it when there is a store and room. Never refuses: the bound is
/// kept by refusing new jobs instead. The record is encoded and written outside the lock, so a
/// large one doesn't stall the runner.
pub(super) fn hold(held: &Mutex<Held>, result: HeldResult) -> u64 {
    let bytes = lock(held).store.is_some().then(|| result.record().to_bytes().ok()).flatten();
    let (id, reserved) = {
        let mut held = lock(held);
        let reserved = bytes.as_ref().and_then(|bytes| held.reserve(u64::try_from(bytes.len()).ok()?));
        (held.push(result), reserved)
    };
    if let (Some(bytes), Some((store, saved))) = (bytes, reserved) {
        let written = store.write(saved.record, &bytes);
        let mut held = lock(held);
        match written {
            Ok(()) => held.saved(id, store.as_ref(), saved),
            Err(_) => held.stored = held.stored.saturating_sub(saved.bytes),
        }
    }
    id
}

impl Held {
    /// Whether new jobs must wait until a held result is dealt with.
    pub(super) fn is_full(&self) -> bool {
        self.entries.len() >= MAX_HELD
    }

    /// Holds `result` as a new entry, without saving it.
    fn push(&mut self, result: HeldResult) -> u64 {
        self.next = self.next.saturating_add(1);
        self.entries.push(Entry { id: self.next, result, pending: None, saved: None });
        self.next
    }

    /// Reserves a record number and `size` bytes of the store for a new record, if it fits.
    fn reserve(&mut self, size: u64) -> Option<(Arc<dyn HeldStore>, Saved)> {
        let store = self.store.clone()?;
        if self.stored.checked_add(size)? > MAX_HELD_STORE_BYTES {
            return None;
        }
        let record = self.last_record.checked_add(1)?;
        self.last_record = record;
        self.stored = self.stored.saturating_add(size);
        Some((store, Saved { record, bytes: size }))
    }

    /// Removes entry `index` and its saved record. A record that can't be removed comes back
    /// after a restart, still held: placing or discarding it then never charges again.
    fn let_go(&mut self, index: usize) {
        if index >= self.entries.len() {
            return;
        }
        let entry = self.entries.remove(index);
        if let (Some(store), Some(saved)) = (&self.store, entry.saved) {
            let _ = store.remove(saved.record);
            self.stored = self.stored.saturating_sub(saved.bytes);
        }
    }

    /// Keeps held results in `store` from now on, holding again the results it has saved (as new
    /// entries, after any held already), up to [`MAX_HELD`] and the store's byte cap; the rest
    /// stay saved, and count against the cap, until a later launch. Records that can't be read
    /// are set aside, and ones from a newer NodeCraft left alone; the error says so, and the store
    /// is used anyway. A store that can't be listed isn't used, and a host opens only one.
    pub(super) fn open(&mut self, store: Box<dyn HeldStore>) -> Result<(), Error> {
        if self.store.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "held results are already kept in a store"));
        }
        let listing = store.list().map_err(|e| Error::new(e.kind, format!("held results won't be kept across restarts: {}", e.message)))?;
        self.last_record = self.last_record.max(listing.highest);
        self.stored = self.stored.saturating_add(listing.bytes);
        let mut problems: Vec<String> = Vec::new();
        let mut restored = 0u64;
        let mut waiting = listing.records.len();
        for (record, size) in listing.records {
            if self.is_full() || restored.saturating_add(size) > MAX_HELD_STORE_BYTES {
                continue;
            }
            match store.read(record).and_then(|bytes| HeldRecord::from_bytes(&bytes)) {
                Ok(result) => {
                    restored = restored.saturating_add(size);
                    waiting = waiting.saturating_sub(1);
                    let id = self.push(result.into());
                    if let Some(entry) = self.entries.last_mut().filter(|entry| entry.id == id) {
                        entry.saved = Some(Saved { record, bytes: size });
                    }
                }
                Err(error) if error.kind == ErrorKind::Unsupported => {
                    waiting = waiting.saturating_sub(1);
                    problems.push(format!("{} (left in place)", error.message));
                }
                Err(error) => {
                    waiting = waiting.saturating_sub(1);
                    self.stored = self.stored.saturating_sub(size);
                    problems.push(match store.set_aside(record) {
                        Ok(()) => format!("{} (set aside)", error.message),
                        Err(aside) => format!("{}; {}", error.message, aside.message),
                    });
                }
            }
        }
        self.store = Some(Arc::from(store));
        if waiting > 0 {
            problems.push(format!("{waiting} more are saved and come back at a later launch, once fewer than {MAX_HELD} are held"));
        }
        match problems.as_slice() {
            [] => Ok(()),
            _ => Err(Error::new(ErrorKind::InvalidInput, format!("some held results couldn't be restored: {}", problems.join("; ")))),
        }
    }

    /// Records that `saved` was written for entry `id`; if the entry was let go meanwhile, the
    /// record goes too.
    fn saved(&mut self, id: u64, store: &dyn HeldStore, saved: Saved) {
        match self.entries.iter_mut().find(|entry| entry.id == id) {
            Some(entry) => entry.saved = Some(saved),
            None => {
                let _ = store.remove(saved.record);
                self.stored = self.stored.saturating_sub(saved.bytes);
            }
        }
    }

    pub(super) fn views(&self) -> Vec<HeldView> {
        self.entries.iter().map(view).collect()
    }

    fn listing(&self) -> Value {
        json!({"held": self.views().iter().map(view_json).collect::<Vec<_>>(), "max": MAX_HELD})
    }

    /// The results waiting to be retried or placed, copied so they stay held until that works.
    pub(super) fn pending(&self) -> Vec<(u64, HeldAction, HeldResult)> {
        self.entries.iter().filter_map(|e| e.pending.map(|action| (e.id, action, e.result.clone()))).collect()
    }

    /// Ends a retry or placement: drops the result when it worked, keeps it otherwise.
    pub(super) fn settle(&mut self, id: u64, worked: bool) {
        if worked {
            if let Some(index) = self.entries.iter().position(|e| e.id == id) {
                self.let_go(index);
            }
        } else if let Some(entry) = self.entries.iter_mut().find(|e| e.id == id) {
            entry.pending = None;
        }
    }

    /// Answers a `run.held*` command's service request.
    pub(super) fn serve(&mut self, request: &Value) -> Result<Value, Error> {
        let request: Request = serde_json::from_value(request.clone()).map_err(|e| invalid(&format!("invalid held-results request: {e}")))?;
        if let Op::List = request.op {
            return Ok(self.listing());
        }
        let id = request.id.ok_or_else(|| invalid("`id` names the held result"))?;
        let index = self.entries.iter().position(|e| e.id == id).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no held result {id}")))?;
        let entry = self.entries.get_mut(index).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no held result {id}")))?;
        if entry.pending.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "this held result is already being retried or placed"));
        }
        match request.op {
            Op::Retry if entry.result.recipe.is_none() => {
                Err(Error::new(ErrorKind::Unsupported, "its step needs other jobs' results too, so it can only be put on the canvas or discarded"))
            }
            Op::Retry => {
                entry.pending = Some(HeldAction::Retry);
                Ok(json!({"id": id, "pending": "retry"}))
            }
            Op::Place => {
                entry.pending = Some(HeldAction::Place);
                Ok(json!({"id": id, "pending": "place"}))
            }
            Op::Discard => {
                self.let_go(index);
                Ok(json!({"id": id, "discarded": true}))
            }
            Op::List => Ok(self.listing()),
        }
    }
}

fn view(e: &Entry) -> HeldView {
    let r = &e.result;
    HeldView {
        id: e.id,
        canvas: r.canvas,
        node: r.node,
        provider: r.provider.clone(),
        model: r.model.clone(),
        reason: r.reason.clone(),
        outputs: r.completed.outputs.len(),
        retry: r.recipe.is_some(),
        pending: e.pending,
        saved: e.saved.is_some(),
    }
}

fn view_json(v: &HeldView) -> Value {
    let pending = v.pending.map(|p| match p {
        HeldAction::Retry => "retry",
        HeldAction::Place => "place",
    });
    json!({"id": v.id, "canvas": v.canvas, "node": v.node, "provider": v.provider, "model": v.model, "reason": v.reason,
        "outputs": v.outputs, "retry": v.retry, "pending": pending, "saved": v.saved})
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// Registers [`HELD_SERVICE`] over `held`.
pub(super) fn register(held: &Arc<Mutex<Held>>, services: &mut crate::commands::Services) -> Result<(), Error> {
    let held = Arc::clone(held);
    services.register(HELD_SERVICE, move |request| lock(&held).serve(request))
}

/// One route per modality, so any output converts, each to the port named for its modality.
fn every_modality() -> Vec<GenerateOutput> {
    [Modality::Text, Modality::Image, Modality::Video, Modality::Audio, Modality::Mesh, Modality::Document]
        .into_iter()
        .map(|modality| GenerateOutput { port: modality_port(modality).to_owned(), modality })
        .collect()
}

fn modality_port(modality: Modality) -> &'static str {
    match modality {
        Modality::Text => "text",
        Modality::Image => "image",
        Modality::Video => "video",
        Modality::Audio => "audio",
        Modality::Mesh => "mesh",
        Modality::Document => "document",
    }
}

/// Puts a held result's outputs on its canvas, in order, as source nodes stacked to the right of
/// its node: one `canvas.paste`, so one undo step.
pub(super) fn place(session: &mut Session, result: &HeldResult) -> Result<(), String> {
    if session.document().id() != result.project {
        return Err("its project isn't open".into());
    }
    let canvas = session.document().canvases().get(&result.canvas).ok_or("its canvas is gone")?;
    let origin = canvas.nodes().get(&result.node).map_or(Position::default(), |n| n.pos);
    let routes = every_modality();
    let produced = generate::produced(&routes, &result.completed).map_err(|e| e.message)?;
    let mut nodes: Vec<Node> = Vec::new();
    nodes.try_reserve_exact(produced.outputs.len()).map_err(|_| "too many outputs to place")?;
    for (index, (port, value)) in produced.outputs.into_iter().enumerate() {
        let modality = routes.iter().find(|r| r.port == port).map(|r| r.modality).ok_or("an output has no modality")?;
        let (kind, parameter) = source_kind(modality).map_err(|e| e.message)?;
        let supplied: ParamValues = [(parameter.to_owned(), value)].into_iter().collect();
        let id = fresh_node(session, &nodes)?;
        let mut node = session.kinds().get(kind).and_then(|k| k.instantiate_instance(id, BTreeMap::new(), &supplied)).map_err(|e| e.message)?;
        let row = u16::try_from(index).map_or(f32::from(u16::MAX), f32::from);
        node.pos = Position { x: origin.x + PLACE_OFFSET.0, y: origin.y + row * PLACE_OFFSET.1 };
        nodes.push(node);
    }
    for (hash, bytes) in produced.assets {
        match session.import_asset(&bytes) {
            Ok(imported) if imported == hash => {}
            Ok(_) => return Err("an asset changed while it was imported".into()),
            Err(error) => return Err(error.message),
        }
    }
    let params = json!({"canvas": result.canvas, "nodes": nodes});
    session.dispatch("canvas.paste", &params).map(|_| ()).map_err(|e| e.message)
}

/// A node id no canvas uses and none of `taken` has.
fn fresh_node(session: &Session, taken: &[Node]) -> Result<NodeId, String> {
    for _ in 0..8 {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| "node id entropy is unavailable")?;
        let id = NodeId::from_bytes(bytes);
        let used = session.document().canvases().values().any(|c| c.nodes().contains_key(&id)) || taken.iter().any(|n| n.id == id);
        if !used {
            return Ok(id);
        }
    }
    Err("couldn't allocate a node id".into())
}
