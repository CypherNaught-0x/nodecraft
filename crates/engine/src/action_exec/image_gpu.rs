//! Built-in image actions on the GPU (#1058). The desktop app turns this on at start
//! ([`enable`]); every other host, and the browser, keeps the CPU reference. One shared
//! executor serves every action in turn, made on first use. When there is no compatible GPU, the
//! device fails, or the GPU refuses a request, the action runs on the CPU instead: the GPU is an
//! accelerator, never the only way to get a result. Outputs match the CPU reference within the
//! tolerance `nodecraft_gpu::image` tests (at most 2 in any 8-bit channel of straight colour,
//! compared premultiplied where alpha is nearly zero).
use nodecraft_actions::image::{ImageInputs, ImageLimits, ImageRequest, ImageResult};
use nodecraft_gpu::image::{GpuExecutor, GpuImageError, GpuOptions};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

/// Whether built-in image actions try the GPU first.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// The one executor, or why there is none.
static SLOT: Mutex<Slot> = Mutex::new(Slot::Untried);

/// How long one action may take on the GPU before it is given up and run on the CPU.
const GPU_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an action waits for the GPU to finish one given up before it, before taking the CPU.
const DRAIN_FOR: Duration = Duration::from_secs(2);
/// How long a host without a compatible GPU waits before looking for one again.
const RETRY_AFTER: Duration = Duration::from_secs(60);

enum Slot {
    Untried,
    Ready(Box<GpuExecutor>),
    /// No compatible GPU was found, or the last one failed, at this time.
    Unavailable(Instant),
}

/// Turns the GPU path on (the desktop app) or off. `NODECRAFT_IMAGE_GPU=0` keeps it off.
pub fn enable(on: bool) {
    let refused = std::env::var("NODECRAFT_IMAGE_GPU").is_ok_and(|value| value == "0");
    ENABLED.store(on && !refused, Ordering::Release);
}

/// Whether built-in image actions try the GPU first.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}

/// Which GPU the actions use, once one has been found.
pub fn adapter() -> Option<String> {
    match &*SLOT.lock().unwrap_or_else(PoisonError::into_inner) {
        Slot::Ready(gpu) => Some(gpu.adapter_info().name.clone()),
        _ => None,
    }
}

/// Runs `request` on the GPU when the path is on and a GPU is there; `None` means run it on the
/// CPU. `cancelled` is checked while it waits for the GPU; a cancelled action leaves its
/// submission to the executor, which reclaims it before the next.
pub(super) fn execute(request: &ImageRequest, inputs: ImageInputs<'_>, limits: ImageLimits, cancelled: &dyn Fn() -> bool) -> Option<ImageResult> {
    if !enabled() {
        return None;
    }
    execute_with(request, inputs, limits, cancelled, GpuOptions::default())
}

fn execute_with(
    request: &ImageRequest,
    inputs: ImageInputs<'_>,
    limits: ImageLimits,
    cancelled: &dyn Fn() -> bool,
    options: GpuOptions,
) -> Option<ImageResult> {
    // Another action holds the GPU: this one goes to the CPU rather than wait.
    let mut slot = SLOT
        .try_lock()
        .or_else(|error| match error {
            std::sync::TryLockError::Poisoned(poisoned) => Ok(poisoned.into_inner()),
            std::sync::TryLockError::WouldBlock => Err(()),
        })
        .ok()?;
    if matches!(&*slot, Slot::Unavailable(at) if at.elapsed() < RETRY_AFTER) {
        return None;
    }
    if !matches!(&*slot, Slot::Ready(_)) {
        *slot = match settle(GpuExecutor::request_with_options(options)) {
            Ok(gpu) => Slot::Ready(Box::new(gpu)),
            Err(_) => Slot::Unavailable(Instant::now()),
        };
    }
    let Slot::Ready(gpu) = &mut *slot else { return None };
    let handle = gpu.poll_handle();
    let mut outcome = drive(&handle, gpu.execute(request, inputs, limits), cancelled);
    // An action given up earlier (cancelled, or out of time) leaves its submission in flight, and
    // only polling the device finishes it: poll it and try again, for up to [`DRAIN_FOR`], so one
    // cancelled action doesn't keep every later one off the GPU.
    let draining = Instant::now();
    while matches!(outcome, Err(Some(GpuImageError::Busy))) && draining.elapsed() < DRAIN_FOR && finish_pending(&handle, cancelled) {
        outcome = drive(&handle, gpu.execute(request, inputs, limits), cancelled);
    }
    match outcome {
        Ok(result) => Some(result),
        // The CPU says precisely what is wrong with the request itself.
        Err(Some(GpuImageError::Action(_) | GpuImageError::Resource | GpuImageError::Unsupported)) => None,
        // Busy: an earlier submission is still in flight; the CPU takes this one.
        Err(Some(GpuImageError::Busy) | None) => None,
        // A device that failed may not recover: an action a minute later looks for a fresh one.
        Err(Some(GpuImageError::Device | GpuImageError::Mapping | GpuImageError::Unavailable)) => {
            *slot = Slot::Unavailable(Instant::now());
            None
        }
    }
}

/// Polls the device for a moment, so what an earlier action submitted can finish; whether it was
/// given the time (not cancelled meanwhile).
fn finish_pending(handle: &nodecraft_gpu::image::GpuPollHandle, cancelled: &dyn Fn() -> bool) -> bool {
    let started = Instant::now();
    // The submission's map callback runs as the device is polled; a few polls usually suffice.
    while started.elapsed() < Duration::from_millis(50) {
        if handle.poll_nonblocking().is_err() || cancelled() {
            return false;
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    !cancelled()
}

/// Polls `future` to its end, for at most [`GPU_TIMEOUT`]: native wgpu's adapter and device
/// requests are ready at once.
fn settle<T>(future: impl Future<Output = Result<T, GpuImageError>>) -> Result<T, GpuImageError> {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let started = Instant::now();
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result;
        }
        if started.elapsed() > GPU_TIMEOUT {
            return Err(GpuImageError::Unavailable);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Polls `future` to its end, driving the device between polls; `Err(None)` when cancelled or
/// out of time.
fn drive<T>(
    handle: &nodecraft_gpu::image::GpuPollHandle,
    future: impl Future<Output = Result<T, GpuImageError>>,
    cancelled: &dyn Fn() -> bool,
) -> Result<T, Option<GpuImageError>> {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let started = Instant::now();
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result.map_err(Some);
        }
        handle.poll_nonblocking().map_err(Some)?;
        if cancelled() || started.elapsed() > GPU_TIMEOUT {
            return Err(None);
        }
        std::thread::sleep(Duration::from_micros(200));
    }
}

#[cfg(test)]
mod tests;
