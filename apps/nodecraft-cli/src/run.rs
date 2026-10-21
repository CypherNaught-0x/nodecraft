//! `run`: runs a project's stale or chosen nodes headlessly and exports what they make.
//!
//! The run is the canvas's own: the engine's `run.quote` prices it and `run.start` takes the
//! quote's token, so the budgets and the consent the app checks are checked here too. A paid or
//! partly priced run starts only with `--yes`; the CLI never asks on a terminal, so a script can't
//! hang on a prompt. Jobs run on a small tokio runtime, as the app's do, and the CLI waits for
//! them. Results are exported with `history.export` and never replace a file; the project file
//! itself is left unchanged.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nodecraft_engine::Session;
use nodecraft_engine::commands::Services;
use nodecraft_engine::jobs::JobBudget;
use nodecraft_engine::run_host::{RunHost, RunNotice};
use nodecraft_exec::cost::{Budgets, UsageLog};
use nodecraft_exec::driver::{Runtime, TaskFuture};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value as Output};
use serde_json::{Value, json};

/// How often the CLI collects what the run did.
const PUMP_EVERY: Duration = Duration::from_millis(20);
/// Results of one list output exported at most, as the app's menu offers.
const MAX_ITEMS: usize = 64;
/// Characters of a card's title kept in an exported file's name.
const MAX_TITLE: usize = 40;
/// Numbered names tried for one result when its name is taken (`-2` up to this).
const MAX_TAKEN: usize = 99;

/// The engine's runtime over the CLI's tokio runtime.
struct Tokio {
    handle: tokio::runtime::Handle,
    start: Instant,
}

impl Runtime for Tokio {
    fn spawn(&self, task: TaskFuture<()>) {
        self.handle.spawn(task);
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        // Spawned, so the timer belongs to the runtime whichever thread asks; dropped, it stops.
        let timer = Timer(self.handle.spawn(tokio::time::sleep(Duration::from_millis(ms))));
        Box::pin(async move {
            let mut timer = timer;
            let _ = (&mut timer.0).await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A sleep's timer task, aborted when the sleep is dropped unfinished.
struct Timer(tokio::task::JoinHandle<()>);
impl Drop for Timer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the person running the CLI agreed to spend.
#[derive(Clone, Copy, Debug, Default)]
pub struct Consent {
    /// `--yes`: a priced, partly priced or unpriced run may start.
    pub spend: bool,
}

/// The run host and the runtime its jobs run on.
pub struct Runner {
    host: RunHost,
    tokio: Option<tokio::runtime::Runtime>,
}

impl Runner {
    /// A runner whose commands are registered in `services`; give them to the session, then
    /// [`Self::install`] it.
    pub fn new(services: &mut Services, session_id: String) -> Result<Self, Error> {
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("nodecraft-runs")
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the run threads: {e}")))?;
        let runtime = Arc::new(Tokio { handle: tokio.handle().clone(), start: Instant::now() });
        let budget = JobBudget {
            log: Arc::new(Mutex::new(UsageLog::default())),
            budgets: Arc::new(Mutex::new(Budgets::default())),
            session: session_id,
            wall_clock_ms: Arc::new(|| {
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            }),
        };
        let host = RunHost::new(runtime, budget);
        host.register(services)?;
        host.register_ownership(services)?;
        host.register_resolution(services)?;
        Ok(Self { host, tokio: Some(tokio) })
    }

    /// Hands the run host's budget state to `session`; call after the providers are routed.
    pub fn install(&self, session: &mut Session) {
        self.host.install(session);
    }

    /// Runs `targets` with their stale upstream (every stale card when empty) once `quote`'s
    /// price is agreed to, and waits until the run ends. Returns each step's outcome.
    pub fn run(&mut self, session: &mut Session, canvas: CanvasId, targets: &[NodeId], quote: &Value, consent: Consent) -> Result<Vec<Value>, Error> {
        let accept = accept(quote, consent)?;
        let params = if targets.is_empty() {
            json!({"canvas": canvas, "mode": "all_stale", "accept": accept})
        } else {
            json!({"canvas": canvas, "mode": "with_upstream", "nodes": targets, "accept": accept})
        };
        session.dispatch("run.start", &params)?;
        let mut steps = Vec::new();
        let mut finished = false;
        loop {
            for notice in self.host.pump(session) {
                match notice {
                    RunNotice::Started { .. } | RunNotice::Unsaved { .. } => {}
                    RunNotice::Recorded { node, .. } => steps.push(json!({"node": node, "status": "recorded"})),
                    RunNotice::Failed { node, reason, .. } => steps.push(json!({"node": node, "status": "failed", "reason": reason})),
                    RunNotice::Cancelled { node, .. } => steps.push(json!({"node": node, "status": "cancelled"})),
                    RunNotice::Unaccounted { node, reason, .. } => steps.push(json!({"node": node, "status": "unaccounted", "reason": reason})),
                    RunNotice::Refused { reason } => return Err(Error::new(ErrorKind::Conflict, format!("the run didn't start: {reason}"))),
                    // The rest of this batch is still read, so no outcome is lost.
                    RunNotice::Finished { canvas: done } => finished |= done == canvas,
                }
            }
            if finished || !self.host.is_busy() {
                return Ok(steps);
            }
            std::thread::sleep(PUMP_EVERY);
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.host.shut_down();
        if let Some(tokio) = self.tokio.take() {
            tokio.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

/// The `accept` a quote takes, or why it can't be accepted as given.
fn accept(quote: &Value, consent: Consent) -> Result<Value, Error> {
    let refused = |message: String| Error::new(ErrorKind::Budget, message);
    let token = quote["token"].as_str().ok_or_else(|| Error::new(ErrorKind::Internal, "the quote has no token"))?;
    if quote["allowed"].as_bool() != Some(true) {
        let reasons: Vec<&str> = ["blocked", "blocks"].iter().flat_map(|key| quote[*key].as_array().into_iter().flatten()).filter_map(Value::as_str).collect();
        return Err(refused(format!("the run isn't allowed: {}", reasons.join("; "))));
    }
    let steps = quote["steps"].as_array().map_or(&[][..], Vec::as_slice);
    if quote["steps_total"].as_u64().is_some_and(|total| u64::try_from(steps.len()).is_ok_and(|n| n != total)) {
        return Err(refused("the run has more steps than its quote lists; run fewer nodes with `--node`".into()));
    }
    let mut amounts = serde_json::Map::new();
    let mut charged = false;
    for (code, total) in quote["totals"].as_object().into_iter().flatten() {
        let micros = total["micros"].as_u64().ok_or_else(|| Error::new(ErrorKind::Internal, "the quote's total is unreadable"))?;
        charged |= micros > 0;
        amounts.insert(code.clone(), json!(micros));
    }
    let mut unpriced = 0u64;
    for step in steps.iter().filter(|step| step["unpriced"].is_string()) {
        // A step whose job count is unknown counts as one job, as it does for its price.
        unpriced = unpriced.saturating_add(step["jobs"].as_u64().unwrap_or(1));
    }
    let complete = quote["complete"].as_bool() == Some(true);
    if (charged || !complete || unpriced > 0) && !consent.spend {
        return Err(refused("the run costs money or its price isn't fully known; check `quote`, then pass `--yes` to run it".into()));
    }
    let mut accept = json!({"token": token, "amounts": amounts});
    if !complete {
        accept["partial"] = json!(true);
    }
    if unpriced > 0 {
        accept["unpriced_jobs"] = json!(unpriced);
    }
    Ok(accept)
}

/// The `--out` folder, resolved; checked before anything runs, so a typo costs nothing.
pub fn out_folder(out: &Path) -> Result<PathBuf, Error> {
    let out = std::fs::canonicalize(out).map_err(|_| Error::new(ErrorKind::NotFound, "the `--out` folder doesn't exist"))?;
    if !out.is_dir() {
        return Err(Error::new(ErrorKind::InvalidInput, "`--out` must be a folder"));
    }
    Ok(out)
}

/// Exports the active media results of `nodes` into the folder `out` (from [`out_folder`]),
/// never replacing a file: a taken name gets a number. Returns each file written or why it
/// wasn't, and the text results, which have no file. One failed file doesn't stop the rest.
pub fn export(session: &mut Session, canvas: CanvasId, nodes: &[NodeId], out: &Path) -> Result<Vec<Value>, Error> {
    let graph = session.document().canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such canvas"))?;
    let mut planned = Vec::new();
    let mut texts = Vec::new();
    for id in nodes {
        let Some(node) = graph.nodes().get(id) else { continue };
        let Some(generation) = node.active.and_then(|active| node.history.iter().find(|g| g.id == active)) else { continue };
        // A result recorded for another port layout can't be matched to its ports.
        if node.outputs.len() != generation.outputs.len() {
            continue;
        }
        let stem = stem(&node.title, &node.kind, *id);
        for (port, value) in node.outputs.iter().zip(&generation.outputs) {
            match value {
                Output::Media(asset) => {
                    planned.push((*id, generation.id, port.name.clone(), None, name(&stem, &port.name, None), extension(asset.mime.essence())))
                }
                Output::List(items) => {
                    let media = nodecraft_engine::history_cmds::list_items(items, !generation.items.is_empty()).take(MAX_ITEMS).enumerate();
                    for (item, value) in media {
                        if let Output::Media(asset) = value {
                            planned.push((
                                *id,
                                generation.id,
                                port.name.clone(),
                                Some(item),
                                name(&stem, &port.name, Some(item)),
                                extension(asset.mime.essence()),
                            ));
                        }
                    }
                }
                Output::Text(text) => texts.push(json!({"node": id, "port": port.name, "text": text})),
                _ => {}
            }
        }
    }
    let mut written = Vec::new();
    for (node, generation, port, item, name, extension) in planned {
        let mut params = json!({"canvas": canvas, "id": node, "generation": generation, "port": port, "overwrite": false});
        if let Some(item) = item {
            params["item"] = json!(item);
        }
        let mut outcome = Err(Error::new(ErrorKind::Conflict, "every name for this result is taken"));
        for taken in 1..=MAX_TAKEN {
            let numbered = if taken == 1 { String::new() } else { format!("-{taken}") };
            let path = out.join(format!("{name}{numbered}.{extension}"));
            params["path"] = json!(path);
            match session.dispatch("history.export", &params) {
                Err(error) if error.kind == ErrorKind::Conflict => continue,
                done => {
                    outcome = done.map(|done| (path, done));
                    break;
                }
            }
        }
        written.push(match outcome {
            Ok((path, done)) => json!({"node": node, "port": port, "item": item, "path": path, "bytes": done["bytes"]}),
            Err(error) => json!({"node": node, "port": port, "item": item, "status": "failed", "reason": error.message}),
        });
    }
    written.extend(texts);
    Ok(written)
}

/// A file name's start for a card: its title (or kind) made safe, and its id's first characters
/// so two cards with one title don't collide.
fn stem(title: &str, kind: &str, id: NodeId) -> String {
    let named = if title.trim().is_empty() { kind } else { title };
    let id = id.to_string();
    format!("{}-{}", slug(named), id.get(..8).unwrap_or(&id))
}

/// `text`'s ASCII letters and digits, lowercased, with each other run as one `-`.
fn slug(text: &str) -> String {
    let mut safe = String::new();
    for c in text.chars().take(MAX_TITLE) {
        if c.is_ascii_alphanumeric() {
            safe.push(c.to_ascii_lowercase());
        } else if !safe.ends_with('-') {
            safe.push('-');
        }
    }
    safe.trim_matches('-').to_owned()
}

/// A result's file name, less its extension and any number added when it is taken.
fn name(stem: &str, port: &str, item: Option<usize>) -> String {
    let port = slug(port);
    let port = if port.is_empty() { "out".to_owned() } else { port };
    let item = item.map(|item| format!("-{}", item.saturating_add(1))).unwrap_or_default();
    format!("{stem}-{port}{item}")
}

/// The usual extension of a result of type `mime`.
fn extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "model/gltf-binary" => "glb",
        "model/gltf+json" => "gltf",
        "model/obj" => "obj",
        "application/pdf" => "pdf",
        _ => "bin",
    }
}

/// What a run did, in words.
pub fn report_text(report: &Value) -> String {
    let mut out = String::new();
    for step in report["steps"].as_array().into_iter().flatten() {
        let node = step["node"].as_str().unwrap_or_default();
        match step["reason"].as_str() {
            Some(reason) => out.push_str(&format!("{node}: {}: {reason}\n", step["status"].as_str().unwrap_or_default())),
            None => out.push_str(&format!("{node}: {}\n", step["status"].as_str().unwrap_or_default())),
        }
    }
    for result in report["results"].as_array().into_iter().flatten() {
        if let Some(path) = result["path"].as_str() {
            out.push_str(&format!("wrote {path}\n"));
        } else if let Some(reason) = result["reason"].as_str() {
            out.push_str(&format!("{}: not exported: {reason}\n", result["node"].as_str().unwrap_or_default()));
        } else if let Some(text) = result["text"].as_str() {
            out.push_str(&format!("{}:\n{text}\n", result["node"].as_str().unwrap_or_default()));
        }
    }
    if out.is_empty() {
        out.push_str("Nothing needed running.\n");
    }
    out
}

#[cfg(test)]
mod tests;
