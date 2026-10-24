//! Captures serialize process-wide, but their devices belong to the capture's worker stack.
use nodecraft_gpu::mesh::capture::MeshCapture;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicBool, Ordering};

type Slot = Option<MeshCapture>;

/// Closing allows one forty-second GPU readback plus five seconds for teardown. Driver calls
/// which never return can outlive this bound; a nonzero report explicitly leaves them running.
#[cfg(not(target_arch = "wasm32"))]
pub const CLOSE_WAIT: std::time::Duration = std::time::Duration::from_secs(45);

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shutdown {
    pub remaining: usize,
    pub panicked: usize,
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError};
    use std::time::{Duration, Instant};

    struct State {
        active: usize,
        closing: bool,
        draining: usize,
        cancel: Arc<AtomicBool>,
        panicked: usize,
    }

    pub(super) struct Captures {
        state: Mutex<State>,
        serial: Mutex<()>,
        idle: Condvar,
    }

    impl Default for Captures {
        fn default() -> Self {
            Self {
                state: Mutex::new(State { active: 0, closing: false, draining: 0, cancel: Arc::new(AtomicBool::new(false)), panicked: 0 }),
                serial: Mutex::new(()),
                idle: Condvar::new(),
            }
        }
    }

    struct Active<'a>(&'a Captures);
    impl Drop for Active<'_> {
        fn drop(&mut self) {
            let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.active = state.active.saturating_sub(1);
            if std::thread::panicking() {
                state.panicked = state.panicked.saturating_add(1);
            }
            if state.active == 0 {
                // New owners may capture again after the preceding drain has actually ended.
                if state.draining == 0 {
                    state.cancel = Arc::new(AtomicBool::new(false));
                    state.closing = false;
                }
                self.0.idle.notify_all();
            }
        }
    }

    impl Captures {
        pub(super) fn with<S, T>(
            &self,
            cancelled: &dyn Fn() -> bool,
            work: impl FnOnce(&mut Option<S>, &dyn Fn() -> bool) -> Result<T, String>,
        ) -> Result<T, String> {
            if cancelled() {
                return Err("the 3D capture was cancelled".into());
            }
            let cancel = {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                if state.closing {
                    return Err("the 3D capture was cancelled".into());
                }
                state.active = state.active.checked_add(1).ok_or("too many 3D captures are waiting")?;
                Arc::clone(&state.cancel)
            };
            let _active = Active(self);
            let _serial = self.serial.lock().unwrap_or_else(PoisonError::into_inner);
            let stopping = || cancelled() || cancel.load(Ordering::Acquire);
            if stopping() {
                return Err("the 3D capture was cancelled".into());
            }
            // Declaration order is deliberate: even unwinding drops the device before the
            // serialization lock and active guard. No static owns a device at process exit.
            let mut slot = None;
            work(&mut slot, &stopping)
        }

        pub(super) fn shutdown(&self, wait: Duration) -> Shutdown {
            let deadline = Instant::now() + wait.min(CLOSE_WAIT);
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.closing = true;
            state.draining = state.draining.saturating_add(1);
            state.cancel.store(true, Ordering::Release);
            while state.active > 0 {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let (next, _) = self.idle.wait_timeout(state, remaining).unwrap_or_else(PoisonError::into_inner);
                state = next;
            }
            let report = Shutdown { remaining: state.active, panicked: std::mem::take(&mut state.panicked) };
            state.draining = state.draining.saturating_sub(1);
            if state.active == 0 && state.draining == 0 {
                state.cancel = Arc::new(AtomicBool::new(false));
                state.closing = false;
            }
            report
        }
    }

    pub(super) static CAPTURES: LazyLock<Captures> = LazyLock::new(Captures::default);

    #[cfg(test)]
    mod tests;
}

#[cfg(not(target_arch = "wasm32"))]
pub(super) fn with<T>(cancelled: &dyn Fn() -> bool, work: impl FnOnce(&mut Slot, &dyn Fn() -> bool) -> Result<T, String>) -> Result<T, String> {
    native::CAPTURES.with(cancelled, work)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn shutdown(wait: std::time::Duration) -> Shutdown {
    native::CAPTURES.shutdown(wait)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn with<T>(cancelled: &dyn Fn() -> bool, work: impl FnOnce(&mut Slot, &dyn Fn() -> bool) -> Result<T, String>) -> Result<T, String> {
    if cancelled() {
        return Err("the 3D capture was cancelled".into());
    }
    work(&mut None, cancelled)
}
