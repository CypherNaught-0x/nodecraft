//! Golden graphs: run a canvas end to end, record each result as the engine would, and compare
//! what every step produced with a blessed JSON file. Set `NODECRAFT_BLESS=1` to write the files
//! instead.
//!
//! The run goes through the planner, the run executor ([`Run`]), `engine::generate` and the
//! providers' `run_to_completion`, one job at a time and without retries. It doesn't exercise the
//! scheduler or driver (lanes, retries, cancellation): their own tests do.

use nodecraft_engine::generate;
use nodecraft_exec::planner::{Planned, RunMode};
use nodecraft_exec::run::{Progress, Run, RunLimits, RunOptions, record_changes};
use nodecraft_exec::scheduler::{self, RetryPolicy};
use nodecraft_graph::{Changeset, Project};
use nodecraft_nodes::{JobSpec, Registry};
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, Provider, run_to_completion};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The run id golden runs use, so their keys and generation ids are stable.
pub const RUN_ID: [u8; 16] = [0x60; 16];

/// A cancel signal that never fires.
pub struct NeverCancelled;
impl CancelSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> BoxFuture<'_, ()> {
        Box::pin(futures::future::pending())
    }
}

/// Virtual time: sleeping advances it at once.
#[derive(Default)]
pub struct InstantClock(AtomicU64);
impl Clock for InstantClock {
    fn sleep(&self, ms: u64) -> BoxFuture<'static, ()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// What a golden run produced.
pub struct Outcome {
    /// The project with every result recorded (as the engine records them).
    pub project: Project,
    /// Per step, by node title: its outputs, or why it failed. Ready to compare as JSON.
    pub report: Json,
}

fn fail(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message.to_owned())
}

/// Plans `requested` on `canvas` in `mode`, runs it with `providers` (by id), and records the
/// results. Nodes in the run need distinct titles (the report is keyed by them).
pub fn run(
    project: &Project,
    canvas: CanvasId,
    requested: &BTreeSet<NodeId>,
    mode: &RunMode,
    kinds: &Registry,
    providers: &[Arc<dyn Provider>],
) -> Result<Outcome, Error> {
    let live = project.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such canvas"))?;
    let mut titles = BTreeMap::new();
    for node in live.nodes().values() {
        if titles.insert(node.title.clone(), node.id).is_some() {
            return Err(fail("golden graphs need distinct node titles"));
        }
    }
    let title = |id: NodeId| live.nodes().get(&id).map_or_else(|| id.to_string(), |n| n.title.clone());
    let stale = nodecraft_exec::staleness::stale(live, kinds)?;
    let mut targets = nodecraft_types::TargetSelection::empty();
    for node in live.nodes().keys() {
        for provider in providers {
            targets.insert(*node, &nodecraft_types::ProviderTarget::default_instance(provider.id())?)?;
        }
    }
    let plan = match nodecraft_exec::planner::plan_with_targets(live, kinds, mode, requested, &stale, &targets)? {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => {
            let reasons: Vec<String> = blockers.iter().map(|b| b.message(live)).collect();
            return Ok(Outcome { project: project.clone(), report: json!({"blocked": reasons}) });
        }
    };
    let options = RunOptions { id: RUN_ID, first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
    nodecraft_exec::admission::measure::canvas(live)?;
    let catalog = crate::catalog::mock_catalog()?;
    let (admitted, quote) = nodecraft_exec::admission::admit_run_with_targets(plan, canvas, live.clone(), kinds, &catalog, Default::default(), &targets)?;
    if !nodecraft_exec::cost::check(&quote, &Default::default(), &Default::default())?.allowed() {
        return Err(fail("golden admission is blocked"));
    }
    let (mut run, first) = Run::start_admitted(admitted, kinds, options)?;
    let clock = InstantClock::default();
    let (mut finished, mut failed) = (BTreeMap::new(), BTreeMap::new());
    let mut requests: BTreeMap<NodeId, Vec<Json>> = BTreeMap::new();
    let mut queue: VecDeque<scheduler::JobSpec> = VecDeque::new();
    let mut absorb = |progress: Progress, queue: &mut VecDeque<scheduler::JobSpec>| {
        queue.extend(progress.jobs);
        finished.extend(progress.finished.into_iter().map(|r| (r.node, r.generation)));
        failed.extend(progress.failed.into_iter().map(|f| (f.node, f.reason)));
    };
    absorb(first, &mut queue);
    while let Some(job) = queue.pop_front() {
        let spec = run.work(job.id).cloned().ok_or_else(|| Error::new(ErrorKind::Internal, "a scheduled job has no work"))?;
        let progress = match &spec {
            JobSpec::Source { output, value, .. } => run.job_done(kinds, job.id, vec![(output.clone(), value.clone())]),
            JobSpec::Action { .. } => run.job_failed(kinds, job.id, "the golden runner has no action executor"),
            JobSpec::Generate { node, outputs, .. } => {
                let attempt = generate::provider_job(&spec, &job.idempotency_key).and_then(|(provider, request)| {
                    let inputs: Vec<Json> = request
                        .inputs
                        .iter()
                        .map(|input| match input {
                            nodecraft_providers::JobInput::Text { role, text } => json!({"role": role, "text": text}),
                            nodecraft_providers::JobInput::Media { role, mime, bytes } => json!({"role": role, "mime": mime, "bytes": bytes.len()}),
                        })
                        .collect();
                    requests.entry(*node).or_default().push(
                        json!({"provider": provider, "model": request.model, "params": request.params, "inputs": inputs, "key": request.idempotency_key}),
                    );
                    let adapter =
                        providers.iter().find(|p| p.id() == provider).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no provider {provider}")))?;
                    let done = futures::executor::block_on(run_to_completion(
                        adapter.as_ref(),
                        &request,
                        &NeverCancelled,
                        &clock,
                        nodecraft_providers::RunLimits::default(),
                        |_| {},
                    ))
                    .map_err(|failure| Error::new(ErrorKind::Unsupported, failure.to_string()))?;
                    generate::produced(outputs, &done)
                });
                match attempt {
                    Ok(produced) => run.job_done(kinds, job.id, produced.outputs),
                    Err(error) => run.job_failed(kinds, job.id, &error.message),
                }
            }
        };
        absorb(progress, &mut queue);
    }
    if !run.is_finished() {
        return Err(Error::new(ErrorKind::Internal, "the run ended with steps still open"));
    }
    let mut recorded = project.clone();
    for (node, generation) in &finished {
        let current = recorded
            .canvases()
            .get(&canvas)
            .and_then(|c| c.nodes().get(node))
            .cloned()
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "a run node vanished"))?;
        Changeset::new(record_changes(canvas, &current, generation.clone())?).apply(&mut recorded)?;
    }
    let mut report = serde_json::Map::new();
    for (node, generation) in &finished {
        report.insert(title(*node), json!({"outputs": generation.outputs, "stamped": generation.input_hash.is_some(), "requests": requests.get(node)}));
    }
    for (node, reason) in &failed {
        report.insert(title(*node), json!({"failed": reason, "requests": requests.get(node)}));
    }
    Ok(Outcome { project: recorded, report: Json::Object(report) })
}

/// Whether this run blesses golden files (`NODECRAFT_BLESS=1`).
pub fn blessing() -> bool {
    std::env::var("NODECRAFT_BLESS").is_ok_and(|v| v == "1")
}

/// Compares `actual` with the golden file `dir/<name>.json`, or writes it when blessing. Line
/// endings don't matter (Windows checkouts may convert them). `Err` explains a mismatch or a
/// missing file.
pub fn check(dir: &Path, name: &str, actual: &Json) -> Result<(), String> {
    let path = dir.join(format!("{name}.json"));
    let pretty = serde_json::to_string_pretty(actual).map_err(|e| e.to_string())? + "\n";
    if blessing() {
        return std::fs::write(&path, pretty).map_err(|e| format!("writing {}: {e}", path.display()));
    }
    let expected =
        std::fs::read_to_string(&path).map_err(|e| format!("{}: {e} (run with NODECRAFT_BLESS=1 to create it)", path.display()))?.replace("\r\n", "\n");
    if expected == pretty { Ok(()) } else { Err(format!("{} differs from this run:\n--- expected\n{expected}\n--- actual\n{pretty}", path.display())) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_files_compare_regardless_of_line_endings() {
        let dir = std::env::temp_dir().join(format!("nodecraft-golden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let value = json!({"a": [1, 2]});
        let lf = serde_json::to_string_pretty(&value).unwrap() + "\n";
        std::fs::write(dir.join("crlf.json"), lf.replace('\n', "\r\n")).unwrap();
        if !blessing() {
            assert!(check(&dir, "crlf", &value).is_ok());
            assert!(check(&dir, "crlf", &json!({"a": [1]})).is_err());
            assert!(check(&dir, "missing", &value).unwrap_err().contains("NODECRAFT_BLESS=1"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
