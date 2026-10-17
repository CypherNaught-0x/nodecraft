//! A deterministic fake provider. Each model is scripted with a [`MockModel`]; results depend only
//! on the model, its parameters and its inputs, so tests can compare them byte for byte.

use nodecraft_providers::{BoxFuture, Completed, ErrorKind, JobHandle, JobInput, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission};
use nodecraft_types::Money;
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
/// polls, so its `Poll` and `Job` failures happen at submit instead.
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
    polls: u64,
}

/// See the module docs.
#[derive(Clone)]
pub struct MockProvider {
    id: String,
    models: Arc<BTreeMap<String, MockModel>>,
    state: Arc<Mutex<State>>,
}

impl MockProvider {
    /// A mock provider called `id` with `models` (model id → behaviour).
    pub fn new(id: impl Into<String>, models: impl IntoIterator<Item = (String, MockModel)>) -> Self {
        Self { id: id.into(), models: Arc::new(models.into_iter().collect()), state: Arc::default() }
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
            // route it like real output; the claimed type is deliberately vague.
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
            let model = *self.models.get(&job.model).ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "unknown model"))?;
            match model.fail {
                Some(MockFailure::Submit(kind)) => return Err(ProviderError::new(kind, "mock submit failure")),
                Some(MockFailure::Poll(kind) | MockFailure::Job(kind)) if model.polls == 0 => return Err(ProviderError::new(kind, "mock failure")),
                _ => {}
            }
            let next = s.accepted.len();
            let first = !s.accepted.contains_key(&job.idempotency_key);
            let id = s.accepted.entry(job.idempotency_key.clone()).or_insert_with(|| format!("mock-{next}")).clone();
            if first && let Some(MockFailure::LostReply(kind)) = model.fail {
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
            Ok(JobProgress::Done(result(&model, job)))
        })();
        Box::pin(async move { outcome })
    }

    fn cancel<'a>(&'a self, handle: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        self.lock().cancelled.push(handle.remote_id.clone());
        Box::pin(async { Ok(()) })
    }

    fn poll_interval_ms(&self, _: &JobProgress) -> u64 {
        0
    }
}
