//! Fixture-only gates wait BEFORE invoking the mock's eager future constructor.
use super::{
    bounds::{self, Check, MIB},
    runtime::Fault,
};
use nodecraft_providers::{
    BoxFuture, ErrorKind, JobHandle, JobInput, JobProgress, Provider, ProviderError, ProviderJob, Submission,
    mock::{MockModel, MockOperation, MockProvider},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll, Waker},
};
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
struct GateState {
    open: bool,
    wakers: [Option<Waker>; 256],
}
impl Default for GateState {
    fn default() -> Self {
        Self { open: false, wakers: std::array::from_fn(|_| None) }
    }
}
/// Fixed live waiter slots. Dropping a cancelled wait removes its waker immediately.
#[derive(Clone, Default)]
pub struct Gate(Arc<Mutex<GateState>>);
impl Gate {
    pub fn open(&self) {
        let wakes = {
            let mut s = lock(&self.0);
            s.open = true;
            std::mem::replace(&mut s.wakers, std::array::from_fn(|_| None))
        };
        for w in wakes.into_iter().flatten() {
            w.wake();
        }
    }
    pub fn is_open(&self) -> bool {
        lock(&self.0).open
    }
    pub fn wait(&self) -> GateWait {
        GateWait { gate: self.clone(), slot: None }
    }
}
pub struct GateWait {
    gate: Gate,
    slot: Option<usize>,
}
impl Future for GateWait {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let gate = self.gate.0.clone();
        let mut s = lock(&gate);
        if s.open {
            return Poll::Ready(());
        }
        let index = match self.slot {
            Some(i) => Some(i),
            None => s.wakers.iter().position(Option::is_none),
        };
        if let Some(i) = index
            && let Some(w) = s.wakers.get_mut(i)
        {
            *w = Some(cx.waker().clone());
            self.slot = Some(i);
        }
        Poll::Pending
    }
}
impl Drop for GateWait {
    fn drop(&mut self) {
        if let Some(i) = self.slot
            && let Some(slot) = lock(&self.gate.0).wakers.get_mut(i)
        {
            *slot = None;
        }
    }
}
/// Only synthetic mock configuration is admitted, never an arbitrary live Provider.
pub struct AdapterFixture {
    pub id: String,
    pub instance: Option<nodecraft_types::ProviderInstanceId>,
    pub models: Vec<(String, MockModel)>,
    pub operations: Vec<MockOperation>,
    pub submit_open: bool,
    pub poll_open: bool,
    pub synthetic: Option<super::synthetic::SyntheticFixture>,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Calls {
    pub submits: usize,
    pub polls: usize,
    pub cancels: usize,
    pub bytes: usize,
}
pub struct Gated {
    pub mock: MockProvider,
    pub synthetic: bool,
    pub submit_gate: Gate,
    pub poll_gate: Gate,
    pub calls: Mutex<Calls>,
    global: Arc<Mutex<Calls>>,
    fault: Fault,
    inner: Arc<dyn Provider>,
}
impl AdapterFixture {
    pub(super) fn build(self, fault: Fault, emissions: Arc<Mutex<super::synthetic::Emissions>>, global: Arc<Mutex<Calls>>) -> Check<Arc<Gated>> {
        if self.id.len() > 64
            || self.id.capacity() > 64
            || self.models.len() > 64
            || self.models.capacity() > 64
            || self.operations.len() > 64
            || self.operations.capacity() > 64
        {
            return Err("fixture_models");
        }
        let mut bytes = self.id.len();
        for (id, m) in &self.models {
            if id.len() > 64 || id.capacity() > 64 || m.polls > 16 {
                return Err("fixture_model");
            }
            bytes = bytes.checked_add(id.len() + 128).ok_or("fixture_overflow")?;
        }
        for op in &self.operations {
            for x in [op.script.submit_failures, op.script.lost_replies, op.script.poll_failures, op.script.failed_generations].into_iter().flatten() {
                if x.count > 4 {
                    return Err("fixture_failures");
                }
            }
            if op.id.len() > 64 || op.model.len() > 64 || op.sent_keys.capacity() > 8 || op.sent_keys.len() > 8 {
                return Err("fixture_keys");
            }
            bytes = bytes.checked_add(op.id.len() + op.model.len() + 128).ok_or("fixture_overflow")?;
            for key in &op.sent_keys {
                if key.len() > 64 {
                    return Err("fixture_key");
                }
                bytes = bytes.checked_add(key.len() + 128).ok_or("fixture_overflow")?;
            }
        }
        let admitted = self.synthetic.map(|s| s.admit(&emissions)).transpose()?;
        if let Some(s) = &admitted {
            bytes = bytes.checked_add(s.charge).ok_or("fixture_bytes")?;
        }
        if bytes > 8 * MIB {
            return Err("fixture_bytes");
        }
        {
            let mut all = lock(&global);
            all.bytes = all.bytes.checked_add(bytes).filter(|n| *n <= 8 * MIB).ok_or("fixture_bytes")?;
        }
        let mock = if self.operations.is_empty() {
            MockProvider::new(self.id, self.models)
        } else {
            MockProvider::scripted(self.id, self.models, self.operations).map_err(|_| "fixture_script")?
        };
        let synthetic = admitted.is_some();
        let inner: Arc<dyn Provider> = match admitted {
            Some(s) => Arc::new(s.attach(mock.clone(), emissions, fault.clone())),
            None => Arc::new(mock.clone()),
        };
        let submit_gate = Gate::default();
        let poll_gate = Gate::default();
        if self.submit_open {
            submit_gate.open();
        }
        if self.poll_open {
            poll_gate.open();
        }
        Ok(Arc::new(Gated { mock, submit_gate, poll_gate, calls: Mutex::new(Calls::default()), global, fault, inner, synthetic }))
    }
}
impl Gated {
    fn enter(&self, kind: u8, job: Option<&ProviderJob>) -> Result<(), ProviderError> {
        let mut global = lock(&self.global);
        let mut c = lock(&self.calls);
        let total = global
            .submits
            .checked_add(global.polls)
            .and_then(|n| n.checked_add(global.cancels))
            .ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "fixture_call_limit"))?;
        if total >= 32768 {
            self.fault.set("provider_calls");
            return Err(ProviderError::new(ErrorKind::UnsupportedInput, "fixture_call_limit"));
        }
        if let Some(j) = job {
            let mut n = j.model.len().checked_add(j.idempotency_key.len()).ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "fixture_request"))?;
            let q = bounds::params(&j.params);
            if let Ok(q) = q {
                n = n.saturating_add(q.owned.saturating_mul(6));
            } else {
                return Err(ProviderError::new(ErrorKind::UnsupportedInput, "fixture_request"));
            }
            // The mock Text formatter adds model, separators and a fixed digest. Bound its
            // future output before invoking the eager constructor or copying the request log.
            let text = j
                .inputs
                .iter()
                .filter_map(|i| match i {
                    JobInput::Text { text, .. } => Some(text.len()),
                    _ => None,
                })
                .try_fold(0usize, |n, len| n.checked_add(len).and_then(|n| n.checked_add(3)));
            if text.and_then(|n| n.checked_add(j.model.len() + 32)).is_none_or(|n| n > 8192) {
                return Err(ProviderError::new(ErrorKind::UnsupportedInput, "fixture_completion_bound"));
            }
            for i in &j.inputs {
                let len = match i {
                    JobInput::Text { role, text } => role.len().saturating_add(text.len()),
                    JobInput::Media { role, mime, bytes } => role.len().saturating_add(mime.len()).saturating_add(bytes.len()),
                };
                if len > 65536 {
                    return Err(ProviderError::new(ErrorKind::UnsupportedInput, "fixture_request"));
                }
                n = n.saturating_add(len + 128);
            }
            let next = global
                .bytes
                .checked_add(n.saturating_mul(4))
                .filter(|n| *n <= 8 * MIB)
                .ok_or_else(|| ProviderError::new(ErrorKind::UnsupportedInput, "fixture_request"))?;
            let own = next.saturating_sub(global.bytes);
            c.bytes = c.bytes.saturating_add(own);
            global.bytes = next;
        }
        match kind {
            0 => {
                c.submits += 1;
                global.submits += 1;
            }
            1 => {
                c.polls += 1;
                global.polls += 1;
            }
            _ => {
                c.cancels += 1;
                global.cancels += 1;
            }
        }
        Ok(())
    }
    pub fn counters(&self) -> Calls {
        *lock(&self.calls)
    }
    pub fn requests(&self) -> Check<Vec<ProviderJob>> {
        if lock(&self.calls).bytes > 8 * MIB {
            return Err("request_snapshot");
        }
        Ok(self.mock.requests())
    }
}
impl Provider for Gated {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn retry_ambiguous_submit(&self) -> bool {
        self.inner.retry_ambiguous_submit()
    }
    fn poll_interval_ms(&self, p: &JobProgress) -> u64 {
        self.inner.poll_interval_ms(p)
    }
    fn submit<'a>(&'a self, j: &'a ProviderJob) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            self.submit_gate.wait().await;
            self.enter(0, Some(j))?;
            self.inner.submit(j).await
        })
    }
    fn submit_with_delivery<'a>(
        &'a self,
        j: &'a ProviderJob,
        context: &'a nodecraft_providers::SubmitContext,
    ) -> BoxFuture<'a, Result<Submission, ProviderError>> {
        Box::pin(async move {
            self.submit_gate.wait().await;
            self.enter(0, Some(j))?;
            self.inner.submit_with_delivery(j, context).await
        })
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            self.poll_gate.wait().await;
            self.enter(1, None)?;
            self.inner.poll(h).await
        })
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        Box::pin(async move {
            self.enter(2, None)?;
            self.inner.cancel(h).await
        })
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "native_tests.rs"]
mod native_tests;

#[cfg(test)]
pub(super) fn fixed_gate_storage() -> usize {
    2 * (std::mem::size_of::<GateState>() + 128)
}
