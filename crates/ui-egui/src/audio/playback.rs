//! One device/decoder worker per sound host. No progress depends on egui frames.
use super::output::{Output, us};
use crate::video::Retry;
use nodecraft_media::video::{AudioAsset, MAX_AUDIO_DURATION_US, PcmChunk, VideoError, VideoJob, VideoService};
use std::sync::{
    Arc, Condvar, Mutex, MutexGuard, PoisonError,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const CHUNK_US: u64 = 2_000_000;
const LOW_US: u64 = 3_000_000;
const POLL: Duration = Duration::from_millis(20);
const SEEK_SETTLE: Duration = Duration::from_millis(75);
const LOST_DRAG: Duration = Duration::from_secs(2);
const STALL: Duration = Duration::from_secs(3);
const DEVICE_STOPPED: &str = "The sound device stopped; press play to try again";
const DEVICE_UNRESPONSIVE: &str = "The sound device isn't responding; press play to try again";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

trait Sink {
    fn push(&mut self, samples: &[i16]);
    fn played(&self) -> u64;
    fn buffered(&self) -> u64;
    fn reset(&mut self);
    fn failed(&self) -> bool;
    fn callbacks(&self) -> u64;
}
impl Sink for Output {
    fn push(&mut self, samples: &[i16]) {
        Output::push(self, samples)
    }
    fn played(&self) -> u64 {
        Output::played(self)
    }
    fn buffered(&self) -> u64 {
        Output::buffered(self)
    }
    fn reset(&mut self) {
        Output::reset(self)
    }
    fn failed(&self) -> bool {
        Output::failed(self)
    }
    fn callbacks(&self) -> u64 {
        Output::callbacks(self)
    }
}

trait ChunkJob: Send {
    fn poll(&mut self, sink: &mut dyn Sink) -> Result<Option<u64>, VideoError>;
}
impl ChunkJob for VideoJob<PcmChunk> {
    fn poll(&mut self, sink: &mut dyn Sink) -> Result<Option<u64>, VideoError> {
        self.try_result().map(|chunk| {
            chunk.map(|chunk| {
                sink.push(chunk.samples());
                u64::try_from(chunk.frames()).unwrap_or(u64::MAX)
            })
        })
    }
}
trait Decoder: Send + Sync {
    fn request(&self, start_us: u64, length_us: u64) -> Result<Box<dyn ChunkJob>, VideoError>;
}
struct Source {
    service: Arc<VideoService>,
    asset: AudioAsset,
}
impl Decoder for Source {
    fn request(&self, start_us: u64, length_us: u64) -> Result<Box<dyn ChunkJob>, VideoError> {
        self.service.pcm(&self.asset, start_us, length_us).map(|job| Box::new(job) as Box<dyn ChunkJob>)
    }
}

#[derive(Clone)]
struct Request {
    decoder: Arc<dyn Decoder>,
    duration_us: u64,
    from_us: u64,
    /// A seek reuses the current device; a new play always closes it first.
    play: u64,
    settle_until: Option<Instant>,
    held: Option<Instant>,
    admitted_at: Instant,
}
#[derive(Default)]
struct Desired {
    epoch: u64,
    request: Option<Request>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Status {
    pub position_us: u64,
    pub duration_us: u64,
    pub playing: bool,
    pub notice: Option<&'static str>,
}
struct Report {
    epoch: u64,
    status: Status,
}
#[derive(Default)]
struct Shared {
    desired: Mutex<Desired>,
    report: Mutex<Option<Report>>,
    wake: Condvar,
    quit: AtomicBool,
    dead: AtomicBool,
    #[cfg(test)]
    turns: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    idle: AtomicBool,
}

type Open = Box<dyn FnMut() -> Result<Box<dyn Sink>, &'static str> + Send>;

/// The handle never owns or opens a device. Its mailbox retains only the newest request.
pub(super) struct Feeder {
    shared: Arc<Shared>,
}
impl Feeder {
    pub fn new() -> Result<Self, &'static str> {
        Self::spawn(Box::new(|| Output::open().map(|device| Box::new(device) as Box<dyn Sink>)))
    }

    /// Replace only a worker that has finished unwinding, never one blocked in a native call.
    pub fn ready(slot: &mut Option<Self>) -> Result<&Self, &'static str> {
        Self::ready_with(slot, Self::new)
    }

    fn ready_with(slot: &mut Option<Self>, create: impl FnOnce() -> Result<Self, &'static str>) -> Result<&Self, &'static str> {
        if slot.as_ref().is_none_or(|feeder| feeder.shared.dead.load(Ordering::Acquire)) {
            *slot = Some(create()?);
        }
        slot.as_ref().ok_or("Can't start sound playback")
    }

    fn spawn(mut open: Open) -> Result<Self, &'static str> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("nodecraft-sound".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&worker, &mut open)));
                if result.is_err() {
                    let desired = lock(&worker.desired);
                    if let Some(request) = &desired.request {
                        *lock(&worker.report) = Some(Report {
                            epoch: desired.epoch,
                            status: Status {
                                position_us: request.from_us,
                                duration_us: request.duration_us,
                                playing: false,
                                notice: Some("Sound playback stopped unexpectedly; press play to try again"),
                            },
                        });
                    }
                }
                worker.dead.store(true, Ordering::Release);
            })
            .map_err(|_| "Can't start sound playback")?;
        Ok(Self { shared })
    }

    pub fn play(&self, service: Arc<VideoService>, asset: AudioAsset, from_us: u64, duration_us: u64) -> Result<(), &'static str> {
        self.request(Arc::new(Source { service, asset }), duration_us, from_us)
    }

    fn request(&self, decoder: Arc<dyn Decoder>, duration_us: u64, from_us: u64) -> Result<(), &'static str> {
        if self.shared.dead.load(Ordering::Acquire) {
            return Err("The sound playback worker stopped; reopen the app");
        }
        let mut desired = lock(&self.shared.desired);
        desired.epoch = desired.epoch.checked_add(1).ok_or("Too many sound requests; reopen the app")?;
        desired.request = Some(Request { decoder, duration_us, from_us, play: desired.epoch, settle_until: None, held: None, admitted_at: Instant::now() });
        drop(desired);
        self.shared.wake.notify_one();
        Ok(())
    }

    pub fn seek(&self, from_us: u64) {
        self.move_to(from_us, false);
    }

    pub fn scrub(&self, from_us: u64) {
        self.move_to(from_us, true);
    }

    pub fn finish_seek(&self) {
        let from = lock(&self.shared.desired).request.as_ref().filter(|request| request.held.is_some()).map(|request| request.from_us);
        if let Some(from) = from {
            self.seek(from);
        }
    }

    fn move_to(&self, from_us: u64, held: bool) {
        let mut desired = lock(&self.shared.desired);
        let Some(epoch) = desired.epoch.checked_add(1) else { return };
        let Some(request) = &mut desired.request else { return };
        request.from_us = from_us;
        request.settle_until = Instant::now().checked_add(SEEK_SETTLE);
        request.held = held.then(|| Instant::now().checked_add(LOST_DRAG)).flatten();
        request.admitted_at = Instant::now();
        desired.epoch = epoch;
        drop(desired);
        self.shared.wake.notify_one();
    }

    pub fn stop(&self) {
        lock(&self.shared.desired).request = None;
        self.shared.wake.notify_one();
    }

    pub fn status(&self) -> Option<Status> {
        let desired = lock(&self.shared.desired);
        let request = desired.request.as_ref()?;
        let report = lock(&self.shared.report);
        if let Some(report) = report.as_ref().filter(|report| report.epoch == desired.epoch) {
            return Some(report.status);
        }
        let stalled = request.admitted_at.elapsed() >= STALL;
        Some(Status { position_us: request.from_us, duration_us: request.duration_us, playing: !stalled, notice: stalled.then_some(DEVICE_UNRESPONSIVE) })
    }
}
impl Drop for Feeder {
    fn drop(&mut self) {
        self.stop();
        self.shared.quit.store(true, Ordering::Release);
        self.shared.wake.notify_one();
        // Native device opening/closing may block. Never join it from an app frame.
    }
}

struct Active {
    play: u64,
    epoch: u64,
    request: Request,
    output: Box<dyn Sink>,
    job: Option<Box<dyn ChunkJob>>,
    next_us: u64,
    length_us: u64,
    ended: bool,
    retry: Option<Retry>,
    callbacks: u64,
    callback_at: Instant,
}
impl Drop for Active {
    fn drop(&mut self) {
        // Cancel subprocess work before a native device close is allowed to block.
        self.job = None;
    }
}
impl Active {
    fn status(&self, playing: bool, notice: Option<&'static str>) -> Status {
        Status {
            position_us: self.request.from_us.saturating_add(us(self.output.played())).min(self.next_us),
            duration_us: if self.ended { self.next_us } else { self.request.duration_us.max(self.next_us) },
            playing,
            notice,
        }
    }

    fn poll(&mut self, now: Instant, elapsed: f64) -> Status {
        let callbacks = self.output.callbacks();
        if callbacks != self.callbacks {
            self.callbacks = callbacks;
            self.callback_at = now;
        }
        if self.output.failed() || now.saturating_duration_since(self.callback_at) >= STALL {
            return self.status(false, Some(DEVICE_STOPPED));
        }
        if let Some(job) = &mut self.job {
            match job.poll(self.output.as_mut()) {
                Ok(Some(frames)) => {
                    self.job = None;
                    self.retry = None;
                    self.next_us = self.next_us.saturating_add(us(frames)).min(MAX_AUDIO_DURATION_US);
                    // Ignore a few frames of resampling rounding, but require a nonempty chunk.
                    self.ended = frames == 0 || us(frames.saturating_add(64)) < self.length_us || self.next_us >= MAX_AUDIO_DURATION_US;
                }
                Ok(None) => {}
                Err(error) => {
                    self.job = None;
                    self.retry = Retry::after(self.retry, error, elapsed);
                    if self.retry.is_none() {
                        return self.status(false, Some(super::native::message(error)));
                    }
                }
            }
        }
        if self.ended && self.output.buffered() == 0 {
            let mut status = self.status(false, None);
            status.position_us = self.next_us;
            return status;
        }
        if self.job.is_none() && !self.ended && us(self.output.buffered()) < LOW_US && self.retry.is_none_or(|retry| retry.due(elapsed)) {
            self.length_us = CHUNK_US.min(MAX_AUDIO_DURATION_US.saturating_sub(self.next_us));
            if self.length_us == 0 {
                self.ended = true;
            } else {
                match self.request.decoder.request(self.next_us, self.length_us) {
                    Ok(job) => self.job = Some(job),
                    Err(VideoError::Busy) => {}
                    Err(error) => {
                        self.retry = Retry::after(self.retry, error, elapsed);
                        if self.retry.is_none() {
                            return self.status(false, Some(super::native::message(error)));
                        }
                    }
                }
            }
        }
        self.status(true, None)
    }
}

fn run(shared: &Shared, open: &mut Open) {
    let started = Instant::now();
    let mut active: Option<Active> = None;
    let mut finished = None;
    loop {
        #[cfg(test)]
        shared.turns.fetch_add(1, Ordering::Relaxed);
        if shared.quit.load(Ordering::Acquire) {
            return;
        }
        let (epoch, request) = {
            let desired = lock(&shared.desired);
            (desired.epoch, desired.request.clone())
        };
        let Some(mut request) = request else {
            active = None;
            wait_idle(shared, epoch, false);
            continue;
        };
        if finished == Some(epoch) {
            wait_idle(shared, epoch, true);
            continue;
        }
        let now = Instant::now();
        if active.as_ref().is_some_and(|current| current.play != request.play) {
            active = None;
        }
        if let Some(current) = &mut active
            && current.epoch != epoch
        {
            current.job = None;
            current.output.reset();
            current.epoch = epoch;
            current.request = request.clone();
            current.next_us = request.from_us;
            current.ended = false;
            current.retry = None;
        }
        if request.held.is_some_and(|due| now >= due) {
            // A stationary held pointer or missing app frames must not suspend sound forever.
            let mut desired = lock(&shared.desired);
            if desired.epoch != epoch {
                continue;
            }
            if let Some(latest) = &mut desired.request {
                latest.held = None;
                latest.settle_until = None;
            }
            request.held = None;
            request.settle_until = None;
        }
        if request.held.is_some() || request.settle_until.is_some_and(|due| now < due) {
            wait(shared, epoch);
            continue;
        }
        if active.is_none() {
            match open() {
                Ok(output) => {
                    // A cancelled/opening request never starts decoding when the OS returns.
                    if shared.quit.load(Ordering::Acquire) {
                        return;
                    }
                    let desired = lock(&shared.desired);
                    if desired.request.is_none() || desired.epoch != epoch {
                        drop(desired);
                        drop(output);
                        continue;
                    }
                    drop(desired);
                    let callbacks = output.callbacks();
                    active = Some(Active {
                        play: request.play,
                        epoch,
                        next_us: request.from_us,
                        request,
                        output,
                        job: None,
                        length_us: CHUNK_US,
                        ended: false,
                        retry: None,
                        callbacks,
                        callback_at: Instant::now(),
                    });
                }
                Err(notice) => {
                    *lock(&shared.report) = Some(Report {
                        epoch,
                        status: Status { position_us: request.from_us, duration_us: request.duration_us, playing: false, notice: Some(notice) },
                    });
                    finished = Some(epoch);
                    continue;
                }
            }
        }
        if let Some(current) = &mut active {
            let status = current.poll(Instant::now(), started.elapsed().as_secs_f64());
            let mut desired = lock(&shared.desired);
            if desired.epoch == epoch
                && let Some(request) = &mut desired.request
            {
                request.duration_us = status.duration_us;
                *lock(&shared.report) = Some(Report { epoch, status });
            }
            drop(desired);
            if !status.playing {
                active = None;
                finished = Some(epoch);
            }
        }
        wait(shared, epoch);
    }
}
fn wait(shared: &Shared, epoch: u64) {
    let desired = lock(&shared.desired);
    if shared.quit.load(Ordering::Acquire) || desired.epoch != epoch || desired.request.is_none() {
        return;
    }
    drop(shared.wake.wait_timeout(desired, POLL).unwrap_or_else(PoisonError::into_inner));
}

fn wait_idle(shared: &Shared, epoch: u64, had_request: bool) {
    let mut desired = lock(&shared.desired);
    // Check under the same guard used by request/stop, so a notification before this wait
    // cannot be lost. Idle and terminal epochs do not need wall-clock polling.
    while !shared.quit.load(Ordering::Acquire) && desired.epoch == epoch && desired.request.is_some() == had_request {
        #[cfg(test)]
        shared.idle.store(true, Ordering::Release);
        desired = shared.wake.wait(desired).unwrap_or_else(PoisonError::into_inner);
        #[cfg(test)]
        shared.idle.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests;
