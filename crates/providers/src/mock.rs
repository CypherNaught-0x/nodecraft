//! A deterministic fake provider. Each model is scripted with a [`MockModel`]; results depend only
//! on the model, its parameters and its inputs, so tests can compare them byte for byte.

use crate::{BoxFuture, Completed, ErrorKind, JobHandle, JobInput, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::Money;
mod scripts;
pub use scripts::{MockFailures, MockOperation, MockScript};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

/// What a mock model returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MockOutput {
    Text,
    Image,
    Video,
    Audio,
}

/// Where a mock model fails, if it does. A model that finishes at submit (`polls: 0`) has no
/// polls, so its `Poll` and `Job` failures happen at submit instead (unbilled by the mock, though
/// hosts may treat some of those kinds as possibly billed).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MockFailure {
    /// The submit is refused with this kind; nothing is billed.
    Submit(ErrorKind),
    /// The provider accepts (and bills) the job, but the reply is lost: the submit fails with
    /// this kind. A retry under the same key gets the job back without a second charge.
    LostReply(ErrorKind),
    /// Every poll fails with this kind (a transient kind exercises poll retries).
    Poll(ErrorKind),
    /// The provider reports the job itself as failed.
    Job(ErrorKind),
    /// Asked to cancel, the provider answers with this kind and keeps running the job.
    Cancel(ErrorKind),
}

/// How a mock model behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockModel {
    pub output: MockOutput,
    /// Polls before the job is done; 0 finishes at submit (a synchronous API).
    pub polls: u32,
    /// The cost it reports, if any.
    pub cost: Option<Money>,
    pub fail: Option<MockFailure>,
}

impl MockModel {
    pub fn new(output: MockOutput) -> Self {
        Self { output, polls: 0, cost: None, fail: None }
    }
}

#[derive(Default)]
struct State {
    /// Every request, in order (retries included).
    requests: Vec<ProviderJob>,
    /// Jobs accepted, by idempotency key: their handle id. A retry under the same key gets the
    /// same job back and isn't billed again.
    accepted: BTreeMap<String, String>,
    /// Pending jobs by handle id: the request and the polls left.
    pending: BTreeMap<String, (ProviderJob, u32)>,
    /// Results of jobs finished at submit, by key: a retry gets the original result.
    finished: BTreeMap<String, Completed>,
    cancelled: Vec<String>,
    /// Cancels refused ([`MockFailure::Cancel`]).
    cancel_refused: u32,
    polls: u64,
    scripted: BTreeMap<String, scripts::Progress>,
}

/// See the module docs.
#[derive(Clone)]
pub struct MockProvider {
    id: String,
    models: Arc<BTreeMap<String, MockModel>>,
    scripts: Arc<scripts::Scripts>,
    state: Arc<Mutex<State>>,
}

impl MockProvider {
    /// A mock provider called `id` with `models` (model id → behaviour).
    pub fn new(id: impl Into<String>, models: impl IntoIterator<Item = (String, MockModel)>) -> Self {
        Self { id: id.into(), models: Arc::new(models.into_iter().collect()), scripts: Arc::default(), state: Arc::default() }
    }

    /// The default mock provider (`mock`): `mock-text` and `mock-image`, `mock-video`,
    /// `mock-audio`, all finishing at submit with no cost.
    pub fn standard() -> Self {
        Self::new(
            "mock",
            [("mock-text", MockOutput::Text), ("mock-image", MockOutput::Image), ("mock-video", MockOutput::Video), ("mock-audio", MockOutput::Audio)]
                .map(|(id, output)| (id.to_owned(), MockModel::new(output))),
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every request received, retries included.
    pub fn requests(&self) -> Vec<ProviderJob> {
        self.lock().requests.clone()
    }

    /// Jobs accepted (and so billed): one per idempotency key.
    pub fn billed(&self) -> usize {
        self.lock().accepted.len()
    }

    /// Handles the host asked to cancel; their jobs report `Cancelled` from then on.
    pub fn cancelled(&self) -> Vec<String> {
        self.lock().cancelled.clone()
    }

    /// Cancels it refused ([`MockFailure::Cancel`]).
    pub fn cancels_refused(&self) -> u32 {
        self.lock().cancel_refused
    }

    /// Polls received.
    pub fn polls(&self) -> u64 {
        self.lock().polls
    }
}

/// A deterministic digest of a request: model, parameters and inputs.
fn digest(job: &ProviderJob) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    let mut field = |bytes: &[u8]| {
        hasher.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
        hasher.update(bytes);
    };
    field(job.model.as_bytes());
    // `Map` is ordered, so its JSON is canonical.
    field(serde_json::Value::Object(job.params.clone()).to_string().as_bytes());
    for input in &job.inputs {
        match input {
            JobInput::Text { role, text } => {
                field(role.as_bytes());
                field(text.as_bytes());
            }
            JobInput::Media { role, mime, bytes } => {
                field(role.as_bytes());
                field(mime.as_bytes());
                field(blake3::hash(bytes).as_bytes());
            }
        }
    }
    hasher.finalize()
}

/// What a model returns for `job`.
fn result(model: &MockModel, job: &ProviderJob) -> Completed {
    let digest = digest(job);
    let short: String = digest.to_hex().chars().take(8).collect();
    let output = match model.output {
        MockOutput::Text => {
            let prompt: Vec<&str> = job.inputs.iter().filter_map(|i| if let JobInput::Text { text, .. } = i { Some(text.as_str()) } else { None }).collect();
            Output::Text(format!("{}: {} #{short}", job.model, prompt.join(" / ")))
        }
        media => {
            // A real format signature (PNG, WAV, MP4) followed by the digest, so hosts sniff and
            // route it like real output.
            let (signature, mime): (&[u8], &str) = match media {
                MockOutput::Image => (b"\x89PNG\r\n\x1a\n", "image/png"),
                MockOutput::Video => (b"\x00\x00\x00\x10ftypisom\x00\x00\x02\x00", "video/mp4"),
                _ => (b"RIFF\x00\x00\x00\x00WAVEfmt ", "audio/wav"),
            };
            let mut bytes = signature.to_vec();
            bytes.extend_from_slice(b"NODECRAFT-MOCK ");
            bytes.extend_from_slice(digest.as_bytes());
            Output::Bytes { mime: mime.to_owned(), bytes: Arc::from(bytes) }
        }
    };
    let mut done = Completed::new(vec![output]);
    done.usage.cost = model.cost;
    done
}

impl Provider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn submit<'a>(&'a self, job: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        let outcome = (|| {
            let mut s = self.lock();
            s.requests.push(job.clone());
            // Acceptance owns the key, including lost replies and cancelled/completed jobs.
            // Retry payloads must not select another model or execute its failure script.
            if let Some(id) = s.accepted.get(&job.idempotency_key).cloned() {
                if let Some(progress) = s.scripted.get_mut(&job.idempotency_key)
                    && !matches!(progress.terminal, Some(scripts::Terminal::Failed | scripts::Terminal::Cancelled))
                    && progress.lost > 0
                {
                    progress.lost -= 1;
                    let kind = self.scripts.keys.get(&job.idempotency_key).and_then(|key| key.script.lost_replies).ok_or_else(scripts::invalid)?.kind;
                    return Err(ProviderError::new(kind, "mock reply lost"));
                }
                if let Some(done) = s.finished.get(&job.idempotency_key) {
                    return Ok(Submission::Finished(done.clone()));
                }
                return Ok(Submission::Queued(JobHandle { remote_id: id.clone(), data: serde_json::Value::Null }));
            }
            if let Some(key) = self.scripts.keys.get(&job.idempotency_key) {
                if key.model != job.model {
                    return Err(scripts::invalid());
                }
                // Every predecessor must remain failed: cancellation of an older generation
                // closes this operation too. Constructor-built chains are finite and acyclic.
                let mut prior = key.prior.as_ref();
                while let Some(previous) = prior {
                    if s.scripted.get(previous).is_none_or(|p| p.terminal != Some(scripts::Terminal::Failed)) {
                        return Err(scripts::invalid());
                    }
                    prior = self.scripts.keys.get(previous).and_then(|config| config.prior.as_ref());
                }
                let progress = s.scripted.get_mut(&job.idempotency_key).ok_or_else(scripts::invalid)?;
                if progress.submit > 0 {
                    progress.submit -= 1;
                    let kind = key.script.submit_failures.ok_or_else(scripts::invalid)?.kind;
                    return Err(ProviderError::new(kind, "mock submit failure"));
                }
            } else if self.scripts.models.contains(&job.model) {
                return Err(scripts::invalid());
            }
            let model = *self.models.get(&job.model).ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "unknown model"))?;
            match model.fail {
                Some(MockFailure::Submit(kind)) => return Err(ProviderError::new(kind, "mock submit failure")),
                Some(MockFailure::Poll(kind) | MockFailure::Job(kind)) if model.polls == 0 => return Err(ProviderError::new(kind, "mock failure")),
                _ => {}
            }
            let next = s.accepted.len();
            let id = format!("mock-{next}");
            s.accepted.insert(job.idempotency_key.clone(), id.clone());
            let scripted_lost = if let Some(progress) = s.scripted.get_mut(&job.idempotency_key) {
                if progress.lost > 0 {
                    progress.lost -= 1;
                    self.scripts.keys.get(&job.idempotency_key).and_then(|key| key.script.lost_replies).map(|f| f.kind)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(kind) = scripted_lost.or(if let Some(MockFailure::LostReply(kind)) = model.fail { Some(kind) } else { None }) {
                // Accepted and billed, but the caller never hears so.
                if model.polls > 0 {
                    s.pending.insert(id, (job.clone(), model.polls));
                } else {
                    s.finished.insert(job.idempotency_key.clone(), result(&model, job));
                }
                return Err(ProviderError::new(kind, "mock reply lost"));
            }
            if model.polls == 0 {
                let done = s.finished.entry(job.idempotency_key.clone()).or_insert_with(|| result(&model, job)).clone();
                return Ok(Submission::Finished(done));
            }
            s.pending.entry(id.clone()).or_insert_with(|| (job.clone(), model.polls));
            Ok(Submission::Queued(JobHandle { remote_id: id, data: serde_json::Value::Null }))
        })();
        Box::pin(async move { outcome })
    }

    fn poll<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        let outcome = (|| {
            let mut s = self.lock();
            s.polls = s.polls.saturating_add(1);
            if s.cancelled.contains(&handle.remote_id) {
                return Ok(JobProgress::Failed(ProviderError::new(ErrorKind::Cancelled, "cancelled")));
            }
            let key = s.pending.get(&handle.remote_id).ok_or_else(|| ProviderError::new(ErrorKind::NotFound, "unknown job"))?.0.idempotency_key.clone();
            if let Some(progress) = s.scripted.get_mut(&key) {
                let config = self.scripts.keys.get(&key).ok_or_else(scripts::invalid)?;
                if progress.terminal == Some(scripts::Terminal::Failed) {
                    return Ok(JobProgress::Failed(ProviderError::new(config.failure.ok_or_else(scripts::invalid)?, "mock job failure")));
                }
                if progress.poll > 0 {
                    progress.poll -= 1;
                    return Err(ProviderError::new(config.script.poll_failures.ok_or_else(scripts::invalid)?.kind, "mock poll failure"));
                }
            }
            let (job, left) = s.pending.get_mut(&handle.remote_id).ok_or_else(|| ProviderError::new(ErrorKind::NotFound, "unknown job"))?;
            let model = *self.models.get(&job.model).ok_or_else(|| ProviderError::new(ErrorKind::NotFound, "unknown model"))?;
            match model.fail {
                Some(MockFailure::Poll(kind)) => return Err(ProviderError::new(kind, "mock poll failure")),
                // (A lost reply only affects the submit; polling works.)
                Some(MockFailure::Job(kind)) => return Ok(JobProgress::Failed(ProviderError::new(kind, "mock job failure"))),
                _ => {}
            }
            *left = left.saturating_sub(1);
            if *left > 0 {
                let fraction = 1.0 - (*left as f32 / model.polls.max(1) as f32);
                return Ok(JobProgress::Running { fraction: Some(fraction), message: None, partial: None });
            }
            let done = result(&model, job);
            if let Some(progress) = s.scripted.get_mut(&key) {
                if let Some(kind) = self.scripts.keys.get(&key).and_then(|config| config.failure) {
                    progress.terminal = Some(scripts::Terminal::Failed);
                    return Ok(JobProgress::Failed(ProviderError::new(kind, "mock job failure")));
                }
                progress.terminal = Some(scripts::Terminal::Done);
            }
            Ok(JobProgress::Done(done))
        })();
        Box::pin(async move { outcome })
    }

    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        let mut state = self.lock();
        let refused = state.pending.get(&handle.remote_id).and_then(|(job, _)| self.models.get(&job.model)).and_then(|model| match model.fail {
            Some(MockFailure::Cancel(kind)) => Some(kind),
            _ => None,
        });
        if let Some(kind) = refused {
            state.cancel_refused = state.cancel_refused.saturating_add(1);
            return Box::pin(async move { Err(ProviderError::new(kind, "mock cancel refused")) });
        }
        let key = state.pending.get(&handle.remote_id).map(|(job, _)| job.idempotency_key.clone());
        if let Some(progress) = key.and_then(|key| state.scripted.get_mut(&key)) {
            progress.terminal = Some(scripts::Terminal::Cancelled);
        }
        state.cancelled.push(handle.remote_id.clone());
        Box::pin(async { Ok(()) })
    }

    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        0
    }
}

#[cfg(test)]
mod script_tests;
