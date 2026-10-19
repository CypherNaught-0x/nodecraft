//! Explicit finite fixtures. Counters are shared by clones; reconstruct configuration to reset.
//! Handles are scoped to their provider instance and may alias in another instance.
use super::*;
use std::collections::BTreeSet;

/// Finite failures at one stage. Zero is equivalent to an absent stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MockFailures {
    pub count: u32,
    pub kind: ErrorKind,
}
/// Four distinct lifecycle stages; no payload interpreter or arbitrary event program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MockScript {
    pub submit_failures: Option<MockFailures>,
    pub lost_replies: Option<MockFailures>,
    pub poll_failures: Option<MockFailures>,
    pub failed_generations: Option<MockFailures>,
}
/// One logical operation, with exact sent keys in replacement-generation order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MockOperation {
    pub id: String,
    pub model: String,
    pub sent_keys: Vec<String>,
    pub script: MockScript,
}
#[derive(Default)]
pub(super) struct Scripts {
    pub models: BTreeSet<String>,
    pub keys: BTreeMap<String, Key>,
}
pub(super) struct Key {
    pub model: String,
    pub prior: Option<String>,
    pub failure: Option<ErrorKind>,
    pub script: MockScript,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Terminal {
    Failed,
    Done,
    Cancelled,
}
pub(super) struct Progress {
    pub submit: u32,
    pub lost: u32,
    pub poll: u32,
    pub terminal: Option<Terminal>,
}
impl Progress {
    pub fn new(script: MockScript) -> Self {
        Self {
            submit: script.submit_failures.map_or(0, |f| f.count),
            lost: script.lost_replies.map_or(0, |f| f.count),
            poll: script.poll_failures.map_or(0, |f| f.count),
            terminal: None,
        }
    }
}
pub(super) fn invalid() -> ProviderError {
    ProviderError::new(ErrorKind::UnsupportedInput, "invalid finite mock script configuration or operation key")
}
fn name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.is_ascii()
}
fn stage(f: Option<MockFailures>) -> Result<Option<MockFailures>, ProviderError> {
    match f {
        Some(f) if f.count > 1024 => Err(invalid()),
        Some(f) if f.count == 0 => Ok(None),
        other => Ok(other),
    }
}
impl MockProvider {
    /// A bounded, explicit finite fixture. Legacy `new`/`standard` behavior is unchanged.
    /// Limits apply to retained/copied configuration, not caller-preallocated inputs or logs.
    /// At most 64 models/operations/keys per operation; each name/key is at most 64 ASCII bytes.
    /// Each counter is at most 1024; failed generations additionally need a following key.
    pub fn scripted(
        id: impl AsRef<str>,
        models: impl IntoIterator<Item = (String, MockModel)>,
        operations: impl IntoIterator<Item = MockOperation>,
    ) -> Result<Self, ProviderError> {
        let id = id.as_ref();
        if !name(id) {
            return Err(invalid());
        }
        let mut model_map = BTreeMap::new();
        for (name_id, model) in models {
            if model_map.len() >= 64 || !name(&name_id) || model_map.contains_key(&name_id) {
                return Err(invalid());
            }
            model_map.insert(name_id.as_str().to_owned(), model);
        }
        let mut scripts = Scripts::default();
        let mut ids = BTreeSet::new();
        let mut progress = BTreeMap::new();
        for mut operation in operations {
            if ids.len() >= 64
                || !name(&operation.id)
                || !name(&operation.model)
                || ids.contains(&operation.id)
                || operation.sent_keys.is_empty()
                || operation.sent_keys.len() > 64
                || operation.sent_keys.iter().any(|key| !name(key))
            {
                return Err(invalid());
            }
            let model = model_map.get(&operation.model).ok_or_else(invalid)?;
            if model.fail.is_some() {
                return Err(invalid());
            }
            operation.script = MockScript {
                submit_failures: stage(operation.script.submit_failures)?,
                lost_replies: stage(operation.script.lost_replies)?,
                poll_failures: stage(operation.script.poll_failures)?,
                failed_generations: stage(operation.script.failed_generations)?,
            };
            let failures = operation.script.failed_generations.map_or(0, |f| f.count);
            let max_failures = operation.sent_keys.len().checked_sub(1).ok_or_else(invalid)?;
            let failures = usize::try_from(failures).map_err(|_| invalid())?;
            if failures > max_failures || (model.polls == 0 && (operation.script.poll_failures.is_some() || failures > 0)) {
                return Err(invalid());
            }
            ids.insert(operation.id.as_str().to_owned());
            scripts.models.insert(operation.model.as_str().to_owned());
            let mut prior = None;
            for (ordinal, key) in operation.sent_keys.into_iter().enumerate() {
                if scripts.keys.len() >= 4096 || scripts.keys.contains_key(&key) {
                    return Err(invalid());
                }
                let failure = operation.script.failed_generations.filter(|_| ordinal < failures).map(|f| f.kind);
                progress.insert(key.as_str().to_owned(), Progress::new(operation.script));
                scripts.keys.insert(key.as_str().to_owned(), Key { model: operation.model.as_str().to_owned(), prior, failure, script: operation.script });
                prior = Some(key.as_str().to_owned());
            }
        }
        Ok(Self {
            id: id.to_owned(),
            models: Arc::new(model_map),
            scripts: Arc::new(scripts),
            state: Arc::new(Mutex::new(State { scripted: progress, ..State::default() })),
        })
    }
}
