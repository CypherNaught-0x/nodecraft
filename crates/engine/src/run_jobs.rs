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
use nodecraft_providers::{ErrorKind as Cause, JobProgress, Output, RunUpdate};
use nodecraft_types::{Blake3, CanvasId, Money, NodeId};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Most recently ended jobs kept for the panel; older ones are dropped first.
pub const MAX_RECENT: usize = 64;
/// Longest progress message or failure reason kept, in characters.
const MAX_TEXT: usize = 240;
/// Largest preview of a sound kept (#875): as much as the largest speech reply read. A bigger
/// one is dropped, and the job shows the one before.
pub const MAX_PREVIEW_BYTES: usize = 32 * 1024 * 1024;

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
    /// The text answer so far, while it streams (#504), at most
    /// [`nodecraft_providers::MAX_STREAMED_TEXT`] bytes. View state: only the finished answer is
    /// recorded. Dropped when the job finishes; a cancelled or failed job keeps it, to be kept
    /// on its card if the person asks ([`crate::run_host::RunHost::keep_partial`]).
    pub partial: Option<Arc<String>>,
    /// The attempt `partial` streamed from; text from an earlier attempt is dropped.
    pub(crate) partial_attempt: u32,
    /// The latest preview of a sound still arriving (#875), while the job runs. View state, like
    /// `partial`: never recorded, saved or undone; dropped when the job ends or tries again.
    pub preview: Option<AudioPreview>,
    /// What a finished job made (#875), so a card knows the sound it shows is this job's take.
    pub made: Option<Made>,
}

/// One attempt of one job (#875): a retry is another take, whose sound starts afresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Take {
    pub job: JobKey,
    /// 1 for the first attempt.
    pub attempt: u32,
}

/// The sound a job has sent so far, as one playable file (#875): ElevenLabs' MP3 while it streams.
#[derive(Clone, PartialEq, Eq)]
pub struct AudioPreview {
    /// The attempt making it: its later previews, and the result it finishes with, continue it.
    pub take: Take,
    /// What the bytes sniff as, never what the provider claimed.
    pub mime: &'static str,
    /// The bytes' BLAKE3 digest, so a player keys it like an asset.
    pub hash: Blake3,
    pub bytes: Arc<[u8]>,
}

/// The result a job finished with (#875): the attempt that made it and the assets in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Made {
    pub attempt: u32,
    /// Its outputs' media, at most [`MAX_MADE`] of them.
    pub assets: Vec<Blake3>,
}

/// Most assets of a finished job's result kept in [`Made`].
pub const MAX_MADE: usize = 64;

/// A preview reported by a job and not yet shown, made off the UI thread by [`Preview::of`].
#[derive(Clone)]
pub(crate) struct Preview {
    mime: &'static str,
    hash: Blake3,
    bytes: Arc<[u8]>,
}

// Up to 32 MiB of sound: say how much, never every byte.
impl std::fmt::Debug for AudioPreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioPreview").field("take", &self.take).field("mime", &self.mime).field("hash", &self.hash).field("bytes", &self.bytes.len()).finish()
    }
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview").field("mime", &self.mime).field("hash", &self.hash).field("bytes", &self.bytes.len()).finish()
    }
}

impl Preview {
    /// The playable sound in `update`'s `partial`, if it has one: bytes that sniff as audio, at
    /// most [`MAX_PREVIEW_BYTES`]. Anything else (text, a link, an image) shows nothing.
    pub(crate) fn of(update: &RunUpdate) -> Option<Self> {
        let RunUpdate::Progress(JobProgress::Running { partial: Some(Output::Bytes { bytes, .. }), .. }) = update else { return None };
        if bytes.is_empty() || bytes.len() > MAX_PREVIEW_BYTES {
            return None;
        }
        let mime = nodecraft_media::sniff(bytes).map(|sniffed| sniffed.mime).filter(|mime| mime.starts_with("audio/"))?;
        Some(Self { mime, hash: Blake3::from_bytes(*blake3::hash(bytes).as_bytes()), bytes: Arc::clone(bytes) })
    }
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
            JobState::Failed { cause: Some(cause), reason } => format!("Failed: {}", failure_label(*cause, reason)),
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

/// What a context-window refusal (#84) is called where other failures show their cause.
pub const CONTEXT_LIMIT_LABEL: &str = "too long for the model";
/// What to do about a context-window refusal, in one sentence.
pub const CONTEXT_LIMIT_HINT: &str = "Shorten the prompt, connect fewer or smaller inputs, or pick a model with a larger context.";

/// [`cause_label`] for a failure whose text is known: a context-window refusal is named as one
/// rather than as just another unsupported input.
pub fn failure_label(cause: Cause, reason: &str) -> &'static str {
    if cause == Cause::UnsupportedInput && nodecraft_providers::is_context_limit_text(reason) { CONTEXT_LIMIT_LABEL } else { cause_label(cause) }
}

/// [`fix_hint`] for a failure whose text is known, with the context-window refusal's own advice.
pub fn failure_hint(cause: Cause, reason: &str) -> &'static str {
    if cause == Cause::UnsupportedInput && nodecraft_providers::is_context_limit_text(reason) { CONTEXT_LIMIT_HINT } else { fix_hint(cause) }
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
    /// Text streamed since the last pump, by attempt `text_attempt`.
    text: String,
    text_attempt: u32,
    /// The latest preview since the last pump, and the attempt it came from.
    preview: Option<(u32, Preview)>,
}

enum Latest {
    Waiting(Option<u32>),
    Running { fraction: Option<f32>, message: Option<String> },
}

impl Reports {
    /// Notes `preview` from `attempt` of `job`, replacing one not yet shown. A late one from an
    /// earlier attempt is dropped.
    pub(crate) fn note_preview(&mut self, job: JobId, attempt: u32, preview: Preview) {
        let report = self.0.entry(job).or_default();
        report.sent = true;
        if report.preview.as_ref().is_none_or(|(had, _)| attempt >= *had) {
            report.preview = Some((attempt, preview));
        }
    }

    /// Notes `update` from `attempt` of `job` (1 for the first).
    pub(crate) fn note(&mut self, job: JobId, attempt: u32, update: &RunUpdate) {
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
            RunUpdate::Text(piece) => {
                report.sent = true;
                // A later attempt answers afresh; a late piece of an earlier one is dropped.
                if attempt > report.text_attempt {
                    report.text.clear();
                    report.text_attempt = attempt;
                }
                if attempt == report.text_attempt {
                    append(&mut report.text, piece);
                }
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
    /// Folds the provider's latest reports into the live jobs. The text they streamed is taken,
    /// so absorbing the same reports again doesn't repeat it.
    pub(crate) fn absorb_reports(&mut self, reports: &mut Reports) {
        for (job, report) in &mut reports.0 {
            let Some(view) = self.live.get_mut(job) else { continue };
            view.sent |= report.sent;
            view.reported_sent |= report.sent;
            view.cancel_refused |= report.cancel_refused;
            if !report.text.is_empty() {
                let text = std::mem::take(&mut report.text);
                // The scheduler's retry events may come in the same pump as the new attempt's
                // text, before or after it: the attempt decides, not the order.
                if report.text_attempt > view.partial_attempt {
                    view.partial = None;
                    view.partial_attempt = report.text_attempt;
                }
                if report.text_attempt == view.partial_attempt {
                    append(Arc::make_mut(view.partial.get_or_insert_default()), &text);
                }
            }
            // Shown only while the attempt that made it runs: a retry starts afresh. Like text,
            // a preview of the next attempt may come before its start event, so it waits.
            let running = match view.state {
                JobState::Running { attempt, .. } => Some(attempt),
                _ => None,
            };
            match report.preview.take() {
                Some((attempt, preview)) if running == Some(attempt) => {
                    let take = Take { job: view.key, attempt };
                    view.preview = Some(AudioPreview { take, mime: preview.mime, hash: preview.hash, bytes: preview.bytes });
                }
                Some((attempt, preview)) if running.is_none_or(|running| attempt > running) => report.preview = Some((attempt, preview)),
                _ => {}
            }
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
            view.preview = None;
            // The next attempt answers afresh, unless its text already arrived this pump.
            if view.partial_attempt < next_attempt {
                view.partial = None;
            }
        }
    }

    /// Notes the media in the result `outputs` that job `job` is about to finish with (#875).
    pub(crate) fn made(&mut self, job: JobId, outputs: &[(String, nodecraft_types::Value)]) {
        let Some(view) = self.live.get_mut(&job) else { return };
        let attempt = match view.state {
            JobState::Running { attempt, .. } => attempt,
            _ => 1,
        };
        let mut assets = Vec::new();
        for (_, value) in outputs {
            media(value, &mut assets, 0);
        }
        view.made = Some(Made { attempt, assets });
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
        // A finished answer is recorded whole; what streamed of it is no longer needed.
        if view.state == JobState::Done {
            view.partial = None;
        }
        // A sound is played from its result once it has one; a preview of one that never
        // finished isn't kept, so the recent jobs hold no audio.
        view.preview = None;
        if view.state != JobState::Done {
            view.made = None;
        }
        self.ended.push(view);
    }
}

/// Adds the media in `value` (and in its lists, `depth` deep so far) to `assets`, up to
/// [`MAX_MADE`].
fn media(value: &nodecraft_types::Value, assets: &mut Vec<Blake3>, depth: usize) {
    match value {
        nodecraft_types::Value::Media(asset) if assets.len() < MAX_MADE => assets.push(asset.hash),
        nodecraft_types::Value::List(items) if depth < 2 => {
            for item in items {
                media(item, assets, depth.saturating_add(1));
            }
        }
        _ => {}
    }
}

/// Appends `piece` to `text`, up to [`nodecraft_providers::MAX_STREAMED_TEXT`] bytes.
fn append(text: &mut String, piece: &str) {
    let room = nodecraft_providers::MAX_STREAMED_TEXT.saturating_sub(text.len());
    let mut end = room.min(piece.len());
    while !piece.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.push_str(piece.get(..end).unwrap_or_default());
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
    use nodecraft_types::{AssetRef, Value};

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
            partial: None,
            partial_attempt: 0,
            preview: None,
            made: None,
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
        reports.note(JobId(1), 1, &RunUpdate::Progress(JobProgress::Running { fraction: Some(f32::NAN), message: Some(long), partial: None }));
        reports.note(JobId(2), 1, &RunUpdate::Progress(JobProgress::Running { fraction: Some(7.0), message: None, partial: None }));
        let mut jobs = RunJobs::default();
        for id in [1, 2] {
            let mut v = view(JobState::Running { attempt: 1, waiting: false, position: None, fraction: None, message: None, reconnecting: false });
            v.key.job = JobId(id);
            jobs.live.insert(JobId(id), v);
        }
        jobs.absorb_reports(&mut reports);
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
        late.note(JobId(7), 1, &RunUpdate::Progress(JobProgress::Running { fraction: Some(0.5), message: None, partial: None }));
        jobs.absorb_reports(&mut late);
        assert!(jobs.live.is_empty());
        jobs.end(JobId(7), JobState::Cancelled);
        assert_eq!(jobs.ended.len(), 1, "a second end is ignored");
        assert_eq!(jobs.ended[0].state, JobState::Done);
    }

    #[test]
    fn streamed_text_is_shown_once_reset_on_retry_and_dropped_when_done() {
        let running = || JobState::Running { attempt: 1, waiting: false, position: None, fraction: None, message: None, reconnecting: false };
        let mut jobs = RunJobs::default();
        jobs.live.insert(JobId(1), view(running()));
        let mut reports = Reports::default();
        reports.note(JobId(1), 1, &RunUpdate::Text("Hel".into()));
        reports.note(JobId(1), 1, &RunUpdate::Text("lo".into()));
        // The host absorbs the same reports before and after the events.
        jobs.absorb_reports(&mut reports);
        jobs.absorb_reports(&mut reports);
        assert_eq!(jobs.live[&JobId(1)].partial.as_deref().map(String::as_str), Some("Hello"));
        assert!(jobs.live[&JobId(1)].sent);
        jobs.retrying(JobId(1), 2, "timeout");
        assert_eq!(jobs.live[&JobId(1)].partial, None, "the next attempt answers afresh");
        jobs.started(JobId(1), 2);
        reports.note(JobId(1), 2, &RunUpdate::Text("x".repeat(nodecraft_providers::MAX_STREAMED_TEXT + 10)));
        jobs.absorb_reports(&mut reports);
        assert_eq!(jobs.live[&JobId(1)].partial.as_ref().map(|t| t.len()), Some(nodecraft_providers::MAX_STREAMED_TEXT), "capped");
        jobs.end(JobId(1), JobState::Cancelled);
        assert!(jobs.ended[0].partial.is_some(), "a cancelled job keeps what streamed");
        jobs.live.insert(JobId(2), view(running()));
        reports.note(JobId(2), 1, &RunUpdate::Text("done".into()));
        jobs.absorb_reports(&mut reports);
        jobs.end(JobId(2), JobState::Done);
        assert_eq!(jobs.ended[1].partial, None, "a finished one shows its recorded answer");
    }

    #[test]
    fn a_retry_in_the_same_pump_as_the_next_attempts_text_keeps_that_text() {
        let running = |attempt| JobState::Running { attempt, waiting: false, position: None, fraction: None, message: None, reconnecting: false };
        let mut jobs = RunJobs::default();
        jobs.live.insert(JobId(1), view(running(1)));
        let mut reports = Reports::default();
        // One snapshot: attempt 1's text, then (in the events) Retrying and Started(2), and
        // attempt 2's first piece, and a late piece of attempt 1.
        reports.note(JobId(1), 1, &RunUpdate::Text("old".into()));
        reports.note(JobId(1), 2, &RunUpdate::Text("Hel".into()));
        reports.note(JobId(1), 1, &RunUpdate::Text("late".into()));
        jobs.absorb_reports(&mut reports);
        jobs.retrying(JobId(1), 2, "timeout");
        jobs.started(JobId(1), 2);
        jobs.absorb_reports(&mut reports);
        reports.note(JobId(1), 2, &RunUpdate::Text("lo".into()));
        jobs.absorb_reports(&mut reports);
        assert_eq!(jobs.live[&JobId(1)].partial.as_deref().map(String::as_str), Some("Hello"));
        // A late piece of the earlier attempt in a later pump is dropped too.
        reports.note(JobId(1), 1, &RunUpdate::Text("late".into()));
        jobs.absorb_reports(&mut reports);
        assert_eq!(jobs.live[&JobId(1)].partial.as_deref().map(String::as_str), Some("Hello"));
    }

    fn mp3(len: usize) -> Arc<[u8]> {
        // An MPEG audio frame header, then filler: enough to sniff as MP3.
        let mut bytes = vec![0xFF, 0xFB, 0x90, 0x64];
        bytes.resize(len.max(4), 0);
        bytes.into()
    }

    fn preview_update(bytes: Arc<[u8]>, mime: &str) -> RunUpdate {
        RunUpdate::Progress(JobProgress::Running { fraction: None, message: None, partial: Some(Output::Bytes { mime: mime.into(), bytes }) })
    }

    #[test]
    fn only_bounded_audio_makes_a_preview_and_its_mime_is_sniffed() {
        let sound = mp3(4096);
        let preview = Preview::of(&preview_update(Arc::clone(&sound), "image/png")).unwrap();
        assert_eq!(preview.mime, "audio/mpeg", "the bytes decide, not the claim");
        assert_eq!(preview.hash, Blake3::from_bytes(*blake3::hash(&sound).as_bytes()));
        assert!(Preview::of(&preview_update(b"\x89PNG\r\n\x1a\nnot a sound at all".as_slice().into(), "audio/mpeg")).is_none());
        assert!(Preview::of(&preview_update(Arc::from(Vec::new()), "audio/mpeg")).is_none());
        assert!(Preview::of(&preview_update(mp3(MAX_PREVIEW_BYTES + 1), "audio/mpeg")).is_none(), "capped");
        assert!(Preview::of(&RunUpdate::Text("hello".into())).is_none());
        let text = RunUpdate::Progress(JobProgress::Running { fraction: None, message: None, partial: Some(Output::Text("hi".into())) });
        assert!(Preview::of(&text).is_none());
    }

    #[test]
    fn only_the_latest_preview_of_the_running_attempt_is_shown_and_none_after_the_end() {
        let running = |attempt| JobState::Running { attempt, waiting: false, position: None, fraction: None, message: None, reconnecting: false };
        let mut jobs = RunJobs::default();
        jobs.live.insert(JobId(1), view(running(1)));
        let mut reports = Reports::default();
        let note = |reports: &mut Reports, attempt, len| reports.note_preview(JobId(1), attempt, Preview::of(&preview_update(mp3(len), "audio/mpeg")).unwrap());
        note(&mut reports, 1, 100);
        note(&mut reports, 1, 200);
        jobs.absorb_reports(&mut reports);
        let shown = jobs.live[&JobId(1)].preview.clone().unwrap();
        let key = JobKey { run: 1, job: JobId(1) };
        assert_eq!((shown.bytes.len(), shown.take, shown.mime), (200, Take { job: key, attempt: 1 }, "audio/mpeg"));
        let debug = format!("{shown:?}");
        assert!(debug.contains("bytes: 200") && debug.len() < 400, "says how much, never every byte: {debug}");
        assert!(jobs.live[&JobId(1)].sent);
        // A retry drops it; the next attempt's preview may come in the same pump, before its start.
        note(&mut reports, 2, 300);
        jobs.retrying(JobId(1), 2, "timeout");
        jobs.absorb_reports(&mut reports);
        assert_eq!(jobs.live[&JobId(1)].preview, None, "the next attempt starts afresh");
        jobs.started(JobId(1), 2);
        note(&mut reports, 1, 400);
        jobs.absorb_reports(&mut reports);
        let shown = jobs.live[&JobId(1)].preview.clone().unwrap();
        assert_eq!(shown.bytes.len(), 300, "a late one of attempt 1 is dropped");
        assert_eq!(shown.take, Take { job: key, attempt: 2 }, "the retry is another take: its sound starts afresh");
        note(&mut reports, 2, 500);
        let result = AssetRef { hash: Blake3::from_bytes([9; 32]), mime: "audio/mpeg".parse().unwrap(), meta: Default::default() };
        let text = ("text".to_owned(), Value::Text("words".into()));
        jobs.made(JobId(1), &[text, ("audio".into(), Value::List(vec![Value::Media(result.clone())]))]);
        jobs.end(JobId(1), JobState::Done);
        jobs.absorb_reports(&mut reports);
        assert_eq!(jobs.ended[0].preview, None, "an ended job holds no sound");
        assert_eq!(jobs.ended[0].made, Some(Made { attempt: 2, assets: vec![result.hash] }), "it made its result, in its second take");
        jobs.live.insert(JobId(2), view(running(1)));
        reports.note_preview(JobId(2), 1, Preview::of(&preview_update(mp3(10), "audio/mpeg")).unwrap());
        jobs.absorb_reports(&mut reports);
        jobs.made(JobId(2), &[("audio".into(), Value::Media(result))]);
        jobs.end(JobId(2), JobState::Cancelled);
        assert_eq!(jobs.ended[1].preview, None, "nor a cancelled one");
        assert_eq!(jobs.ended[1].made, None, "which made nothing");
    }
}
