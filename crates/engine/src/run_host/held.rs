//! Paid results a run couldn't keep (#369). When a finished model job's result can't be converted,
//! imported, fitted to its node's ports or recorded, the exact result the provider returned is
//! held here instead of being dropped, with what its step needed. A person (or agent) then
//! retries recording it, puts its outputs on the canvas as source nodes, or discards it; none of
//! these calls the provider again or charges again. A fanned or multi-job step's results are held
//! as one, outputs in job order, and can only be placed or discarded (#611).
//!
//! Held results live in memory and are lost when NodeCraft quits. At most [`MAX_HELD`] are held:
//! once that many wait, every new job is refused before it is sent (see the run host's
//! write-ahead), so a full store never drops a result. Jobs already in flight may still add theirs.
use super::lock;
use crate::Session;
use crate::generate;
use crate::held_cmds::{HELD_SERVICE, MAX_HELD};
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

struct Entry {
    id: u64,
    result: HeldResult,
    pending: Option<HeldAction>,
}

/// The held results, shared by the run host and its service.
#[derive(Default)]
pub(super) struct Held {
    next: u64,
    entries: Vec<Entry>,
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

impl Held {
    /// Whether new jobs must wait until a held result is dealt with.
    pub(super) fn is_full(&self) -> bool {
        self.entries.len() >= MAX_HELD
    }

    /// Holds `result`. Never refuses: the bound is kept by refusing new jobs instead.
    pub(super) fn hold(&mut self, result: HeldResult) -> u64 {
        self.next = self.next.saturating_add(1);
        self.entries.push(Entry { id: self.next, result, pending: None });
        self.next
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
            self.entries.retain(|e| e.id != id);
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
                self.entries.remove(index);
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
    }
}

fn view_json(v: &HeldView) -> Value {
    let pending = v.pending.map(|p| match p {
        HeldAction::Retry => "retry",
        HeldAction::Place => "place",
    });
    json!({"id": v.id, "canvas": v.canvas, "node": v.node, "provider": v.provider, "model": v.model, "reason": v.reason,
        "outputs": v.outputs, "retry": v.retry, "pending": pending})
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
