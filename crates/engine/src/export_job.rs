//! Read-only export jobs retain their own asset snapshot; document edits never wait for them.
#[cfg(not(target_arch = "wasm32"))]
use crate::save_runner::SaveJob;
use crate::{export::Exported, save_runner::SaveRunner};
use nodecraft_format::{AssetStore, asset_source::TransferCancel};
use nodecraft_types::{Error, ErrorKind};
use serde::Serialize;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPhase {
    Queued,
    Checking,
    Writing,
    Finishing,
    Completed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ExportResult {
    Exported { bytes: u64 },
    Results { report: crate::results_export::Report },
    Cancelled,
    Failed { error: Error, name_taken: bool },
}

/// Contains no destination path or project content. The most recent outcome remains until
/// another export is admitted, including cancellation and an abandoned runner job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportStatus {
    pub id: u64,
    pub phase: ExportPhase,
    pub cancellable: bool,
    pub result: Option<ExportResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<ExportProgress>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportProgress {
    pub completed: u32,
    pub total: u32,
    pub written: u32,
    pub failed: u32,
}

/// A read-only receipt remains observable after its Session is replaced or dropped. It
/// retains no project, destination path or cancellation authority.
#[derive(Clone)]
pub struct ExportWatch(Arc<Control>);
impl ExportWatch {
    pub fn status(&self) -> ExportStatus {
        self.0.status()
    }
    /// Receipt identity survives Session replacement; numeric ids alone are session-local.
    pub fn same_job(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

pub(crate) struct Control {
    state: Mutex<ExportStatus>,
    pub(crate) cancel: TransferCancel,
}
impl Control {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn phase(&self, phase: ExportPhase) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.cancel.check()?;
        state.phase = phase;
        // Cancellation and this transition use the same short lock. Once Finishing is visible,
        // publication cannot be cancelled: the atomic writer may be syncing or renaming.
        state.cancellable = phase != ExportPhase::Finishing;
        Ok(())
    }
    fn status(&self) -> ExportStatus {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn finish(&self, outcome: Result<u64, Error>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // An accepted cancel can race with a filesystem error that does not inspect the
        // token. Serialize its terminal result with cancellation and the finishing boundary.
        let result = if state.phase != ExportPhase::Finishing && self.cancel.check().is_err() {
            ExportResult::Cancelled
        } else {
            match outcome {
                Ok(bytes) => ExportResult::Exported { bytes },
                Err(error) if TransferCancel::is_cancellation(&error) => ExportResult::Cancelled,
                Err(error) => {
                    let name_taken = crate::export::name_conflict(&error);
                    ExportResult::Failed { error, name_taken }
                }
            }
        };
        state.result = Some(result);
        state.phase = ExportPhase::Completed;
        state.cancellable = false;
    }
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    pub(crate) fn progress(&self, completed: u32, total: u32, written: u32, failed: u32) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.phase = ExportPhase::Writing;
        state.progress = Some(ExportProgress { completed, total, written, failed });
    }
    #[cfg(all(unix, not(target_arch = "wasm32")))]
    pub(crate) fn finalizing(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.phase = ExportPhase::Finishing;
        state.cancellable = false;
    }
    #[cfg(not(target_arch = "wasm32"))]
    fn finish_results(&self, outcome: Result<crate::results_export::Report, Error>) {
        match outcome {
            Ok(report) => {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                state.result = Some(ExportResult::Results { report });
                state.phase = ExportPhase::Completed;
                state.cancellable = false;
            }
            Err(error) => self.finish(Err(error)),
        }
    }
    fn request_cancel(&self) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.cancellable {
            return Err(Error::new(ErrorKind::Conflict, "the export is finishing or has completed; it can no longer be cancelled"));
        }
        self.cancel.cancel();
        state.cancellable = false;
        Ok(())
    }
}

/// A blocked filesystem call retains the only process export slot until it returns. Cancelling
/// or replacing a Session must not create an unbounded series of replacement threads.
#[cfg(not(target_arch = "wasm32"))]
struct Lease(&'static AtomicBool);
#[cfg(not(target_arch = "wasm32"))]
impl Lease {
    fn acquire() -> Result<Self, Error> {
        static BUSY: AtomicBool = AtomicBool::new(false);
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(&BUSY))
            .map_err(|_| Error::new(ErrorKind::Busy, "another export is still finishing; wait before exporting again"))
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct Completion {
    control: Arc<Control>,
    lease: Option<Lease>,
    finished: bool,
}
#[cfg(not(target_arch = "wasm32"))]
impl Completion {
    fn finish_results(&mut self, result: Result<crate::results_export::Report, Error>) {
        self.lease = None;
        self.control.finish_results(result);
        self.finished = true;
    }
    fn finish(&mut self, result: Result<u64, Error>) {
        self.lease = None;
        self.control.finish(result);
        self.finished = true;
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Completion {
    fn drop(&mut self) {
        if !self.finished {
            self.lease = None;
            self.control.finish(
                self.control.cancel.check().and_then(|()| Err(Error::new(ErrorKind::Internal, "the export worker stopped before finishing; export again"))),
            );
        }
    }
}

#[derive(Default)]
pub(crate) struct Exports {
    #[cfg(not(target_arch = "wasm32"))]
    runner: Option<Arc<dyn SaveRunner>>,
    #[cfg(not(target_arch = "wasm32"))]
    next: u64,
    current: Option<Arc<Control>>,
}
impl Exports {
    pub(crate) fn set_runner(&mut self, runner: Arc<dyn SaveRunner>) {
        #[cfg(target_arch = "wasm32")]
        let _ = runner;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.runner = Some(runner);
        }
    }
    pub(crate) fn status(&self) -> Option<ExportStatus> {
        self.current.as_ref().map(|control| control.status())
    }
    pub(crate) fn watch(&self) -> Option<ExportWatch> {
        self.current.as_ref().map(|control| ExportWatch(Arc::clone(control)))
    }
    pub(crate) fn cancellable(&self, id: u64) -> Result<(), Error> {
        let status = self.status().filter(|status| status.id == id).ok_or_else(|| Error::new(ErrorKind::NotFound, "that export is no longer current"))?;
        if status.cancellable {
            Ok(())
        } else {
            Err(Error::new(ErrorKind::Conflict, "the export is finishing or has completed; it can no longer be cancelled"))
        }
    }
    pub(crate) fn cancel(&self, id: u64) -> Result<(), Error> {
        self.cancellable(id)?;
        self.current.as_ref().ok_or_else(|| Error::new(ErrorKind::NotFound, "that export is no longer current"))?.request_cancel()
    }
    pub(crate) fn start(&mut self, assets: &AssetStore, what: Exported, path: PathBuf, overwrite: bool) -> Result<u64, Error> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (assets, what, path, overwrite);
            Err(Error::new(ErrorKind::Unsupported, "exporting to a file needs the desktop app"))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.status().is_some_and(|status| status.result.is_none()) {
                return Err(Error::new(ErrorKind::Busy, "an export is already in progress; wait or cancel it"));
            }
            let runner = self.runner.as_ref().ok_or_else(|| Error::new(ErrorKind::Unsupported, "background export needs an export runner"))?;
            let id = self.next.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "too many exports; reopen the project"))?;
            let lease = Lease::acquire()?;
            let control = Arc::new(Control {
                state: Mutex::new(ExportStatus { id, phase: ExportPhase::Queued, cancellable: true, result: None, progress: None }),
                cancel: TransferCancel::default(),
            });
            let snapshot = assets.clone();
            let mut completion = Completion { control: Arc::clone(&control), lease: Some(lease), finished: false };
            let job = SaveJob::new(move || {
                let result = crate::session::guard(|| crate::export::write_background(&snapshot, &what, &path, overwrite, &completion.control));
                drop((snapshot, what, path));
                completion.finish(result);
            });
            // Publish the receipt before handing off: even a runner that drops a queued job
            // produces a terminal outcome through Completion. A rejected spawn never runs inline.
            self.next = id;
            self.current = Some(control);
            if let Err(job) = runner.spawn(job) {
                drop(job);
                return Err(Error::new(ErrorKind::Internal, "the export worker couldn't start; export again"));
            }
            Ok(id)
        }
    }
    pub(crate) fn start_results(&mut self, assets: &AssetStore, plan: crate::results_export::Plan) -> Result<u64, Error> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (assets, plan);
            Err(Error::new(ErrorKind::Unsupported, "exporting to a file needs the desktop app"))
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.status().is_some_and(|status| status.result.is_none()) {
                return Err(Error::new(ErrorKind::Busy, "an export is already in progress; wait or cancel it"));
            }
            let runner = self.runner.as_ref().ok_or_else(|| Error::new(ErrorKind::Unsupported, "background export needs an export runner"))?;
            let id = self.next.checked_add(1).ok_or_else(|| Error::new(ErrorKind::Overflow, "too many exports; reopen the project"))?;
            let lease = Lease::acquire()?;
            let control = Arc::new(Control {
                state: Mutex::new(ExportStatus { id, phase: ExportPhase::Queued, cancellable: true, result: None, progress: None }),
                cancel: TransferCancel::default(),
            });
            let snapshot = assets.clone();
            let mut completion = Completion { control: Arc::clone(&control), lease: Some(lease), finished: false };
            let job = SaveJob::new(move || {
                let result = crate::session::guard(|| crate::results_export::run(&snapshot, plan, &completion.control));
                drop(snapshot);
                completion.finish_results(result);
            });
            // Publish the receipt before handing off: even a runner that drops a queued job
            // produces a terminal outcome through Completion. A rejected spawn never runs inline.
            self.next = id;
            self.current = Some(control);
            if let Err(job) = runner.spawn(job) {
                drop(job);
                return Err(Error::new(ErrorKind::Internal, "the export worker couldn't start; export again"));
            }
            Ok(id)
        }
    }
}
impl Drop for Exports {
    fn drop(&mut self) {
        if let Some(control) = &self.current {
            control.cancel.cancel();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
