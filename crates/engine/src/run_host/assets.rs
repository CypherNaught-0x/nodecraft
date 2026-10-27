//! Large conversion and sealing runs in the job worker; pump only adopts receipts.
use crate::{Session, generate, jobs::CompletionDelivery};
use nodecraft_exec::{driver::Runtime, scheduler::JobId};
use nodecraft_format::{AssetStore, asset_source::TransferCancel};
#[cfg(not(target_arch = "wasm32"))]
use nodecraft_format::{
    SealedAsset,
    asset_source::{AssetRange, MemorySource, StreamMetrics},
};
use nodecraft_nodes::GenerateOutput;
use nodecraft_providers::{Completed, Output};
use nodecraft_types::{Blake3, Value};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::Mutex;
use std::{collections::BTreeMap, sync::Arc, task::Poll};
#[cfg(test)]
pub(super) type Hook = Arc<dyn Fn() + Send + Sync>;
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
struct Input {
    store: Arc<AssetStore>,
    routes: Vec<GenerateOutput>,
    cancel: TransferCancel,
    #[cfg(test)]
    hook: Option<Hook>,
}
pub(super) struct Prepared {
    made: generate::Produced,
    #[cfg(not(target_arch = "wasm32"))]
    sealed: Vec<SealedAsset>,
}
impl Prepared {
    pub(super) fn outputs(&self) -> &[(String, Value)] {
        &self.made.outputs
    }
    pub(super) fn take_outputs(&mut self) -> Vec<(String, Value)> {
        std::mem::take(&mut self.made.outputs)
    }
    pub(super) fn adopt(self, session: &mut Session) -> Result<Vec<(String, Value)>, String> {
        #[cfg(not(target_arch = "wasm32"))]
        for sealed in self.sealed {
            session.adopt_sealed_asset(sealed).map_err(|error| error.message)?;
        }
        for (hash, bytes) in self.made.assets {
            import_small(session, hash, bytes)?;
        }
        Ok(self.made.outputs)
    }
}
fn large(completed: &Completed) -> bool {
    completed.outputs.iter().any(|output| matches!(output,Output::Bytes{bytes,..} if bytes.len()>nodecraft_format::MAX_ASSET_BYTES))
}
#[cfg(not(target_arch = "wasm32"))]
fn prepare(input: &Input, completed: &Completed) -> Result<Prepared, String> {
    input.cancel.check().map_err(|error| error.message)?;
    if !input.store.context().supports_large_assets() {
        return Err("large generated images need streaming asset storage on this host".into());
    }
    #[cfg(test)]
    if let Some(hook) = &input.hook {
        hook();
    }
    input.cancel.check().map_err(|error| error.message)?;
    // Includes aggregate admission and hashing; no large conversion runs on the frame.
    let mut made = generate::produced(&input.routes, completed).map_err(|error| error.message)?;
    let mut sealed = Vec::new();
    let mut small = Vec::new();
    for (hash, bytes) in std::mem::take(&mut made.assets) {
        if bytes.len() <= nodecraft_format::MAX_ASSET_BYTES {
            small.push((hash, bytes));
            continue;
        }
        let length = u64::try_from(bytes.len()).map_err(|_| "generated asset extent is out of range")?;
        let source = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, length).map_err(|error| error.message)?;
        let receipt = input.store.seal(&source, &input.cancel, &StreamMetrics::default()).map_err(|error| error.message)?;
        if receipt.hash() != hash {
            return Err("a generated asset changed while it was sealed".into());
        }
        sealed.push(receipt);
    }
    made.assets = small;
    Ok(Prepared { made, sealed })
}
pub(super) fn ready(routes: &[GenerateOutput], completed: &Completed, prepared: Option<Result<Prepared, String>>) -> Result<Prepared, String> {
    match prepared {
        Some(result) => result,
        None if large(completed) => Err("large generated results need worker preparation".into()),
        None => Ok(Prepared {
            made: generate::produced(routes, completed).map_err(|error| error.message)?,
            #[cfg(not(target_arch = "wasm32"))]
            sealed: Vec::new(),
        }),
    }
}
fn import_small(session: &mut Session, expected: Blake3, bytes: Arc<[u8]>) -> Result<(), String> {
    if bytes.len() > nodecraft_format::MAX_ASSET_BYTES {
        return Err("large generated results need worker preparation".into());
    }
    if session.import_asset(&bytes).map_err(|error| error.message)? != expected {
        return Err("a generated asset changed while it was imported".into());
    }
    Ok(())
}
pub(super) struct Delivery {
    pub completion: CompletionDelivery,
    pub prepared: Option<Result<Prepared, String>>,
    pub preparing: bool,
}
impl Delivery {
    /// Called on the job worker: retain the paid original before potentially slow preparation.
    pub(super) fn new(completion: CompletionDelivery) -> (Self, Option<Completed>) {
        let work = large(&completion.completed).then(|| completion.completed.clone());
        (Self { completion, preparing: work.is_some(), prepared: None }, work)
    }
}
/// The owner cancels on close/drop. Workers clone the map, never the owning scope.
#[derive(Default)]
pub(super) struct Scope {
    #[cfg(not(target_arch = "wasm32"))]
    inputs: Arc<Mutex<BTreeMap<JobId, Input>>>,
    #[cfg(not(target_arch = "wasm32"))]
    busy: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    pub(super) hook: Option<Hook>,
}
#[derive(Clone)]
pub(super) struct Worker(
    #[cfg(not(target_arch = "wasm32"))] Arc<Mutex<BTreeMap<JobId, Input>>>,
    #[cfg(not(target_arch = "wasm32"))] Arc<std::sync::atomic::AtomicUsize>,
);
impl Scope {
    pub(super) fn worker(&self) -> Worker {
        Worker(
            #[cfg(not(target_arch = "wasm32"))]
            self.inputs.clone(),
            #[cfg(not(target_arch = "wasm32"))]
            self.busy.clone(),
        )
    }
    pub(super) fn is_busy(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            return self.busy.load(std::sync::atomic::Ordering::SeqCst) > 0;
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }
    pub(super) fn register(&self, job: JobId, store: Arc<AssetStore>, routes: Vec<GenerateOutput>) {
        #[cfg(not(target_arch = "wasm32"))]
        super::lock(&self.inputs).insert(
            job,
            Input {
                store,
                routes,
                cancel: TransferCancel::default(),
                #[cfg(test)]
                hook: self.hook.clone(),
            },
        );
        #[cfg(target_arch = "wasm32")]
        let _ = (job, store, routes);
    }
    pub(super) fn forget(&self, job: JobId) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            super::lock(&self.inputs).remove(&job);
        }
        #[cfg(target_arch = "wasm32")]
        let _ = job;
    }
    pub(super) fn cancel(&self, job: JobId) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(input) = super::lock(&self.inputs).get(&job) {
            input.cancel.cancel();
        }
        #[cfg(target_arch = "wasm32")]
        let _ = job;
    }
    pub(super) fn cancel_all(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        for input in super::lock(&self.inputs).values() {
            input.cancel.cancel();
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.cancel_all();
    }
}
impl Worker {
    pub(super) fn prepare(&self, job: JobId, completed: &Completed) -> Result<Prepared, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _busy = Busy(self.1.clone());
            let input = super::lock(&self.0).get(&job).cloned().ok_or("generated result preparation input is missing")?;
            prepare(&input, completed)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (job, completed);
            Err("large generated images need native streaming asset storage".into())
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
struct Busy(Arc<std::sync::atomic::AtomicUsize>);
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Busy {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
struct Pending {
    #[cfg(not(target_arch = "wasm32"))]
    done: std::sync::mpsc::Receiver<Result<Prepared, String>>,
    cancel: TransferCancel,
}
/// Held actions stay pending while a worker owns their byte Arcs/context. Poll never waits.
#[derive(Default)]
pub(super) struct Actions {
    pending: BTreeMap<u64, Pending>,
    #[cfg(test)]
    pub(super) hook: Option<Hook>,
}
impl Actions {
    pub(super) fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }
    pub(super) fn poll(
        &mut self,
        id: u64,
        store: &AssetStore,
        routes: &[GenerateOutput],
        result: Arc<super::held::HeldResult>,
        runtime: &Arc<dyn Runtime>,
    ) -> Poll<Result<Prepared, String>> {
        if !large(&result.completed) {
            return Poll::Ready(ready(routes, &result.completed, None));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (id, store, runtime);
            return Poll::Ready(Err("large generated images need native streaming asset storage".into()));
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(pending) = self.pending.get_mut(&id) {
                let outcome = match pending.done.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => return Poll::Pending,
                    Ok(outcome) => outcome,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("generated result preparation worker stopped".into()),
                };
                self.pending.remove(&id);
                return Poll::Ready(outcome);
            }
            if self.pending.len() >= super::MAX_HELD {
                return Poll::Ready(Err("too many generated results are being prepared".into()));
            }
            let input = Input {
                store: Arc::new(store.clone()),
                routes: routes.to_vec(),
                cancel: TransferCancel::default(),
                #[cfg(test)]
                hook: self.hook.clone(),
            };
            let cancel = input.cancel.clone();
            let (send, done) = std::sync::mpsc::sync_channel(1);
            runtime.spawn(Box::pin(async move {
                let prepared = prepare(&input, &result.completed);
                let _ = send.send(prepared);
            }));
            self.pending.insert(id, Pending { done, cancel });
            Poll::Pending
        }
    }
    pub(super) fn cancel(&mut self, id: u64) {
        if let Some(pending) = self.pending.remove(&id) {
            pending.cancel.cancel();
        }
    }
    pub(super) fn cancel_all(&mut self) {
        for (_, pending) in std::mem::take(&mut self.pending) {
            pending.cancel.cancel();
        }
    }
}
impl Drop for Actions {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
