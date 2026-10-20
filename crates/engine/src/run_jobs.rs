//! Every job a [`crate::run_host::RunHost`] runs, for the queue panel and card status (#82).
//!
//! A [`JobView`] is made when a job is queued and follows the driver's events and the provider's
//! progress reports. When a job ends it moves to a short list of recent jobs, so the panel can say
//! how it ended. Cancelling is a request: the job's end says what happened.
//!
//! - **Never sent:** cancelled before it started. Nothing was charged.
//! - **Sent:** cancelled after it started, when a request may already be with the provider. It
//!   may still be billed, so the cost stays unresolved. It is never shown as refunded or free.
//! - **Refused:** the provider refused to stop it, or didn't confirm. It may still run and be
//!   billed, so it is followed to its end and its result kept ([`JobView::cancel_refused`]).
//! - **Too late:** it finished, or failed, before the cancel took effect. The result is kept.
use nodecraft_exec::scheduler::JobId;
use nodecraft_providers::{ErrorKind as Cause, JobProgress, RunUpdate};
use nodecraft_types::{CanvasId, Money, NodeId};
use std::collections::BTreeMap;

/// Most recently ended jobs kept for the panel; older ones are dropped first.
pub const MAX_RECENT: usize = 64;
/// Longest progress message or failure reason kept, in characters.
const MAX_TEXT: usize = 240;

/// A job's identity in the host: its run (numbered in start order) and its id in that run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JobKey {
    pub run: u64,
    pub job: JobId,
}

/// Where a job is.
#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    /// Waiting in NodeCraft's queue for a free slot.
    Queued,
    /// Working. `waiting` while the provider has it queued, at `position` when it says;
    /// `fraction` is 0–1 when the provider says; `reconnecting` while a failed poll is retried.
    Running { attempt: u32, waiting: bool, position: Option<u32>, fraction: Option<f32>, message: Option<String>, reconnecting: bool },
    /// Failed for a passing reason and waiting to try again.
    Retrying { next_attempt: u32, reason: String },
    /// Finished; its result is recorded on its card.
    Done,
    /// Ended without a result. `cause` names the provider's reason when there is one.
    Failed { reason: String, cause: Option<Cause> },
    /// Cancelled. See [`JobView::sent`] for whether it may still be billed.
    Cancelled,
}

impl JobState {
    /// Whether the job has ended.
    pub fn is_final(&self) -> bool {
        matches!(self, Self::Done | Self::Failed { .. } | Self::Cancelled)
    }
}

/// One job, as the queue panel shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct JobView {
    pub key: JobKey,
    pub canvas: CanvasId,
    pub node: NodeId,
    /// The provider target and model of a model job; `None` for a local job.
    pub provider: Option<String>,
    pub model: Option<String>,
    /// What the run admitted for it, when it is priced.
    pub estimate: Option<Money>,
    pub state: JobState,
    /// A model job that started: it may have reached the provider, so it may be billed even if
    /// it is cancelled or fails. `false` proves nothing was sent.
    pub sent: bool,
    /// A cancel was asked for and has not yet been answered by the job's end.
    pub cancel_requested: bool,
    /// The provider refused to stop the job, or didn't confirm it did: it may still run and be
    /// billed, so it is followed to its end.
    pub cancel_refused: bool,
    /// The provider itself reported the job (submitted, queued or running), so it was sent.
    pub(crate) reported_sent: bool,
}

impl JobView {
    /// A short, plain status line for the panel.
    pub fn status(&self) -> String {
        match &self.state {
            JobState::Queued if self.cancel_requested => "Cancelling…".into(),
            JobState::Queued => "Queued".into(),
            JobState::Running { .. } if self.cancel_refused => "Cancel refused; still running (may be billed)".into(),
            JobState::Running { .. } if self.cancel_requested => "Cancelling…".into(),
            JobState::Running { reconnecting: true, .. } => "Connection lost, retrying".into(),
            JobState::Running { waiting: true, position: Some(position), .. } => format!("Waiting at the provider (#{position})"),
            JobState::Running { waiting: true, .. } => "Waiting at the provider".into(),
            JobState::Running { fraction: Some(fraction), .. } => format!("Generating {}%", percent(*fraction)),
            JobState::Running { message: Some(message), .. } => message.clone(),
            JobState::Running { .. } => "Generating".into(),
            JobState::Retrying { .. } if self.cancel_requested => "Cancelling…".into(),
            JobState::Retrying { next_attempt, .. } => format!("Retrying (attempt {next_attempt})"),
            JobState::Done if self.cancel_refused => "Finished; the provider refused to cancel".into(),
            JobState::Done if self.cancel_requested => "Finished before it could be cancelled".into(),
            JobState::Done => "Done".into(),
            JobState::Failed { .. } if self.cancel_requested => "Failed before it could be cancelled".into(),
            JobState::Failed { cause: Some(cause), .. } => format!("Failed: {}", cause_label(*cause)),
            JobState::Failed { .. } => "Failed".into(),
            JobState::Cancelled if self.sent => "Cancelled after it was sent (cost unresolved)".into(),
            JobState::Cancelled => "Cancelled before it was sent".into(),
        }
    }
}

fn percent(fraction: f32) -> u32 {
    // `fraction` is clamped to 0–1 when stored, so the cast can't overflow.
    (fraction * 100.0).round().clamp(0.0, 100.0) as u32
}

/// A short name for what went wrong.
pub fn cause_label(cause: Cause) -> &'static str {
    match cause {
        Cause::BlockedContent => "content blocked",
        Cause::UnsupportedInput => "input not supported",
        Cause::Busy => "provider busy",
        Cause::RateLimited => "rate limited",
        Cause::Timeout => "timed out",
        Cause::Budget => "budget or credit",
        Cause::Auth => "API key",
        Cause::Network => "network",
        Cause::Server => "provider error",
        Cause::Malformed => "unexpected reply",
        Cause::NotFound => "job not found",
        Cause::Cancelled => "cancelled",
        Cause::JobFailed => "the job failed",
        Cause::Expired => "took too long",
        _ => "error",
    }
}

/// What to do about a failure, in one sentence.
pub fn fix_hint(cause: Cause) -> &'static str {
    match cause {
        Cause::BlockedContent => "Change the prompt or the input the provider refused, then run it again.",
        Cause::UnsupportedInput => "Check the input's format, size or length against the model, or pick another model.",
        Cause::Busy | Cause::RateLimited => "The provider is overloaded. Wait a moment and retry.",
        Cause::Timeout | Cause::Network => "Check your connection and retry.",
        Cause::Budget => "Raise the budget in Settings, or add credit with the provider.",
        Cause::Auth => "Check the API key for this provider in Settings.",
        Cause::Server | Cause::Malformed => "The provider failed on its side. Retry, or try another model.",
        Cause::NotFound | Cause::Expired => "The provider lost the job. Run it again; it may already have been charged.",
        Cause::JobFailed => "The model ran and failed. Change its inputs or settings before running it again.",
        _ => "Retry, or check the error.",
    }
}

/// The cause named by a job's failure reason: a provider error's own prefix, or a budget refusal.
pub fn cause_of(reason: &str) -> Option<Cause> {
    if crate::jobs::is_budget_refusal(reason) {
        return Some(Cause::Budget);
    }
    Cause::from_error_text(reason)
}

/// The provider's reports for each job since the last pump, written by the update sink off the
/// UI thread. A job keeps one entry, so repeated reports replace each other.
#[derive(Default)]
pub(crate) struct Reports(BTreeMap<JobId, Report>);

#[derive(Default)]
pub(crate) struct Report {
    sent: bool,
    /// The latest progress, which replaces what the job showed.
    latest: Option<Latest>,
    /// A poll failed after the latest progress and is being retried.
    reconnecting: bool,
    cancel_refused: bool,
}

enum Latest {
    Waiting(Option<u32>),
    Running { fraction: Option<f32>, message: Option<String> },
}

impl Reports {
    pub(crate) fn note(&mut self, job: JobId, update: &RunUpdate) {
        let report = self.0.entry(job).or_default();
        match update {
            RunUpdate::Submitted(_) => report.sent = true,
            RunUpdate::Progress(JobProgress::Queued { position }) => {
                report.sent = true;
                report.latest = Some(Latest::Waiting(*position));
                report.reconnecting = false;
            }
            RunUpdate::Progress(JobProgress::Running { fraction, message, .. }) => {
                report.sent = true;
                let fraction = fraction.filter(|f| f.is_finite()).map(|f| f.clamp(0.0, 1.0));
                report.latest = Some(Latest::Running { fraction, message: message.as_deref().map(bounded) });
                report.reconnecting = false;
            }
            RunUpdate::Progress(JobProgress::Done(_) | JobProgress::Failed(_)) => report.sent = true,
            RunUpdate::PollRetry(_) => report.reconnecting = true,
            RunUpdate::CancelRefused(_) => {
                report.sent = true;
                report.cancel_refused = true;
            }
        }
    }
}

/// The jobs of one run, live ones by id.
#[derive(Default)]
pub(crate) struct RunJobs {
    pub(crate) live: BTreeMap<JobId, JobView>,
    /// Jobs that ended since the host last collected them.
    pub(crate) ended: Vec<JobView>,
}

impl RunJobs {
    /// Folds the provider's latest reports into the live jobs.
    pub(crate) fn absorb_reports(&mut self, reports: &Reports) {
        for (job, report) in &reports.0 {
            let Some(view) = self.live.get_mut(job) else { continue };
            view.sent |= report.sent;
            view.reported_sent |= report.sent;
            view.cancel_refused |= report.cancel_refused;
            let JobState::Running { waiting, position, fraction, message, reconnecting, .. } = &mut view.state else { continue };
            match &report.latest {
                Some(Latest::Waiting(at)) => {
                    *waiting = true;
                    *position = *at;
                    *message = None;
                }
                Some(Latest::Running { fraction: reported, message: said }) => {
                    *waiting = false;
                    *position = None;
                    // A provider that sometimes leaves the fraction out keeps the last one.
                    if reported.is_some() {
                        *fraction = *reported;
                    }
                    message.clone_from(said);
                }
                None => {}
            }
            *reconnecting = report.reconnecting || (*reconnecting && report.latest.is_none());
        }
    }

    pub(crate) fn started(&mut self, job: JobId, attempt: u32) {
        if let Some(view) = self.live.get_mut(&job) {
            view.state = JobState::Running { attempt, waiting: false, position: None, fraction: None, message: None, reconnecting: false };
            // From here a request may be in flight, whether or not the provider says so.
            view.sent |= view.provider.is_some();
        }
    }

    pub(crate) fn retrying(&mut self, job: JobId, next_attempt: u32, reason: &str) {
        if let Some(view) = self.live.get_mut(&job) {
            view.state = JobState::Retrying { next_attempt, reason: bounded(reason) };
        }
    }

    /// Ends a job, moving it to `ended`. A job that already ended is left as it was.
    pub(crate) fn end(&mut self, job: JobId, state: JobState) {
        let Some(mut view) = self.live.remove(&job) else { return };
        let first_attempt = match &view.state {
            JobState::Queued => true,
            JobState::Running { attempt, .. } => *attempt <= 1,
            _ => false,
        };
        view.state = match state {
            JobState::Failed { reason, cause } => {
                // The runner refuses before it sends anything; an earlier attempt may have.
                if crate::jobs::is_budget_refusal(&reason) && first_attempt && !view.reported_sent {
                    view.sent = false;
                }
                JobState::Failed { cause: cause.or_else(|| cause_of(&reason)), reason: bounded(&reason) }
            }
            other => other,
        };
        self.ended.push(view);
    }
}

/// `text` cut to [`MAX_TEXT`] characters.
fn bounded(text: &str) -> String {
    match text.char_indices().nth(MAX_TEXT) {
        Some((end, _)) => format!("{}…", text.get(..end).unwrap_or_default()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_providers::ProviderError;

    fn view(state: JobState) -> JobView {
        JobView {
            key: JobKey { run: 1, job: JobId(1) },
            canvas: CanvasId::from_bytes([1; 16]),
            node: NodeId::from_bytes([2; 16]),
            provider: Some("mock".into()),
            model: Some("m".into()),
            estimate: None,
            state,
            sent: false,
            cancel_requested: false,
            cancel_refused: false,
            reported_sent: false,
        }
    }

    #[test]
    fn every_provider_error_names_its_cause() {
        for kind in Cause::ALL {
            assert_eq!(cause_of(&ProviderError::new(kind, "details: with a colon").to_string()), Some(kind));
        }
        assert_eq!(cause_of("budget: over the session cap"), Some(Cause::Budget));
        assert_eq!(cause_of("the model's result went missing"), None);
    }

    #[test]
    fn cancelled_jobs_say_whether_they_may_be_billed() {
        let mut v = view(JobState::Cancelled);
        assert_eq!(v.status(), "Cancelled before it was sent");
        v.sent = true;
        assert!(v.status().contains("cost unresolved"), "{}", v.status());
        let mut done = view(JobState::Done);
        done.cancel_requested = true;
        assert_eq!(done.status(), "Finished before it could be cancelled");
    }

    #[test]
    fn hostile_progress_is_bounded() {
        let mut reports = Reports::default();
        let long = "é".repeat(10_000);
        reports.note(JobId(1), &RunUpdate::Progress(JobProgress::Running { fraction: Some(f32::NAN), message: Some(long), partial: None }));
        reports.note(JobId(2), &RunUpdate::Progress(JobProgress::Running { fraction: Some(7.0), message: None, partial: None }));
        let mut jobs = RunJobs::default();
        for id in [1, 2] {
            let mut v = view(JobState::Running { attempt: 1, waiting: false, position: None, fraction: None, message: None, reconnecting: false });
            v.key.job = JobId(id);
            jobs.live.insert(JobId(id), v);
        }
        jobs.absorb_reports(&reports);
        let JobState::Running { fraction, message, .. } = &jobs.live[&JobId(1)].state else { panic!() };
        assert_eq!(*fraction, None);
        assert_eq!(message.as_ref().unwrap().chars().count(), MAX_TEXT + 1);
        assert_eq!(jobs.live[&JobId(2)].status(), "Generating 100%");
        assert!(jobs.live[&JobId(1)].sent);
    }

    #[test]
    fn reports_after_the_end_change_nothing() {
        let mut jobs = RunJobs::default();
        jobs.live.insert(JobId(7), view(JobState::Queued));
        jobs.end(JobId(7), JobState::Done);
        let mut late = Reports::default();
        late.note(JobId(7), &RunUpdate::Progress(JobProgress::Running { fraction: Some(0.5), message: None, partial: None }));
        jobs.absorb_reports(&late);
        assert!(jobs.live.is_empty());
        jobs.end(JobId(7), JobState::Cancelled);
        assert_eq!(jobs.ended.len(), 1, "a second end is ignored");
        assert_eq!(jobs.ended[0].state, JobState::Done);
    }
}
