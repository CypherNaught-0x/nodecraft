//! Explicit synthetic scenario8 output, separate from transparent gate instrumentation.
use super::{
    bounds::{Check, MIB},
    runtime::Fault,
};
use nodecraft_providers::{BoxFuture, Completed, JobHandle, JobProgress, Output, Provider, ProviderError, ProviderJob, Submission, mock::MockProvider};
use std::sync::{Arc, Mutex, PoisonError};
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
/// Only output payloads are configurable. Genuine inner usage is always preserved.
pub struct SyntheticFixture {
    pub outputs: Option<Vec<Output>>,
    pub partial_keys: Vec<String>,
}
#[derive(Default)]
pub(super) struct Emissions {
    pub slots: usize,
    pub configured: usize,
}
impl Emissions {
    pub fn activate(&mut self, attempts: usize, cap: usize) -> Check<usize> {
        let n = attempts
            .checked_mul(2 * 8192)
            .and_then(|n| n.checked_add(self.configured))
            .and_then(|n| n.checked_add(if self.configured > 0 { 8192 } else { 0 }))
            .ok_or("original_ownership")?;
        if cap > 8 * MIB || n > cap {
            return Err("original_ownership");
        }
        self.slots = attempts;
        Ok(n)
    }
}
struct Partial {
    key: String,
    remote: Option<String>,
    used: bool,
}
pub(super) struct SyntheticOutput {
    inner: MockProvider,
    outputs: Option<Vec<Output>>,
    partial: Mutex<Vec<Partial>>,
    emissions: Arc<Mutex<Emissions>>,
    fault: Fault,
}
pub(super) struct AdmittedSynthetic {
    config: SyntheticFixture,
    pub charge: usize,
}
impl SyntheticFixture {
    pub(super) fn admit(self, emissions: &Arc<Mutex<Emissions>>) -> Check<AdmittedSynthetic> {
        if self.partial_keys.len() > 64 || self.partial_keys.capacity() > 64 {
            return Err("synthetic_operations");
        }
        let mut charge =
            self.partial_keys.capacity().checked_mul(std::mem::size_of::<String>()).and_then(|n| n.checked_add(256)).ok_or("synthetic_ownership")?;
        for (i, key) in self.partial_keys.iter().enumerate() {
            if key.is_empty() || key.len() > 64 || key.capacity() > 64 || !key.is_ascii() || self.partial_keys.iter().take(i).any(|k| k == key) {
                return Err("synthetic_identity");
            }
            charge = charge.checked_add(256 + key.capacity()).ok_or("synthetic_ownership")?;
        }
        if let Some(outputs) = &self.outputs {
            if outputs.len() > 16 || outputs.capacity() > 16 {
                return Err("synthetic_outputs");
            }
            charge =
                charge.checked_add(outputs.capacity().checked_mul(std::mem::size_of::<Output>()).ok_or("synthetic_ownership")?).ok_or("synthetic_ownership")?;
            let mut raw = 0usize;
            for output in outputs {
                let n = match output {
                    Output::Text(s) => s.capacity(),
                    Output::Bytes { mime, bytes } => {
                        if mime.len() > 64 || mime.capacity() > 64 {
                            return Err("synthetic_mime");
                        }
                        charge = charge.checked_add(mime.capacity()).ok_or("synthetic_ownership")?;
                        bytes.len()
                    }
                    Output::Url { .. } => return Err("synthetic_url"),
                };
                raw = raw.checked_add(n).ok_or("synthetic_ownership")?;
            }
            if raw > 8192 {
                return Err("synthetic_payload");
            }
            charge = charge.checked_add(raw).ok_or("synthetic_ownership")?;
        }
        if charge > 16384 {
            return Err("synthetic_configuration");
        }
        let mut e = lock(emissions);
        e.configured = e.configured.checked_add(charge).filter(|n| *n <= 8 * MIB).ok_or("synthetic_ownership")?;
        drop(e);
        Ok(AdmittedSynthetic { config: self, charge })
    }
}
impl AdmittedSynthetic {
    pub(super) fn attach(self, inner: MockProvider, emissions: Arc<Mutex<Emissions>>, fault: Fault) -> SyntheticOutput {
        SyntheticOutput {
            inner,
            outputs: self.config.outputs,
            partial: Mutex::new(self.config.partial_keys.into_iter().map(|key| Partial { key, remote: None, used: false }).collect()),
            emissions,
            fault,
        }
    }
}
impl SyntheticOutput {
    fn terminal(&self, mut done: Completed) -> Completed {
        let Some(outputs) = &self.outputs else { return done };
        let mut e = lock(&self.emissions);
        if e.slots == 0 {
            self.fault.set("synthetic_emission_slot");
            return done;
        }
        e.slots -= 1;
        drop(e);
        // Configuration and every possible emitted owner were jointly charged before provider work.
        // Only outputs change: never replace usage, accounting state or delivery proof.
        done.outputs = outputs.clone();
        done
    }
    fn associate(&self, key: &str, handle: &JobHandle) {
        let mut p = lock(&self.partial);
        let Some(slot) = p.iter_mut().find(|p| p.key == key) else { return };
        if let Some(remote) = &slot.remote {
            if remote != &handle.remote_id {
                self.fault.set("synthetic_handle_replaced");
            }
            return;
        }
        if handle.remote_id.len() > 64 || handle.remote_id.capacity() > 64 {
            self.fault.set("synthetic_handle_bound");
            return;
        }
        slot.remote = Some(handle.remote_id.clone());
    }
}
impl Provider for SyntheticOutput {
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
            Ok(match self.inner.submit(j).await? {
                Submission::Finished(c) => Submission::Finished(self.terminal(c)),
                Submission::Queued(h) => {
                    self.associate(&j.idempotency_key, &h);
                    Submission::Queued(h)
                }
            })
        })
    }
    fn poll<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<JobProgress, ProviderError>> {
        Box::pin(async move {
            let emit = {
                let mut p = lock(&self.partial);
                if let Some(slot) = p.iter_mut().find(|s| s.remote.as_ref() == Some(&h.remote_id)) {
                    if slot.used {
                        false
                    } else {
                        slot.used = true;
                        true
                    }
                } else {
                    false
                }
            };
            if emit {
                return Ok(JobProgress::Running { fraction: Some(0.5), message: None, partial: Some(Output::Text("synthetic fixture partial".into())) });
            }
            Ok(match self.inner.poll(h).await? {
                JobProgress::Done(c) => JobProgress::Done(self.terminal(c)),
                other => other,
            })
        })
    }
    fn cancel<'a>(&'a self, h: &'a JobHandle) -> BoxFuture<'a, Result<(), ProviderError>> {
        self.inner.cancel(h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn work_ceiling_and_byte_ceiling_have_distinct_boundaries() {
        let mut e = Emissions::default();
        assert_eq!(e.activate(512, 8 * MIB).unwrap(), 8 * MIB);
        assert!(e.activate(1024, 8 * MIB).is_err());
        let mut configured = Emissions { configured: 1, ..Default::default() };
        assert!(configured.activate(512, 8 * MIB).is_err());
        assert_eq!(configured.slots, 0);
    }
}
