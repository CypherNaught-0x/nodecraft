//! The run host's jobs as commands (#167): what the queue panel shows, for agents too.
//!
//! - `run.jobs` (read-only): the host's jobs as of its last frame, newest last, at most
//!   [`MAX_LISTED`]: each one's state, queue position and progress where the provider says, and
//!   whether it may have been charged.
//! - `run.cancel {run, job}`: asks the host to stop a job that hasn't ended, as the queue panel's
//!   cancel does. A job not yet sent is never sent. A job already sent is asked to stop at its
//!   provider, which is no refund: it may still be billed, and a provider that refuses or can't
//!   cancel is followed to its end and says so in `run.jobs`. The host carries it out on its next
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
    Ok(Effect::Service { capability: JOBS_SERVICE, request: json!({"op": "cancel", "run": p.run, "job": p.job}) })
}

/// The jobs as the host last published them, and the cancels asked since.
#[derive(Default)]
pub(crate) struct Board {
    jobs: Vec<JobView>,
    pub(crate) cancels: Vec<JobKey>,
}

impl Board {
    /// Replaces the listed jobs with `jobs`, keeping the newest [`MAX_LISTED`].
    pub(crate) fn publish(&mut self, mut jobs: Vec<JobView>) {
        let extra = jobs.len().saturating_sub(MAX_LISTED);
        jobs.drain(..extra);
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
        "may_have_been_charged": v.may_have_been_charged(),
    })
}

fn list_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

fn cancel_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["run", "job"],
        "properties": {"run": {"type": "integer", "minimum": 0}, "job": {"type": "integer", "minimum": 0}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "run.jobs",
            label: "List run jobs",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {jobs: [{run, job, canvas, node, provider, model, estimate, state: queued|running|retrying|done|failed|cancelled, waiting_at_provider, position, fraction, reason, status, sent, cancel_requested, cancel_refused, may_have_been_charged}], max}. The host's jobs as of its last frame, newest last, at most `max`; `position` and `fraction` only where the provider says. `sent` false proves nothing reached the provider. Read-only.",
            enabled: enabled_list,
            run: run_list,
        },
        CommandSpec {
            id: "run.cancel",
            label: "Cancel a run job",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: cancel_schema,
            params_doc: "{run, job} (from run.jobs) -> {run, job, cancelling: true, may_be_billed}. Asks the host to stop a job that hasn't ended, on its next frame. A job not yet sent is never sent. A sent one is asked to stop at its provider: that is no refund, it may still be billed, and a provider that refuses or can't cancel is followed to its end (`cancel_refused` in run.jobs). Ended jobs are refused. Never spends money or opens a dialog.",
            enabled: enabled_cancel,
            run: run_cancel,
        },
    ]
}
