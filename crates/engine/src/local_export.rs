//! `export.local_plan` / `export.local_run` (#98): write a **batch** of results into one folder,
//! with a naming pattern, a collision policy, partial failure and cancellation.
//!
//! # What this is, and what #557 already owns
//!
//! [`crate::export_cmds`] (`history.export`, #557) already ships the single-file half of export on
//! this machine, and it owns it outright: the absolute-path check, the rule that the destination
//! folder must already exist, never replacing a file without consent, hash-verifying the asset
//! and streaming it through the store's verifying copy, refusing a text result, and refusing the
//! browser build. Batch publication uses pinned-directory, descriptor-relative operations instead of the
//! single-file writer's pathname publication: a folder grant must survive hostile rename/link
//! races. The verifying asset-copy primitive is shared with `history.export`.
//!
//! What is additive here is only what a batch needs and one file does not have:
//!
//! * a **naming pattern** over many results ([`nodecraft_format::export::Template`]) and one
//!   **folder** rather than one path,
//! * a **collision policy** across the batch (`skip` / `overwrite` / `numbered`),
//! * **folder authority** ([`ExportHost`]) — one grant covering many writes,
//! * **cancellation between files**, and
//! * **one outcome per request**, so `partial` is never a guess.
//!
//! Export is deliberately **not** a document edit: it writes files and changes nothing on the
//! canvas, so it has no undo step and never touches `graph`. The handler resolves and validates the
//! whole request (a pure read of the document) and returns [`Effect::LocalExport`]; `Session`
//! performs the writes, because handlers must not do I/O. Programmatic callers — palette, CLI,
//! control channel, MCP, the in-canvas agent — all dispatch these same ids, and none of them opens
//! a dialog: the destination arrives as a parameter, and the app decides in advance which folders
//! an export may write into by injecting an [`ExportHost`].
//!
//! The **active generation** of a source is its payload, exactly as `history.export` treats it:
//! this does not export "all history", and the parameter documentation says so.

use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_format::asset_source::TransferCancel;
use nodecraft_format::export::{
    Collision, EntryOutcome, EntryState, ExportName, ExportOutcome, ExportPlan, ExportSource, FORMAT_CAPABILITY, FormatAction, MAX_COLLISION_SUFFIX,
    MAX_DESTINATION_BYTES, MAX_ENTRIES, MAX_ITEM_INDEX, MAX_MIME_BYTES, MAX_NAME_BYTES, MAX_TEMPLATE_BYTES, NameFields, PlannedEntry, TEMPLATE_TOKENS,
    Template, media_for, reasons,
};
use nodecraft_graph::Node;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod secure;

#[cfg(unix)]
#[derive(Clone)]
struct PinnedRoot {
    original: PathBuf,
    resolved: PathBuf,
    file: std::sync::Arc<std::fs::File>,
}

/// Most folders one session may export into.
pub const MAX_ROOTS: usize = 16;
/// Most results one export writes; mirrors the limit inside `format::export`.
pub const MAX_SOURCES: usize = MAX_ENTRIES;

/// The host's export authority (#98): the folders an export may write into, and the token that
/// stops one in progress.
///
/// A command parameter is caller-controlled — the palette, the CLI and an agent in the canvas can
/// all supply one — so a path in a parameter is never authority to write there. The app injects
/// this after a person chooses a folder; without it every export is refused, and on the web there
/// is no filesystem to authorize.
///
/// This is the batch's counterpart to the one path `history.export` gets from a save dialog: there,
/// a person named the exact file; here, a person names the folder and the pattern fills in the
/// rest, so the folder itself has to be the grant.
#[derive(Clone)]
pub struct ExportHost {
    roots: Vec<PathBuf>,
    #[cfg(unix)]
    pinned: Vec<PinnedRoot>,
    cancel: TransferCancel,
}

impl ExportHost {
    /// Authorizes `roots` for export. Each must exist; Unix opens and pins its resolved inode,
    /// rejects a link as the selected folder and refuses changed root identity.
    pub fn new(roots: impl IntoIterator<Item = PathBuf>, cancel: TransferCancel) -> Result<Self, Error> {
        let mut resolved = Vec::new();
        #[cfg(unix)]
        let mut pinned = Vec::new();
        for root in roots {
            if resolved.len() >= MAX_ROOTS {
                return Err(Error::new(ErrorKind::InvalidInput, "an app may authorize at most 16 export folders"));
            }
            #[cfg(unix)]
            let original = root.symlink_metadata().map_err(|_| Error::new(ErrorKind::InvalidInput, "an authorized export folder does not exist"))?;
            #[cfg(unix)]
            if original.file_type().is_symlink() {
                return Err(Error::new(ErrorKind::InvalidInput, "an authorized export folder is a symbolic link; choose the real folder"));
            }
            let path = std::fs::canonicalize(&root).map_err(|_| Error::new(ErrorKind::InvalidInput, "an authorized export folder does not exist"))?;
            if !path.is_dir() {
                return Err(Error::new(ErrorKind::InvalidInput, "an authorized export folder is not a folder"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                let file = secure::open_root(&path)?;
                let opened = file.metadata().map_err(|_| Error::new(ErrorKind::InvalidInput, "could not verify the authorized export folder"))?;
                if original.dev() != opened.dev() || original.ino() != opened.ino() {
                    return Err(Error::new(ErrorKind::InvalidInput, "the authorized export folder changed; choose it again"));
                }
                pinned.push(PinnedRoot { original: root, resolved: path.clone(), file });
            }
            resolved.push(path);
        }
        if resolved.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, "export needs at least one authorized folder"));
        }
        Ok(Self {
            roots: resolved,
            #[cfg(unix)]
            pinned,
            cancel,
        })
    }

    /// Authorizes one folder, the common case: the app created or picked it, so export may use it
    /// and its subfolders.
    pub fn authorize(root: &Path) -> Result<Self, Error> {
        Self::new([root.to_path_buf()], TransferCancel::default())
    }

    /// The token a host flips to stop an export between files. The desktop app flips this from its
    /// Cancel affordance; a run already mid-file finishes or fails that file. An exclusive new
    /// file can be visible during copying (see the decision record for crash/cleanup limits).
    pub fn cancel(&self) -> &TransferCancel {
        &self.cancel
    }

    /// Whether the destination's spelling lies under a granted root. This is only a preliminary
    /// check: native writes open every component under the root's pinned descriptor, refuse links,
    /// and compare parent identities before publication.
    pub(crate) fn authorizes(&self, destination: &Path) -> bool {
        #[cfg(unix)]
        {
            self.pinned.iter().any(|root| destination.starts_with(&root.original) || destination.starts_with(&root.resolved))
        }
        #[cfg(not(unix))]
        {
            self.roots.iter().any(|root| destination == root || destination.starts_with(root))
        }
    }

    #[cfg(unix)]
    fn pinned_destination<'a>(&'a self, destination: &'a Path) -> Result<secure::Destination<'a>, Error> {
        for root in &self.pinned {
            let relative = destination.strip_prefix(&root.original).or_else(|_| destination.strip_prefix(&root.resolved));
            if let Ok(relative) = relative {
                return secure::Destination::open(&root.file, &root.resolved, relative);
            }
        }
        Err(unsupported(reasons::UNSUPPORTED_TARGET))
    }

    fn destination(&self, destination: &str, for_write: bool) -> Result<PathBuf, Error> {
        #[cfg(unix)]
        let _ = for_write;
        if destination.is_empty() || destination.len() > MAX_DESTINATION_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, "the export destination is missing or longer than 4096 bytes"));
        }
        let path = Path::new(destination);
        if !path.is_absolute() {
            return Err(Error::new(ErrorKind::InvalidInput, "the export destination must be an absolute folder path"));
        }
        if path.components().any(|component| component == std::path::Component::ParentDir) {
            return Err(Error::new(ErrorKind::InvalidInput, "the export destination cannot contain '..'; choose a folder inside the authorized root"));
        }
        // Decide the grant from the spelled path before touching the filesystem, so a destination
        // outside every grant gets the same refusal whether or not it exists.
        #[cfg(unix)]
        if !self.authorizes(path) {
            return Err(unsupported(reasons::UNSUPPORTED_TARGET));
        }
        let resolved = std::fs::canonicalize(path).map_err(|_| Error::new(ErrorKind::InvalidInput, reasons::DESTINATION_MISSING))?;
        if !self.roots.iter().any(|root| resolved.starts_with(root)) {
            return Err(unsupported(reasons::UNSUPPORTED_TARGET));
        }
        #[cfg(unix)]
        {
            // Check the *spelled* path relative to the grant too; canonicalization above
            // checks scope but would otherwise hide a symlinked directory within the root.
            self.pinned_destination(path)?;
            Ok(path.to_path_buf())
        }
        #[cfg(not(unix))]
        {
            if for_write {
                Err(unsupported("secure folder export requires descriptor-relative no-follow file operations on this platform"))
            } else {
                // A plan reads metadata but never publishes a file. It needs an authorized folder,
                // not the Unix-only descriptor-relative writer required by the run command.
                Ok(path.to_path_buf())
            }
        }
    }
}

fn unsupported(reason: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported, reason)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    node: NodeId,
    port: String,
    #[serde(default)]
    item: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    canvas: CanvasId,
    sources: Vec<Source>,
    destination: String,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    on_collision: Option<Collision>,
    #[serde(default)]
    format: Option<nodecraft_format::export::FormatRequest>,
}

/// The title a `{title}` token uses: the person's own title, or the kind's last part.
fn title_of(kind: &str, title: &str) -> String {
    let trimmed = title.trim();
    let value = if trimmed.is_empty() { kind.rsplit('.').next().unwrap_or(kind) } else { trimmed };
    value.chars().take(MAX_NAME_BYTES).collect()
}

fn host<'a>(c: &Context<'a>) -> Result<&'a ExportHost, Error> {
    c.resources.get::<ExportHost>().ok_or_else(|| unsupported(if cfg!(target_arch = "wasm32") { reasons::WASM_HOST } else { reasons::NO_HOST }))
}

/// Validates the request and resolves every source against the document. This may open a
/// directory descriptor to verify the grant but writes no file; `Session` publishes the plan.
fn build(c: &Context<'_>, parameters: &Value, for_write: bool) -> Result<ExportPlan, Error> {
    let request: Request = params(parameters)?;
    if request.sources.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "export needs at least one result to write"));
    }
    if request.sources.len() > MAX_SOURCES {
        return Err(Error::new(ErrorKind::InvalidInput, "an export writes at most 256 results at a time"));
    }
    let host = host(c)?;
    let format = request.format.clone().unwrap_or_default();
    format.validate()?;
    // Refused here, before a single byte: an unimplemented conversion must not write the stored
    // bytes and let the person assume they were converted.
    let action = format.action()?;
    let template = match request.template.as_deref() {
        Some(value) if value.len() > MAX_TEMPLATE_BYTES => {
            return Err(Error::new(ErrorKind::InvalidInput, "the filename template is longer than 128 bytes"));
        }
        Some(value) => Template::parse(value)?,
        None => Template::default(),
    };
    let destination = host.destination(&request.destination, for_write)?;
    let canvas = c.document.canvases().get(&request.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no canvas with this id is open in this project"))?;
    let nodes = canvas.nodes();

    let mut entries = Vec::new();
    for (index, source) in request.sources.iter().enumerate() {
        let position = u32::try_from(index).map_err(|_| Error::new(ErrorKind::InvalidInput, "too many results to export"))?.saturating_add(1);
        entries.push(resolve(c, request.canvas, nodes, source, position, &template, action));
    }
    Ok(ExportPlan { destination, collision: request.on_collision.unwrap_or_default(), format, entries })
}

/// One entry's answer when it cannot become a file: the request that was refused, and why.
fn refused(source: ExportSource, reason: &str) -> PlannedEntry {
    PlannedEntry::Failed { source, reason: reason.to_owned() }
}

/// Resolves one requested source into a file to write, or the reason it cannot become one. A
/// source that cannot be exported fails *its* entry; the rest of the export still runs.
///
/// Which value a `(node, port, generation, item)` means is decided by
/// [`crate::history_cmds::result_value`] — the resolver `history.export`, `history.extract` and
/// `history.copy` share — so a batch can never disagree with a one-file export about what a source
/// refers to, including which list items exist and when a result's port layout no longer matches
/// the node's.
fn resolve(
    c: &Context<'_>,
    canvas: CanvasId,
    nodes: &BTreeMap<NodeId, Node>,
    source: &Source,
    position: u32,
    template: &Template,
    action: FormatAction,
) -> PlannedEntry {
    let request = ExportSource { canvas, node: source.node, port: source.port.clone(), item: source.item, generation: None };
    if source.item.is_some_and(|item| item > MAX_ITEM_INDEX) {
        return refused(request, reasons::ITEM_OUT_OF_RANGE);
    }
    let Some(node) = nodes.get(&source.node) else {
        return refused(request, reasons::UNKNOWN_NODE);
    };
    // The active generation, and nothing else: an export writes the result in use, never the whole
    // history. A node that was never run says so rather than writing a stale or empty file.
    let Some(active) = node.active else {
        return refused(request, reasons::NO_RESULT);
    };
    let held = ExportSource { generation: Some(active), ..request };
    let item = source.item.and_then(|item| usize::try_from(item).ok());
    let selected = match crate::history_cmds::result_value(c, canvas, source.node, active, Some(source.port.as_str()), item) {
        Ok((value, _)) => value,
        Err(error) => return refused(held.clone(), &error.message),
    };
    let (asset, mime, bytes, hash) = match media_for(c.assets, selected) {
        Ok(resolved) => resolved,
        Err(reason) => return refused(held.clone(), reason),
    };
    if [node.kind.as_str(), node.title.as_str(), source.port.as_str()].iter().any(|value| value.contains(['/', '\\']) || value.contains("..")) {
        return refused(held, reasons::NAME_FAILED);
    }
    let title = title_of(&node.kind, &node.title);
    let fields = NameFields {
        index: position,
        node: source.node,
        generation: Some(active),
        kind: &node.kind,
        title: &title,
        output: &source.port,
        item: source.item,
        hash,
    };
    match template.render(&fields, &mime) {
        Ok(name) => PlannedEntry::Ready { source: held, name, mime, bytes, action, asset },
        // The message stays a fixed one: a template and a node title are caller- and file-supplied,
        // and neither belongs in a report.
        Err(_) => refused(held, reasons::NAME_FAILED),
    }
}

/// The `export.local_plan` result: what would be written, and why anything would not be. The
/// destination is not echoed back: the caller named it, and a project's folder path has no reason
/// to travel through a command result.
fn planned_json(plan: &ExportPlan) -> Value {
    // The folder is not read here, so each name is predicted from the batch alone: a repeated name
    // follows the collision policy exactly as `run` applies it, but whether a file already exists
    // on disk is only known when the export runs.
    let ready = ready_names(plan);
    let mut assigned = assign_names(&ready, plan.collision, &|_| Ok(false)).into_iter();
    let entries = plan
        .entries
        .iter()
        .map(|entry| match entry {
            PlannedEntry::Ready { source, name, mime, bytes, action, .. } => {
                let (file, state, reason) = match assigned.next().unwrap_or_else(|| Err(reasons::NAME_FAILED.to_owned())) {
                    Ok(Some(file)) => (file, "planned", None),
                    Ok(None) => (name.file_name(), "skipped", Some(reasons::SKIPPED.to_owned())),
                    Err(reason) => (name.file_name(), "unavailable", Some(reason)),
                };
                json!({
                    "source_id": source.stable_id(),
                    "canvas": source.canvas.to_string(),
                    "node": source.node.to_string(),
                    "port": source.port,
                    "item": source.item,
                    "generation": source.generation.map(|id| id.to_string()),
                    "file": file,
                    "mime": mime,
                    "bytes": bytes,
                    "action": action.name(),
                    "state": state,
                    "reason": reason,
                })
            }
            PlannedEntry::Failed { source, reason } => json!({
                "source_id": source.stable_id(),
                "canvas": source.canvas.to_string(),
                "node": source.node.to_string(),
                "port": source.port,
                "item": source.item,
                "generation": source.generation.map(|id| id.to_string()),
                "reason": reason,
                "state": "unavailable",
            }),
        })
        .collect::<Vec<_>>();
    json!({
        "on_collision": plan.collision.name(),
        "format_mode": plan.format.mode.name(),
        "action": plan.format.action().map(|action| action.name()).unwrap_or("unavailable"),
        "capability": FORMAT_CAPABILITY,
        "template_tokens": TEMPLATE_TOKENS,
        "entries": entries,
    })
}

/// The `export.local_run` result: exactly one entry per request, in the requested order, each with
/// its state and, when it is not written, why. Nothing is ever left out silently.
pub fn outcome_json(outcome: &ExportOutcome) -> Value {
    json!({
        "complete": outcome.complete(),
        "cancelled": outcome.cancelled,
        "partial": !outcome.complete(),
        "counts": {
            "written": outcome.count(EntryState::Written),
            "failed": outcome.count(EntryState::Failed),
            "skipped": outcome.count(EntryState::Skipped),
            "not_started": outcome.count(EntryState::NotStarted),
        },
        "entries": outcome.entries.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

fn entry_json(entry: &EntryOutcome) -> Value {
    json!({
        "source_id": entry.source.stable_id(),
        "canvas": entry.source.canvas.to_string(),
        "node": entry.source.node.to_string(),
        "port": entry.source.port,
        "item": entry.source.item,
        "generation": entry.source.generation.map(|id| id.to_string()),
        "file": entry.file,
        "state": entry.state.name(),
        "reason": entry.reason,
        "bytes": entry.bytes,
        "action": entry.action.map(|action| action.name()),
    })
}

fn source_schema() -> Value {
    json!({"type":"object","properties":{
        "node":{"type":"string","format":"uuid"},
        "port":{"type":"string","minLength":1,"maxLength":256},
        "item":{"type":"integer","minimum":0,"maximum":MAX_ITEM_INDEX}},
        "required":["node","port"],"additionalProperties":false})
}

fn request_schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","properties":{
        "canvas":{"type":"string","format":"uuid"},
        "sources":{"type":"array","minItems":1,"maxItems":MAX_ENTRIES,"items":source_schema()},
        "destination":{"type":"string","minLength":1,"maxLength":MAX_DESTINATION_BYTES,
            "description":"An absolute folder the app has authorized for export; export never creates it."},
        "template":{"type":"string","minLength":1,"maxLength":MAX_TEMPLATE_BYTES,
            "description":"A file-name pattern. Tokens: {index}, {node}, {generation}, {kind}, {title}, {output}, {item}, {hash}. Literal separators and `..` in the pattern are refused. A node title, kind or output containing a separator or `..` refuses that result; other unsafe characters in those values become `_`."},
        "on_collision":{"enum":["skip","overwrite","numbered"],"default":"skip",
            "description":"skip leaves an existing file alone; overwrite replaces it atomically; numbered writes `name (2)`, `name (3)` and so on. New files are exclusive but can be visible while copying; a crash can leave a partial file."},
        "format":{"type":"object","properties":{
            "mode":{"enum":["original","convert","vectorise"],"default":"original",
                "description":"original writes the stored bytes unchanged. convert and vectorise exist so a form can state what is unavailable; both are refused in this build."},
            "mime":{"type":"string","maxLength":MAX_MIME_BYTES,"description":"Required by convert and vectorise; a lowercase type/subtype."}},
            "required":["mode"],"additionalProperties":false}},
        "required":["canvas","sources","destination"],"additionalProperties":false})
}

const PLAN_DOC: &str = "Preview a batch local export without writing anything. Params: {canvas:uuid, sources:[{node:uuid, \
    port:string, item?:int}], destination:string, template?:string, on_collision?:skip|overwrite|numbered, \
    format?:{mode:original|convert|vectorise, mime?:string}}. A source's active generation is its payload; history \
    is never exported, and a text result is refused exactly as history.export refuses it. Returns each file's name \
    with the node and generation it came from, and why any source cannot be written. Names are predicted from the \
    batch alone: whether a file already exists in the folder is only checked when the export runs. Pass-through \
    only: no conversion is performed or implied.";

const RUN_DOC: &str = "Write a batch of results into a folder the app has authorized. Same params as \
    export.local_plan. Each asset is hash-verified and copied through a pinned folder descriptor. Overwrite stages \
    and renames; skip and numbered use exclusive creation, which can be visible partially during copying and can \
    remain incomplete after a crash or failed cleanup. Two results that render to one name follow on_collision as if \
    the earlier file existed, and overwrite refuses the repeat. An entry failure does not stop the others; the result \
    names every entry and why. The destination folder must exist. Cancel stops the run between files.";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "export.local_plan",
            label: "Preview local export",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: request_schema,
            params_doc: PLAN_DOC,
            enabled: |c, v| build(c, v, false).map(|_| ()),
            run: |c, v| build(c, v, false).map(|plan| Effect::None { result: planned_json(&plan) }),
        },
        CommandSpec {
            id: "export.local_run",
            label: "Export files locally",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: request_schema,
            params_doc: RUN_DOC,
            enabled: |c, v| build(c, v, true).map(|_| ()),
            run: |c, v| build(c, v, true).map(|plan| Effect::LocalExport { plan }),
        },
    ]
}

/// Perform `plan`, writing each ready entry into the already-authorized destination.
///
/// Hash-verifies each asset with `history.export`'s shared copying primitive, but keeps its
/// destination and publication operations relative to the host's pinned folder descriptor.
/// Overwrite uses a staged rename. Exclusive new-file creation (`skip`/`numbered`) can be visible
/// while copying; a crash can leave a partial file, and even an ordinary cleanup can fail. This
/// is the intentionally documented weaker guarantee for systems without atomic no-replace links.
pub(crate) fn run(assets: &nodecraft_format::AssetStore, plan: &ExportPlan, host: &ExportHost) -> Result<ExportOutcome, Error> {
    #[cfg(unix)]
    {
        let outcome = host.pinned_destination(&plan.destination).and_then(|destination| run_pinned(assets, plan, host.cancel(), &destination));
        // A cancel stops the export it was pressed for, and must not stop the next one, even when
        // this one was refused before it began.
        host.cancel().reset();
        outcome
    }
    #[cfg(not(unix))]
    {
        let _ = (assets, plan, host);
        Err(unsupported("secure folder export requires descriptor-relative no-follow file operations on this platform"))
    }
}

#[cfg(unix)]
fn run_pinned(
    assets: &nodecraft_format::AssetStore,
    plan: &ExportPlan,
    cancel: &TransferCancel,
    destination: &secure::Destination<'_>,
) -> Result<ExportOutcome, Error> {
    let mut outcome = ExportOutcome::default();
    // Cancelled before the first file: nothing is written, and nothing is left behind to find.
    let mut stopped = cancel.check().is_err();
    outcome.cancelled = stopped;

    let ready = ready_names(plan);
    let taken = |candidate: &str| destination.taken(candidate).map_err(|error| error.message);
    let mut assigned = assign_names(&ready, plan.collision, &taken).into_iter();

    for entry in &plan.entries {
        if stopped {
            outcome.entries.push(not_started(entry));
            continue;
        }
        match entry {
            PlannedEntry::Ready { source, name, action, asset, .. } => {
                let choice = assigned.next().unwrap_or_else(|| Err(reasons::NAME_FAILED.to_owned()));
                let target = match choice {
                    Err(reason) => {
                        outcome.entries.push(outcome_of(source, Some(name.file_name()), EntryState::Failed, Some(reason.as_str()), 0, None));
                        continue;
                    }
                    // The policy says to leave the existing file alone: reported, never dropped.
                    Ok(None) => {
                        outcome.entries.push(outcome_of(source, Some(name.file_name()), EntryState::Skipped, Some(reasons::SKIPPED), 0, None));
                        continue;
                    }
                    Ok(Some(target)) => target,
                };
                match destination.write(assets, asset, &target, plan.collision == Collision::Overwrite) {
                    Ok(bytes) => outcome.entries.push(outcome_of(source, Some(target), EntryState::Written, None, bytes, Some(*action))),
                    Err(error) => outcome.entries.push(outcome_of(source, Some(target), EntryState::Failed, Some(&error.message), 0, None)),
                }
            }
            PlannedEntry::Failed { source, reason } => {
                outcome.entries.push(outcome_of(source, None, EntryState::Failed, Some(reason.as_str()), 0, None));
            }
        }

        // A cancel that arrived during that file takes effect from the next entry onwards.
        if cancel.check().is_err() {
            stopped = true;
            outcome.cancelled = true;
        }
    }
    Ok(outcome)
}

#[cfg(unix)]
fn outcome_of(source: &ExportSource, file: Option<String>, state: EntryState, reason: Option<&str>, bytes: u64, action: Option<FormatAction>) -> EntryOutcome {
    EntryOutcome { source: source.clone(), file, state, reason: reason.map(str::to_owned), bytes, action }
}

/// The outcome an entry gets when the export never reached it.
#[cfg(unix)]
fn not_started(entry: &PlannedEntry) -> EntryOutcome {
    let (source, file) = match entry {
        PlannedEntry::Failed { source, .. } => (source, None),
        PlannedEntry::Ready { source, name, .. } => (source, Some(name.file_name())),
    };
    outcome_of(source, file, EntryState::NotStarted, Some(reasons::NOT_STARTED), 0, None)
}

/// The names of the entries that can become files, in request order.
fn ready_names(plan: &ExportPlan) -> Vec<&ExportName> {
    plan.entries
        .iter()
        .filter_map(|entry| match entry {
            PlannedEntry::Ready { name, .. } => Some(name),
            PlannedEntry::Failed { .. } => None,
        })
        .collect()
}

/// The file each name in `names` will use under `collision`, in order. `Ok(None)` means the policy
/// leaves that entry out; `Err` means it cannot be named.
///
/// `taken` says whether a name already exists in the folder. A name this batch has already claimed
/// counts as taken as well, so two results that render to one name never replace each other. Run
/// passes the pinned folder; a preview passes nothing, and its names are predicted from the batch
/// alone.
///
/// Selection is only advisory on disk: `O_EXCL` in the pinned parent remains the final no-replace
/// authority for new names.
fn assign_names(names: &[&ExportName], collision: Collision, taken: &dyn Fn(&str) -> Result<bool, String>) -> Vec<Result<Option<String>, String>> {
    let mut claimed = std::collections::BTreeSet::new();
    let mut assigned = Vec::with_capacity(names.len());
    for name in names {
        let choice = choose(name, collision, &claimed, taken);
        if let Ok(Some(file)) = &choice {
            claimed.insert(folded(file));
        }
        assigned.push(choice);
    }
    assigned
}

/// A name as the claimed set compares it. macOS and Windows folders are case-insensitive by
/// default, so `Hero.png` and `hero.png` are one file there and must be one name here too.
fn folded(name: &str) -> String {
    name.to_lowercase()
}

fn choose(
    name: &ExportName,
    collision: Collision,
    claimed: &std::collections::BTreeSet<String>,
    taken: &dyn Fn(&str) -> Result<bool, String>,
) -> Result<Option<String>, String> {
    let intended = name.file_name();
    if collision == Collision::Overwrite {
        // Overwrite may replace a file already on disk, but never one this batch has written.
        return if claimed.contains(&folded(&intended)) { Err(reasons::DUPLICATE_NAME.to_owned()) } else { Ok(Some(intended)) };
    }
    // An uncertain lookup is an error, never a guess that the name is free.
    let occupied = |candidate: &str| -> Result<bool, String> { Ok(claimed.contains(&folded(candidate)) || taken(candidate)?) };
    if !occupied(&intended)? {
        return Ok(Some(intended));
    }
    match collision {
        Collision::Skip => Ok(None),
        Collision::Overwrite => Err(reasons::DUPLICATE_NAME.to_owned()),
        Collision::Numbered => {
            let mut suffix = 2_u32;
            while suffix <= MAX_COLLISION_SUFFIX {
                let candidate = name.with_suffix(suffix);
                if !occupied(&candidate)? {
                    return Ok(Some(candidate));
                }
                suffix = suffix.saturating_add(1);
            }
            Err(reasons::NO_FREE_NAME.to_owned())
        }
    }
}

#[cfg(test)]
mod tests;
