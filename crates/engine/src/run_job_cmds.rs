//! The run host's jobs as commands (#167): what the queue panel shows, for agents too.
//!
//! - `run.jobs` (read-only): the host's jobs as of its last frame, newest last, at most
//!   [`MAX_LISTED`]: each one's state, queue position and progress where the provider says, and
//!   whether it may have been charged.
//! - `run.cancel {run, job, accept_loss?}`: asks the host to stop a job that hasn't ended, as the
//!   queue panel's cancel does. A job not yet sent is never sent. A job already sent is asked to
//!   stop at its provider only with `accept_loss: true`: that is no refund, it may still be
//!   billed and its result lost, and a provider that refuses or can't cancel is followed to its
//!   end and says so in `run.jobs`. The host carries it out on its next
//!   frame; nothing opens a dialog.
//!
//! Neither spends money. Host contract: provide [`JOBS_SERVICE`] (the desktop's
//! [`crate::run_host::RunHost::register`] does).

use crate::commands::{CommandSpec, Context, Effect, params};
use crate::run_jobs::{JobKey, JobState, JobView};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, PoisonError};

/// The host service that lists and cancels jobs.
pub const JOBS_SERVICE: &str = "run.jobs_host";
/// Most jobs `run.jobs` lists, newest kept.
pub const MAX_LISTED: usize = 256;
/// Most cancels waiting for the host's next frame.
const MAX_PENDING_CANCELS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelParams {
    run: u64,
    job: u64,
    /// Required to cancel a job already sent: its result may be billed and lost.
    #[serde(default)]
    accept_loss: bool,
}

fn host(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(JOBS_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "this host runs no jobs")) }
}

fn enabled_list(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<ListParams>(value)?;
    host(c)
}

fn run_list(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_list(c, value)?;
    Ok(Effect::Service { capability: JOBS_SERVICE, request: json!({"op": "list"}) })
}

fn enabled_cancel(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<CancelParams>(value)?;
    host(c)
}

fn run_cancel(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p = params::<CancelParams>(value)?;
    host(c)?;
    Ok(Effect::Service { capability: JOBS_SERVICE, request: json!({"op": "cancel", "run": p.run, "job": p.job, "accept_loss": p.accept_loss}) })
}

/// The jobs as the host last published them, and the cancels asked since.
#[derive(Default)]
pub(crate) struct Board {
    jobs: Vec<JobView>,
    pub(crate) cancels: Vec<JobKey>,
}

impl Board {
    /// Replaces the listed jobs with `jobs`, at most [`MAX_LISTED`]. Past that, the oldest
    /// ended jobs go first, so every live job stays listed and cancellable.
    pub(crate) fn publish(&mut self, mut jobs: Vec<JobView>) {
        let mut extra = jobs.len().saturating_sub(MAX_LISTED);
        if extra > 0 {
            jobs.retain(|v| {
                let drop = extra > 0 && v.state.is_final();
                extra = extra.saturating_sub(usize::from(drop));
                !drop
            });
            // More live jobs than fit: the newest are kept.
            let more = jobs.len().saturating_sub(MAX_LISTED);
            jobs.drain(..more);
        }
        self.jobs = jobs;
    }

    fn serve(&mut self, request: &Value) -> Result<Value, Error> {
        match request.get("op").and_then(Value::as_str) {
            Some("list") => Ok(json!({"jobs": self.jobs.iter().map(view).collect::<Vec<_>>(), "max": MAX_LISTED})),
            Some("cancel") => {
                let number =
                    |name: &str| request.get(name).and_then(Value::as_u64).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "run and job are numbers"));
                let key = JobKey { run: number("run")?, job: nodecraft_exec::scheduler::JobId(number("job")?) };
                let job = self.jobs.iter().find(|v| v.key == key).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such job; list them with run.jobs"))?;
                if job.state.is_final() {
                    return Err(Error::new(ErrorKind::Conflict, "this job has already ended"));
                }
                let sent = job.sent;
                if sent && request.get("accept_loss").and_then(Value::as_bool) != Some(true) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "this job was already sent: cancelling it may lose a result that is billed anyway, and getting it again means paying again. Pass accept_loss: true to cancel it",
                    ));
                }
                if !self.cancels.contains(&key) {
                    if self.cancels.len() >= MAX_PENDING_CANCELS {
                        return Err(Error::new(ErrorKind::Busy, "too many cancels are waiting; try again in a moment"));
                    }
                    self.cancels.push(key);
                }
                // A sent job may already be billed: a cancel only asks the provider to stop.
                Ok(json!({"run": key.run, "job": key.job.0, "cancelling": true, "may_be_billed": sent}))
            }
            _ => Err(Error::new(ErrorKind::InvalidInput, "unknown jobs request")),
        }
    }
}

pub(crate) fn register(board: &Arc<Mutex<Board>>, services: &mut crate::commands::Services) -> Result<(), Error> {
    let board = Arc::clone(board);
    services.register(JOBS_SERVICE, move |request| board.lock().unwrap_or_else(PoisonError::into_inner).serve(request))
}

fn view(v: &JobView) -> Value {
    let (state, waiting, position, fraction, reason) = match &v.state {
        JobState::Queued => ("queued", false, None, None, None),
        JobState::Running { waiting, position, fraction, .. } => ("running", *waiting, *position, fraction.filter(|f| f.is_finite()), None),
        JobState::Retrying { reason, .. } => ("retrying", false, None, None, Some(reason.clone())),
        JobState::Done => ("done", false, None, None, None),
        JobState::Failed { reason, .. } => ("failed", false, None, None, Some(reason.clone())),
        JobState::Cancelled => ("cancelled", false, None, None, None),
    };
    json!({
        "run": v.key.run,
        "job": v.key.job.0,
        "canvas": v.canvas,
        "node": v.node,
        "provider": v.provider,
        "model": v.model,
        "estimate": v.estimate,
        "state": state,
        "waiting_at_provider": waiting,
        "position": position,
        "fraction": fraction,
        "reason": reason,
        "status": v.status(),
        "sent": v.sent,
        "cancel_requested": v.cancel_requested,
        "cancel_refused": v.cancel_refused,
        "may_be_billed": may_be_billed(v),
        "may_have_been_charged": v.may_have_been_charged(),
    })
}

/// Whether the job may be billed: it reached the provider and wasn't proven refused.
fn may_be_billed(v: &JobView) -> bool {
    match v.state {
        JobState::Failed { .. } => v.may_have_been_charged(),
        _ => v.sent,
    }
}

fn list_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

fn cancel_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["run", "job"],
        "properties": {"run": {"type": "integer", "minimum": 0}, "job": {"type": "integer", "minimum": 0}, "accept_loss": {"type": "boolean"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "run.jobs",
            label: "List run jobs",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {jobs: [{run, job, canvas, node, provider, model, estimate, state: queued|running|retrying|done|failed|cancelled, waiting_at_provider, position, fraction, reason, status, sent, cancel_requested, cancel_refused, may_be_billed, may_have_been_charged}], max}. The host's jobs as of its last frame, newest last, at most `max` (ended jobs are dropped first, so live ones stay listed); `position` and `fraction` only where the provider says. `sent` false proves nothing reached the provider. `may_be_billed`: it reached the provider and wasn't proven refused, finished or not; count these when tallying spend. `may_have_been_charged`: it ended without a kept result (failed or cancelled) and may still be billed. Read-only.",
            enabled: enabled_list,
            run: run_list,
        },
        CommandSpec {
            id: "run.cancel",
            label: "Cancel a run job",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: cancel_schema,
            params_doc: "{run, job (from run.jobs), accept_loss?: bool} -> {run, job, cancelling: true, may_be_billed}. Asks the host to stop a job that hasn't ended, on its next frame. A job not yet sent is never sent and needs nothing more. A sent one is refused unless `accept_loss` is true: it is asked to stop at its provider, which is no refund, it may still be billed and its result lost, and getting it again means paying again; a provider that refuses or can't cancel is followed to its end (`cancel_refused` in run.jobs). Ended jobs are refused. Never spends money or opens a dialog.",
            enabled: enabled_cancel,
            run: run_cancel,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use nodecraft_exec::scheduler::JobId;
    use nodecraft_types::{CanvasId, NodeId, ProjectId};

    fn job(n: u64, state: JobState, sent: bool) -> JobView {
        JobView {
            key: JobKey { run: 1, job: JobId(n) },
            canvas: CanvasId::from_bytes([1; 16]),
            node: NodeId::from_bytes([2; 16]),
            provider: Some("mock".into()),
            model: Some("mock-style".into()),
            estimate: None,
            state,
            sent,
            cancel_requested: false,
            cancel_refused: false,
            reported_sent: sent,
            partial: None,
            partial_attempt: 0,
            unrefused_attempt: false,
            preview: None,
            made: None,
        }
    }

    fn running() -> JobState {
        JobState::Running { attempt: 1, waiting: false, position: None, fraction: Some(0.5), message: None, reconnecting: false }
    }

    fn cancel(board: &mut Board, n: u64, accept_loss: bool) -> Result<Value, Error> {
        board.serve(&json!({"op": "cancel", "run": 1, "job": n, "accept_loss": accept_loss}))
    }

    #[test]
    fn a_host_without_jobs_refuses_both_commands() {
        let project = nodecraft_graph::Project::new(ProjectId::from_bytes([3; 16]), "P").unwrap();
        let mut s = Session::new(project).unwrap();
        for (id, params) in [("run.jobs", json!({})), ("run.cancel", json!({"run": 1, "job": 1}))] {
            let error = s.dispatch(id, &params).unwrap_err();
            assert_eq!((error.kind, error.message.as_str()), (ErrorKind::Unsupported, "this host runs no jobs"), "{id}");
        }
        for bad in
            [json!({"run": -1, "job": 1}), json!({"run": 1}), json!({"run": 1, "job": 1, "extra": true}), json!({"run": 1, "job": 1, "accept_loss": "yes"})]
        {
            assert_eq!(s.dispatch("run.cancel", &bad).unwrap_err().kind, ErrorKind::InvalidInput, "{bad}");
        }
        assert_eq!(s.dispatch("run.jobs", &json!({"x": 1})).unwrap_err().kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn a_sent_job_is_cancelled_only_when_its_loss_is_accepted() {
        let mut board = Board::default();
        board.publish(vec![job(1, JobState::Queued, false), job(2, running(), true), job(3, JobState::Done, true)]);
        // Not yet sent: nothing can be lost.
        assert_eq!(cancel(&mut board, 1, false).unwrap()["may_be_billed"], false);
        let refused = cancel(&mut board, 2, false).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::InvalidInput);
        assert!(refused.message.contains("billed") && refused.message.contains("paying again") && refused.message.contains("accept_loss"), "{refused:?}");
        assert_eq!(board.cancels, [JobKey { run: 1, job: JobId(1) }], "the refused cancel isn't queued");
        assert_eq!(cancel(&mut board, 2, true).unwrap()["may_be_billed"], true);
        assert_eq!(cancel(&mut board, 2, true).unwrap()["cancelling"], true, "asking twice queues it once");
        assert_eq!(board.cancels.len(), 2);
        assert_eq!(cancel(&mut board, 3, true).unwrap_err().kind, ErrorKind::Conflict, "an ended job");
        assert_eq!(cancel(&mut board, 9, true).unwrap_err().kind, ErrorKind::NotFound);
        assert_eq!(board.serve(&json!({"op": "cancel", "run": "1", "job": 1})).unwrap_err().kind, ErrorKind::InvalidInput);
        assert_eq!(board.serve(&json!({"op": "forget"})).unwrap_err().kind, ErrorKind::InvalidInput, "an unknown request");
        assert_eq!(board.serve(&json!({})).unwrap_err().kind, ErrorKind::InvalidInput);
    }

    #[test]
    fn cancels_waiting_for_the_host_are_bounded() {
        let mut board = Board::default();
        board.publish((0..=MAX_PENDING_CANCELS as u64).map(|n| job(n, JobState::Queued, false)).collect());
        for n in 0..MAX_PENDING_CANCELS as u64 {
            cancel(&mut board, n, false).unwrap();
        }
        assert_eq!(cancel(&mut board, MAX_PENDING_CANCELS as u64, false).unwrap_err().kind, ErrorKind::Busy);
        assert!(cancel(&mut board, 0, false).is_ok(), "one already waiting is not counted again");
    }

    #[test]
    fn the_list_is_capped_and_keeps_live_jobs_before_ended_ones() {
        let mut board = Board::default();
        // Ended jobs first, then live ones, more than fit.
        let ended = (0..300).map(|n| job(n, JobState::Done, true));
        let live = (300..310).map(|n| job(n, running(), true));
        board.publish(ended.chain(live).collect());
        let listed = board.serve(&json!({"op": "list"})).unwrap();
        let jobs = listed["jobs"].as_array().unwrap();
        assert_eq!((jobs.len(), listed["max"].clone()), (MAX_LISTED, json!(MAX_LISTED)));
        assert!((300..310).all(|n| jobs.iter().any(|j| j["job"] == json!(n))), "every live job stays listed");
        assert_eq!(jobs.first().unwrap()["job"], json!(54), "the oldest ended jobs went first");
        assert!(cancel(&mut board, 309, true).is_ok());
        // More live jobs than fit: the newest are kept.
        board.publish((0..300).map(|n| job(n, running(), true)).collect());
        let listed = board.serve(&json!({"op": "list"})).unwrap();
        assert_eq!(listed["jobs"][0]["job"], json!(44));
    }

    #[test]
    fn a_running_or_finished_paid_job_may_be_billed() {
        let mut board = Board::default();
        let refused = JobState::Failed { reason: "refused".into(), cause: None };
        board.publish(vec![job(1, running(), true), job(2, JobState::Done, true), job(3, JobState::Queued, false), job(4, refused, false)]);
        let listed = board.serve(&json!({"op": "list"})).unwrap();
        let flags: Vec<_> = listed["jobs"].as_array().unwrap().iter().map(|j| (j["may_be_billed"].clone(), j["may_have_been_charged"].clone())).collect();
        assert_eq!(flags, [(json!(true), json!(false)), (json!(true), json!(false)), (json!(false), json!(false)), (json!(false), json!(false))]);
    }
}
