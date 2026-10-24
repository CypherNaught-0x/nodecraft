//! Following jobs submitted before a restart (#508). A restored job is polled from its saved
//! handle, never sent again: its runner registration is resume-only. One saved with only the
//! request its adapter staged (#664) has that request sent again exactly, under its key, and is
//! polled from the handle that brings. When it ends its charge
//! settles like any other job's, and a finished one's result is recorded on its card when it can
//! be (see [`super::RunHost::resume`]).
use super::held::{self, Held, HeldResult};
use super::{HELD_NOTE, RunNotice, lock};
use crate::Session;
use crate::generate;
use crate::jobs::{CompletionAccounting, CompletionDelivery, JobBudget, JobPrice, ProviderRunner, QueuedJob, Sent};
use crate::provider_routing::RegisteredProvider;
use crate::run_jobs::{JobKey, JobState, JobView, Reports, RunJobs};
use crate::run_journal::Submitted;
use nodecraft_exec::driver::{CancelToken, JobRunner, Runtime};
use nodecraft_exec::run::ResumeRecipe;
use nodecraft_exec::scheduler::{JobId, Lane, Outcome, Start};
use nodecraft_nodes::GenerateOutput;
use nodecraft_providers::{Completed, ProviderJob, RunUpdate};
use nodecraft_types::{CanvasId, NodeId};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The [`JobKey::run`] of restored jobs; runs started in this session count from 1.
pub(super) const RESUMED_RUN: u64 = 0;
/// Attempts at following a restored job when the provider can't be reached, before it is kept
/// for the next launch.
const ATTEMPTS: u32 = 3;
/// The wait before the next attempt, doubled each time.
const BACKOFF_MS: u64 = 2_000;

#[derive(Default)]
struct Mailbox {
    deliveries: BTreeMap<JobId, CompletionDelivery>,
    outcomes: Vec<(JobId, Outcome)>,
    reports: Reports,
    /// Handles a staged request sent again got back (#664).
    submitted: Vec<(JobId, nodecraft_providers::JobHandle)>,
}

/// Restored jobs being followed.
pub(super) struct Resumes {
    runner: Option<Arc<ProviderRunner>>,
    mailbox: Arc<Mutex<Mailbox>>,
    next: u64,
    pub(super) live: BTreeMap<JobId, (Submitted, CancelToken)>,
    pub(super) jobs: RunJobs,
}

impl Default for Resumes {
    fn default() -> Self {
        Self { runner: None, mailbox: Arc::default(), next: 1, live: BTreeMap::new(), jobs: RunJobs::default() }
    }
}

/// How a restored job ended, for the host.
pub(super) enum Ended {
    /// It ended (finished, failed or was cancelled); its charge has settled.
    Settled,
    /// The provider couldn't be reached: keep it for the next launch.
    Kept(Box<Submitted>),
    /// It finished while its project was closed: wait for the project, then follow it again to
    /// record its result. Its charge has settled, so it isn't charged again.
    Parked(Box<Submitted>),
}

impl Resumes {
    fn runner(&mut self, runtime: &Arc<dyn Runtime>, budget: &JobBudget) -> Arc<ProviderRunner> {
        if let Some(runner) = &self.runner {
            return Arc::clone(runner);
        }
        let deliveries = Arc::clone(&self.mailbox);
        let reports = Arc::clone(&self.mailbox);
        let runner = Arc::new(
            ProviderRunner::new(
                Arc::clone(runtime),
                nodecraft_providers::RunLimits::default(),
                Arc::new(move |job, delivery| {
                    lock(&deliveries).deliveries.insert(job, delivery);
                }),
                Arc::new(move |job, sent: Sent<'_>, update: RunUpdate| {
                    let mut mailbox = lock(&reports);
                    if let RunUpdate::Submitted(handle) = &update {
                        mailbox.submitted.push((job, handle.clone()));
                    }
                    mailbox.reports.note(job, sent.attempt, &update);
                }),
            )
            .with_budget(budget.clone()),
        );
        self.runner = Some(Arc::clone(&runner));
        runner
    }

    /// Starts following `entry` on `binding`, its provider.
    pub(super) fn start(
        &mut self,
        entry: Submitted,
        binding: &RegisteredProvider,
        runtime: &Arc<dyn Runtime>,
        budget: &JobBudget,
    ) -> Result<(), Box<(Submitted, String)>> {
        let runner = self.runner(runtime, budget);
        let id = JobId(self.next);
        let lane = match Lane::for_target(binding.target()) {
            Ok(lane) => lane,
            Err(error) => return Err(Box::new((entry, error.message))),
        };
        let job = ProviderJob { model: entry.model.clone(), params: Default::default(), inputs: Vec::new(), idempotency_key: entry.key.clone() };
        // A resumed job carries no token rates (#595): one that reports tokens but no cost settles
        // at its estimate. Chat adapters answer from submit, so they save no handle to resume.
        let price = entry.priced.then(|| JobPrice { project: Some(entry.project), ..JobPrice::new(entry.node, entry.model.clone(), entry.estimate) });
        runner.register(id, QueuedJob::resume(binding.clone(), job, price));
        let restored = match &entry.staged {
            Some(staged) if entry.handle.remote_id.is_empty() => runner.restore_target_staged(binding, entry.key.clone(), entry.generation, staged.clone()),
            _ => runner.restore_target_generation(binding, entry.key.clone(), entry.generation, entry.handle.clone()),
        };
        if let Err(error) = restored {
            runner.forget(id);
            return Err(Box::new((entry, error.message)));
        }
        self.next = self.next.saturating_add(1);
        let cancel = CancelToken::default();
        let view = JobView {
            key: JobKey { run: RESUMED_RUN, job: id },
            canvas: entry.canvas,
            node: entry.node,
            provider: Some(entry.target.adapter().to_owned()),
            model: Some(entry.model.clone()),
            estimate: entry.estimate,
            state: JobState::Queued,
            sent: true,
            cancel_requested: false,
            cancel_refused: false,
            reported_sent: true,
            partial: None,
            partial_attempt: 0,
            preview: None,
            made: None,
        };
        self.jobs.live.insert(id, view);
        self.jobs.started(id, 1);
        let start = Start { job: id, lane, attempt: 1, idempotency_key: entry.key.clone() };
        self.live.insert(id, (entry, cancel.clone()));
        let mailbox = Arc::clone(&self.mailbox);
        let clock = Arc::clone(runtime);
        runtime.spawn(Box::pin(async move {
            let mut start = start;
            let mut wait = BACKOFF_MS;
            let outcome = loop {
                let outcome = runner.run(start.clone(), cancel.clone()).await;
                match outcome {
                    Outcome::Failed { transient: true, .. } if start.attempt < ATTEMPTS && !cancel.is_cancelled() => {
                        clock.sleep(wait).await;
                        wait = wait.saturating_mul(2);
                        start.attempt = start.attempt.saturating_add(1);
                    }
                    // Cancelled while waiting to try again: stop following it.
                    Outcome::Failed { transient: true, .. } if cancel.is_cancelled() => break Outcome::Cancelled,
                    outcome => break outcome,
                }
            };
            lock(&mailbox).outcomes.push((id, outcome));
        }));
        Ok(())
    }

    /// Asks a restored job to stop; whether it was one.
    pub(super) fn cancel(&mut self, job: JobId) -> bool {
        let Some((_, cancel)) = self.live.get(&job) else { return false };
        cancel.cancel();
        if let Some(view) = self.jobs.live.get_mut(&job) {
            view.cancel_requested = true;
        }
        true
    }

    /// Absorbs what the restored jobs reported: records finished ones, settles every one that
    /// ended. Returns how each ended, and whether a job sent again got a handle to save.
    pub(super) fn absorb(&mut self, session: &mut Session, budget: &JobBudget, held: &Mutex<Held>, notices: &mut Vec<RunNotice>) -> (Vec<Ended>, bool) {
        let (outcomes, mut reports, submitted) = {
            let mut mailbox = lock(&self.mailbox);
            (std::mem::take(&mut mailbox.outcomes), std::mem::take(&mut mailbox.reports), std::mem::take(&mut mailbox.submitted))
        };
        let mut handled = false;
        for (id, handle) in submitted {
            if let Some((entry, _)) = self.live.get_mut(&id) {
                // Followed by its handle from here on, after another restart too.
                entry.handle = handle;
                entry.staged = None;
                handled = true;
            }
        }
        self.jobs.absorb_reports(&mut reports);
        let mut ended = Vec::new();
        for (id, outcome) in outcomes {
            let Some((entry, _)) = self.live.remove(&id) else { continue };
            let delivery = lock(&self.mailbox).deliveries.remove(&id);
            let (canvas, node) = (entry.canvas, entry.node);
            let runner = self.runner.clone();
            let forget = || {
                if let Some(runner) = &runner {
                    runner.forget(id);
                }
            };
            let elsewhere = session.document().id() != entry.project;
            match (outcome, delivery) {
                (Outcome::Done, Some(_)) if elsewhere => {
                    forget();
                    let reason = "it finished while its project was closed; it is recorded when the project is open again".to_owned();
                    self.jobs.end(id, JobState::Failed { reason: reason.clone(), cause: None });
                    notices.push(RunNotice::Failed { canvas, node, reason });
                    ended.push(Ended::Parked(Box::new(entry)));
                }
                (Outcome::Done, Some(delivery)) => {
                    forget();
                    let notice = record(session, &entry, &delivery, held);
                    self.jobs.end(id, if matches!(notice, RunNotice::Recorded { .. }) { JobState::Done } else { failed(&notice) });
                    notices.push(notice);
                    ended.push(Ended::Settled);
                }
                // The provider finished and billed; only writing the charge failed.
                (Outcome::Failed { .. }, Some(mut delivery)) => {
                    if let CompletionAccounting::Failed(failure) = &mut delivery.accounting {
                        let charged = match failure.retry.as_mut() {
                            Some(retry) => retry.retry(budget).map_err(|error| format!("the charge couldn't be recorded: {error:?}")),
                            None => Err(format!("the charge couldn't be recorded: {}", failure.message)),
                        };
                        if let Err(reason) = charged {
                            notices.push(RunNotice::Unaccounted { canvas, node, reason });
                        }
                    }
                    forget();
                    let notice = record(session, &entry, &delivery, held);
                    self.jobs.end(id, if matches!(notice, RunNotice::Recorded { .. }) { JobState::Done } else { failed(&notice) });
                    notices.push(notice);
                    ended.push(Ended::Settled);
                }
                (Outcome::Failed { error, transient: true }, None) => {
                    // Its reservation stays open, and the job is followed again next launch.
                    let reason = format!("couldn't reach the provider ({error}); it will be followed again when NodeCraft next starts");
                    self.jobs.end(id, JobState::Failed { reason: reason.clone(), cause: None });
                    notices.push(RunNotice::Failed { canvas, node, reason });
                    ended.push(Ended::Kept(Box::new(entry)));
                }
                (Outcome::Cancelled, _) => {
                    forget();
                    self.jobs.end(id, JobState::Cancelled);
                    notices.push(RunNotice::Cancelled { canvas, node });
                    ended.push(Ended::Settled);
                }
                (Outcome::Failed { error, .. }, None) => {
                    forget();
                    self.jobs.end(id, JobState::Failed { reason: error.clone(), cause: None });
                    notices.push(RunNotice::Failed { canvas, node, reason: error });
                    ended.push(Ended::Settled);
                }
                (Outcome::Done, None) => {
                    forget();
                    let reason = "the model's result went missing".to_owned();
                    self.jobs.end(id, JobState::Failed { reason: reason.clone(), cause: None });
                    notices.push(RunNotice::Failed { canvas, node, reason });
                    ended.push(Ended::Settled);
                }
            }
        }
        self.jobs.absorb_reports(&mut reports);
        (ended, handled)
    }
}

fn failed(notice: &RunNotice) -> JobState {
    match notice {
        RunNotice::Failed { reason, .. } => JobState::Failed { reason: reason.clone(), cause: None },
        _ => JobState::Done,
    }
}

/// Records a restored job's result on its card, when its step can be assembled from it alone and
/// the card is still there. A result that can't be recorded, its project's included when that
/// isn't open, is held (#369); a held result is retried or placed only once its project is open.
fn record(session: &mut Session, entry: &Submitted, delivery: &CompletionDelivery, held: &Mutex<Held>) -> RunNotice {
    let (canvas, node) = (entry.canvas, entry.node);
    let outcome = match &entry.recipe {
        _ if session.document().id() != entry.project => Err("it finished after a restart, but its project isn't open, so it wasn't recorded".into()),
        Some(recipe) => record_alone(session, canvas, node, &entry.routes, recipe, &delivery.completed),
        None => Err("it finished after a restart, but its step needs other jobs' results too, so it wasn't recorded".into()),
    };
    match outcome {
        Ok(()) => RunNotice::Recorded { canvas, node },
        Err(reason) => {
            let result = HeldResult {
                project: entry.project,
                canvas,
                node,
                provider: entry.target.adapter().to_owned(),
                model: entry.model.clone(),
                reason: reason.clone(),
                completed: delivery.completed.clone(),
                routes: entry.routes.clone(),
                recipe: entry.recipe.clone(),
            };
            held::hold(held, result);
            RunNotice::Failed { canvas, node, reason: format!("{reason}; {HELD_NOTE}") }
        }
    }
}

/// Records `completed` as the whole result of the single-job step `recipe` describes, on
/// `node` on `canvas`: converted by `routes`, its assets imported, assembled and recorded with
/// `run.record`.
pub(super) fn record_alone(
    session: &mut Session,
    canvas: CanvasId,
    node: NodeId,
    routes: &[GenerateOutput],
    recipe: &ResumeRecipe,
    completed: &Completed,
) -> Result<(), String> {
    let produced = generate::produced(routes, completed).map_err(|e| e.message)?;
    for (hash, bytes) in produced.assets {
        match session.import_asset(&bytes) {
            Ok(imported) if imported == hash => {}
            Ok(_) => return Err("a generated asset changed while it was imported".into()),
            Err(error) => return Err(error.message),
        }
    }
    let generation = recipe.assemble_with_execution(produced.outputs, completed.usage.billable_units, completed.usage.execution_ms)?;
    // A variation that isn't the last goes on its own card on a canvas that lays them out so (#808).
    let as_cards = session.document().canvases().get(&canvas).is_some_and(nodecraft_graph::Canvas::variation_cards);
    let entry = match as_cards && !recipe.later.is_empty() {
        true => json!({"node": node, "generation": generation, "card": crate::variation_cmds::card_id(recipe.generation)}),
        false => json!({"node": node, "generation": generation, "later": recipe.later}),
    };
    let params = json!({"canvas": canvas, "results": [entry]});
    session.dispatch("run.record", &params).map(|_| ()).map_err(|error| format!("the result could not be saved: {}", error.message))
}
