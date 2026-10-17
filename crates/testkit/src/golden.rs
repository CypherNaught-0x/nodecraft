//! Golden graphs: run a canvas end to end (plan, run executor, provider calls through
//! `run_to_completion`), record each result as the engine would, and compare what every step
//! produced with a blessed JSON file. Set `NODECRAFT_BLESS=1` to write the files instead.

use nodecraft_engine::generate;
use nodecraft_exec::planner::{Planned, RunMode, plan};
use nodecraft_exec::run::{Progress, Run, RunLimits, RunOptions, record_changes};
use nodecraft_exec::scheduler::RetryPolicy;
use nodecraft_graph::{Changeset, Project};
use nodecraft_nodes::{JobSpec, Registry};
use nodecraft_providers::{BoxFuture, CancelSignal, Clock, Provider, run_to_completion};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The run id golden runs use, so their keys and generation ids are stable.
pub const RUN_ID: [u8; 16] = [0x60; 16];

struct NeverCancelled;
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
struct InstantClock(AtomicU64);
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

fn name(project: &Project, canvas: CanvasId, node: NodeId) -> String {
    project
        .canvases()
        .get(&canvas)
        .and_then(|c| c.nodes().get(&node))
        .map(|n| if n.title.is_empty() { node.to_string() } else { n.title.clone() })
        .unwrap_or_else(|| node.to_string())
}

/// Plans `requested` on `canvas` in `mode`, runs it with `providers` (by id), and records the
/// results. Steps run one job at a time, in a deterministic order.
pub fn run(
    project: &Project,
    canvas: CanvasId,
    requested: &BTreeSet<NodeId>,
    mode: &RunMode,
    kinds: &Registry,
    providers: &[Arc<dyn Provider>],
) -> Result<Outcome, Error> {
    let live = project.canvases().get(&canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such canvas"))?;
    let stale = nodecraft_exec::staleness::stale(live, kinds)?;
    let plan = match plan(live, kinds, mode, requested, &stale)? {
        Planned::Ready(plan) => plan,
        Planned::Blocked(blockers) => {
            let reasons: Vec<String> = blockers.iter().map(|b| b.message(live)).collect();
            return Ok(Outcome { project: project.clone(), report: json!({"blocked": reasons}) });
        }
    };
    let options = RunOptions { id: RUN_ID, first_job: 1, limits: RunLimits::default(), retry: RetryPolicy::NEVER };
    let (mut run, first) = Run::start(&plan, canvas, live, kinds, options)?;
    let clock = InstantClock::default();
    let mut finished = BTreeMap::new();
    let mut failed = BTreeMap::new();
    let mut queue = std::collections::VecDeque::new();
    let mut absorb = |progress: Progress, queue: &mut std::collections::VecDeque<nodecraft_exec::scheduler::JobSpec>| {
        queue.extend(progress.jobs);
        for result in progress.finished {
            finished.insert(result.node, result.generation);
        }
        for failure in progress.failed {
            failed.insert(failure.node, failure.reason);
        }
    };
    absorb(first, &mut queue);
    while let Some(job) = queue.pop_front() {
        let progress = match run.work(job.id).cloned() {
            Some(JobSpec::Source { output, value, .. }) => run.job_done(kinds, job.id, vec![(output, value)]),
            Some(spec @ JobSpec::Generate { .. }) => {
                let JobSpec::Generate { output, .. } = &spec else { continue };
                let attempt = generate::provider_job(&spec, &job.idempotency_key).and_then(|(provider, request)| {
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
                    generate::produced(output, &done)
                });
                match attempt {
                    Ok(produced) => run.job_done(kinds, job.id, produced.outputs),
                    Err(error) => run.job_failed(job.id, &error.message),
                }
            }
            None => continue,
        };
        absorb(progress, &mut queue);
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
        report.insert(name(project, canvas, *node), json!({"outputs": generation.outputs, "stamped": generation.input_hash.is_some()}));
    }
    for (node, reason) in &failed {
        report.insert(name(project, canvas, *node), json!({"failed": reason}));
    }
    Ok(Outcome { project: recorded, report: Json::Object(report) })
}

/// Compares `actual` with the golden file `dir/<name>.json`, or writes it when `NODECRAFT_BLESS`
/// is set. `Err` explains a mismatch or a missing file.
pub fn check(dir: &Path, name: &str, actual: &Json) -> Result<(), String> {
    let path = dir.join(format!("{name}.json"));
    let pretty = serde_json::to_string_pretty(actual).map_err(|e| e.to_string())? + "\n";
    if std::env::var_os("NODECRAFT_BLESS").is_some() {
        return std::fs::write(&path, pretty).map_err(|e| format!("writing {}: {e}", path.display()));
    }
    let expected = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e} (run with NODECRAFT_BLESS=1 to create it)", path.display()))?;
    if expected == pretty { Ok(()) } else { Err(format!("{} differs from this run:\n--- expected\n{expected}\n--- actual\n{pretty}", path.display())) }
}
