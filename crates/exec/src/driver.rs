//! Runs a [`Scheduler`] on whatever async runtime the app provides.
//!
//! The runtime is abstracted ([`Runtime`]: spawn, sleep, a monotonic clock) so `exec` works with
//! tokio on desktop and `wasm-bindgen-futures` in the browser; the apps implement it. Jobs are run
//! by a [`JobRunner`] (providers for network jobs, the action pool for local ones), which gets a
//! [`CancelToken`] to stop cooperatively. Commands (add jobs, cancel) arrive over a [`Handle`].

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use futures::channel::{mpsc, oneshot};
use futures::{FutureExt, StreamExt};

use crate::scheduler::{CancelScope, Event, JobId, JobSpec, Outcome, Scheduler, SchedulerError, Start};

/// A boxed future the runtime can spawn: `Send` on native targets, not in the browser (where
/// futures such as `reqwest`'s aren't `Send` and everything runs on one thread).
#[cfg(not(target_arch = "wasm32"))]
pub type TaskFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
#[cfg(target_arch = "wasm32")]
pub type TaskFuture<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// `Send + Sync` on native targets, nothing in the browser.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSendSync for T {}

/// The async runtime, implemented by the apps (tokio on desktop, wasm-bindgen-futures on web).
pub trait Runtime: MaybeSendSync {
    fn spawn(&self, task: TaskFuture<()>);
    /// Resolves after `ms` milliseconds.
    fn sleep(&self, ms: u64) -> TaskFuture<()>;
    /// Milliseconds on a monotonic clock (not wall time; any origin).
    fn now_ms(&self) -> u64;
}

/// Runs one attempt of a job. It should stop soon after `cancel` fires and report
/// [`Outcome::Cancelled`], and classify errors as transient (worth retrying) or not.
pub trait JobRunner: MaybeSendSync {
    fn run(&self, start: Start, cancel: CancelToken) -> TaskFuture<Outcome>;
}

/// Cooperative cancellation for one running job.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<CancelInner>);

#[derive(Default)]
struct CancelInner {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<Waker>>,
}

impl std::fmt::Debug for CancelToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CancelToken").field(&self.is_cancelled()).finish()
    }
}

impl CancelToken {
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        let wakers = std::mem::take(&mut *self.0.wakers.lock().unwrap_or_else(PoisonError::into_inner));
        for w in wakers {
            w.wake();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Resolves once the job is cancelled; race it against the work.
    pub fn cancelled(&self) -> Cancelled {
        Cancelled(self.clone())
    }
}

/// Future returned by [`CancelToken::cancelled`].
#[derive(Debug)]
pub struct Cancelled(CancelToken);

impl Future for Cancelled {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.is_cancelled() {
            return Poll::Ready(());
        }
        {
            let mut wakers = self.0.0.wakers.lock().unwrap_or_else(PoisonError::into_inner);
            if !wakers.iter().any(|w| w.will_wake(cx.waker())) {
                wakers.push(cx.waker().clone());
            }
        }
        // Re-check: `cancel` may have run between the first check and registering.
        if self.0.is_cancelled() { Poll::Ready(()) } else { Poll::Pending }
    }
}

mod admission;
use admission::{Admission, Permit};
pub use admission::{AdmissionLimits, DriverError};

struct Envelope {
    command: Command,
    permit: Permit,
}

enum Command {
    Add(Vec<JobSpec>, oneshot::Sender<Result<(), SchedulerError>>),
    Requeue(Vec<(JobId, String)>, oneshot::Sender<Result<(), SchedulerError>>),
    Forget,
    Cancel(CancelScope),
    Pause(bool),
}

/// Sends a job's outcome exactly once. If the task is dropped before it finishes (runtime
/// shutdown, a dropped browser task), the drop reports a failure so the job never hangs as
/// running.
struct Reporter {
    job: JobId,
    tx: Option<mpsc::UnboundedSender<(JobId, Outcome)>>,
}

impl Reporter {
    fn send(&mut self, outcome: Outcome) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.unbounded_send((self.job, outcome));
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.send(Outcome::Failed { error: "the job was stopped before it finished".into(), transient: false });
    }
}

/// Sends commands to a running driver. All clones share bounded admission. After every handle
/// is dropped, an unpaused queue drains normally; a paused queue cancels unfinished work.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Envelope>,
    admission: Arc<Admission>,
}

impl Handle {
    fn send(&self, command: Command) -> Result<(), DriverError> {
        if self.tx.is_closed() {
            return Err(DriverError::Closed);
        }
        let permit = self.admission.reserve(&command)?;
        // Never send or drop a payload while the admission lock is held: waking an executor
        // can synchronously run user code. Failed sends drop the envelope and its permit.
        self.tx.unbounded_send(Envelope { command, permit }).map_err(|_| DriverError::Closed)
    }

    /// Enqueues immediately; the returned future waits only for the scheduler verdict.
    /// Dropping or never polling that future does not cancel accepted jobs. It holds no sender
    /// and does not extend last-handle shutdown. Rejection drops the payload before returning.
    #[must_use = "construction enqueues work; await the future to observe the scheduler verdict"]
    pub fn add(&self, jobs: Vec<JobSpec>) -> impl Future<Output = Result<Result<(), SchedulerError>, DriverError>> + Send + 'static + use<> {
        let (tx, rx) = oneshot::channel();
        let accepted = self.send(Command::Add(jobs, tx));
        async move {
            accepted?;
            rx.await.map_err(|_| DriverError::Closed)
        }
    }

    pub fn cancel(&self, scope: CancelScope) -> Result<(), DriverError> {
        self.send(Command::Cancel(scope))
    }

    /// Retries under new idempotency keys (see [`Scheduler::requeue`]). Like [`Handle::add`],
    /// enqueues at construction, not first poll; abandoning the verdict does not retract it.
    #[must_use = "construction enqueues retries; await the future to observe the scheduler verdict"]
    pub fn requeue(&self, jobs: Vec<(JobId, String)>) -> impl Future<Output = Result<Result<(), SchedulerError>, DriverError>> + Send + 'static + use<> {
        let (tx, rx) = oneshot::channel();
        let accepted = self.send(Command::Requeue(jobs, tx));
        async move {
            accepted?;
            rx.await.map_err(|_| DriverError::Closed)
        }
    }

    /// Drops finished jobs. Saturation is explicit; retry only commands rejected by admission.
    pub fn forget_finished(&self) -> Result<(), DriverError> {
        self.send(Command::Forget)
    }

    /// Stops/resumes starting jobs; running jobs carry on. Uses reserved control capacity.
    pub fn set_paused(&self, paused: bool) -> Result<(), DriverError> {
        self.send(Command::Pause(paused))
    }
}

/// A driver ready to run: spawn [`Driver::run`] on the runtime and keep the [`Handle`].
pub struct Driver {
    scheduler: Scheduler,
    commands: mpsc::UnboundedReceiver<Envelope>,
}

/// Creates a driver around `scheduler` and the handle that controls it.
pub fn driver(scheduler: Scheduler) -> (Driver, Handle) {
    make_driver(scheduler, AdmissionLimits::default())
}

/// Creates a driver with lower pending-command limits. Invalid ceilings return `PayloadLimit`.
pub fn driver_with_admission(scheduler: Scheduler, limits: AdmissionLimits) -> Result<(Driver, Handle), DriverError> {
    if !limits.valid() {
        return Err(DriverError::PayloadLimit);
    }
    Ok(make_driver(scheduler, limits))
}

fn make_driver(scheduler: Scheduler, limits: AdmissionLimits) -> (Driver, Handle) {
    let (tx, rx) = mpsc::unbounded();
    (Driver { scheduler, commands: rx }, Handle { tx, admission: Admission::new(limits) })
}

impl Driver {
    /// Runs until every handle is dropped and no job is left, calling `on_event` for each
    /// scheduler event. Returns the scheduler for inspection.
    ///
    /// Closing the command stream drains an unpaused queue normally. If paused at closure,
    /// queued and retry-waiting work is cancelled without starting; running jobs are asked to
    /// cancel cooperatively and awaited. As with explicit cancellation, a runner must respond
    /// for shutdown to finish, and work that finishes successfully still counts as done.
    pub async fn run(self, runner: Arc<dyn JobRunner>, runtime: Arc<dyn Runtime>, mut on_event: impl FnMut(Event)) -> Scheduler {
        let Driver { mut scheduler, mut commands } = self;
        let (done_tx, mut done_rx) = mpsc::unbounded::<(JobId, Outcome)>();
        let mut tokens: HashMap<JobId, CancelToken> = HashMap::new();
        let mut open = true;
        loop {
            for start in scheduler.start_ready(runtime.now_ms()) {
                let token = CancelToken::default();
                tokens.insert(start.job, token.clone());
                let mut reporter = Reporter { job: start.job, tx: Some(done_tx.clone()) };
                let runner = Arc::clone(&runner);
                runtime.spawn(Box::pin(async move {
                    // A panicking runner (while building its future or while running it) must not
                    // stall the queue: report it as a failure.
                    let crashed = || Outcome::Failed { error: "the job crashed (internal error)".into(), transient: false };
                    let outcome = match std::panic::catch_unwind(AssertUnwindSafe(|| runner.run(start, token))) {
                        Ok(work) => AssertUnwindSafe(work).catch_unwind().await.unwrap_or_else(|_| crashed()),
                        Err(_) => crashed(),
                    };
                    reporter.send(outcome);
                }));
            }
            for event in scheduler.drain_events() {
                // A panicking listener loses its event, not the queue.
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| on_event(event)));
            }
            if !open && scheduler.is_idle() {
                return scheduler;
            }
            let mut wake: TaskFuture<()> = match scheduler.next_wakeup_ms() {
                Some(at) => runtime.sleep(at.saturating_sub(runtime.now_ms())),
                None => Box::pin(futures::future::pending()),
            };
            // Once closed, the command stream cannot make progress or resume paused work.
            let command = async { if open { commands.next().await } else { futures::future::pending().await } };
            futures::pin_mut!(command);
            futures::select! {
                done = done_rx.next() => {
                    if let Some((job, outcome)) = done {
                        tokens.remove(&job);
                        // `NotRunning` can only follow a scheduler bug; the job is ignored either way.
                        let _ = scheduler.report(job, outcome, runtime.now_ms());
                    }
                }
                command = command.fuse() => match command {
                    Some(Envelope { command, permit }) => {
                        match command {
                            Command::Add(jobs, reply) => {
                                let _ = reply.send(scheduler.add(jobs));
                            }
                            Command::Requeue(jobs, reply) => {
                                let _ = reply.send(scheduler.requeue(&jobs));
                            }
                            Command::Pause(paused) => scheduler.set_paused(paused),
                            Command::Forget => scheduler.forget_finished(),
                            Command::Cancel(scope) => {
                                for job in scheduler.cancel(scope) {
                                    if let Some(token) = tokens.get(&job) {
                                        token.cancel();
                                    }
                                }
                            }
                        }
                        drop(permit);
                    }
                    None => {
                        open = false;
                        if scheduler.is_paused() {
                            for job in scheduler.cancel(CancelScope::All) {
                                if let Some(token) = tokens.get(&job) {
                                    token.cancel();
                                }
                            }
                        }
                    }
                },
                () = (&mut wake).fuse() => {}
            }
        }
    }
}

// The tests use a thread pool, which needs `Send` futures: native only.
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::scheduler::{Lane, Limits, RetryPolicy};
    use futures::executor::{ThreadPool, block_on};
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    /// Lets the other futures in a `join!` (the driver) run before continuing.
    async fn yield_now() {
        let mut yielded = false;
        futures::future::poll_fn(|cx| {
            if yielded {
                Poll::Ready(())
            } else {
                yielded = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
        std::thread::yield_now();
    }

    /// Real threads, virtual time: `sleep` advances the clock and returns at once.
    struct TestRuntime {
        pool: ThreadPool,
        clock: AtomicU64,
    }

    impl Runtime for TestRuntime {
        fn spawn(&self, task: TaskFuture<()>) {
            self.pool.spawn_ok(task);
        }
        fn sleep(&self, ms: u64) -> TaskFuture<()> {
            self.clock.fetch_add(ms, Ordering::SeqCst);
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            self.clock.load(Ordering::SeqCst)
        }
    }

    fn runtime() -> Arc<TestRuntime> {
        Arc::new(TestRuntime { pool: ThreadPool::new().unwrap(), clock: AtomicU64::new(0) })
    }

    /// Fails each job transiently `fails[job]` times, records attempts and peak concurrency.
    #[derive(Default)]
    struct Scripted {
        fails: HashMap<u64, u32>,
        attempts: Mutex<Vec<(u64, u32, String)>>,
        running: AtomicUsize,
        peak: AtomicUsize,
        hang: Vec<u64>,
        panic_on: Vec<u64>,
    }

    impl JobRunner for Arc<Scripted> {
        fn run(&self, start: Start, cancel: CancelToken) -> TaskFuture<Outcome> {
            let me = Arc::clone(self);
            Box::pin(async move {
                me.attempts.lock().unwrap().push((start.job.0, start.attempt, start.idempotency_key.clone()));
                let now = me.running.fetch_add(1, Ordering::SeqCst) + 1;
                me.peak.fetch_max(now, Ordering::SeqCst);
                if me.panic_on.contains(&start.job.0) {
                    panic!("runner bug");
                }
                let outcome = if me.hang.contains(&start.job.0) {
                    cancel.cancelled().await;
                    Outcome::Cancelled
                } else if start.attempt <= me.fails.get(&start.job.0).copied().unwrap_or(0) {
                    Outcome::Failed { error: "busy".into(), transient: true }
                } else {
                    Outcome::Done
                };
                me.running.fetch_sub(1, Ordering::SeqCst);
                outcome
            })
        }
    }

    fn spec(id: u64, deps: &[u64]) -> JobSpec {
        JobSpec {
            id: JobId(id),
            deps: deps.iter().map(|d| JobId(*d)).collect(),
            lane: Lane::new("p"),
            retry: RetryPolicy::default(),
            idempotency_key: format!("nodecraft:test:{id}"),
        }
    }

    /// A manually polled runtime bounds even a driver that spins within one poll.
    #[derive(Default)]
    struct BoundedRuntime {
        tasks: Mutex<Vec<TaskFuture<()>>>,
        ticks: AtomicUsize,
    }

    impl Runtime for BoundedRuntime {
        fn spawn(&self, task: TaskFuture<()>) {
            self.tasks.lock().unwrap().push(task);
        }
        fn sleep(&self, _ms: u64) -> TaskFuture<()> {
            Box::pin(futures::future::pending())
        }
        fn now_ms(&self) -> u64 {
            assert!(self.ticks.fetch_add(1, Ordering::SeqCst) < 256, "driver spun within one poll");
            0
        }
    }

    impl BoundedRuntime {
        fn pump(&self, cx: &mut Context<'_>) {
            let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
            for mut task in tasks {
                if task.as_mut().poll(cx).is_pending() {
                    self.tasks.lock().unwrap().push(task);
                }
            }
        }
    }

    #[derive(Default)]
    struct ControlledRunner {
        attempts: Mutex<Vec<JobId>>,
        release: Arc<AtomicBool>,
        wait_for_cancel: bool,
    }

    impl JobRunner for Arc<ControlledRunner> {
        fn run(&self, start: Start, cancel: CancelToken) -> TaskFuture<Outcome> {
            self.attempts.lock().unwrap().push(start.job);
            let release = Arc::clone(&self.release);
            let wait_for_cancel = self.wait_for_cancel;
            Box::pin(async move {
                if start.job == JobId(1) {
                    if wait_for_cancel {
                        cancel.cancelled().await;
                        return Outcome::Cancelled;
                    }
                    futures::future::poll_fn(|_| if release.load(Ordering::SeqCst) { Poll::Ready(()) } else { Poll::Pending }).await;
                }
                Outcome::Done
            })
        }
    }

    fn finish_bounded(task: &mut Pin<Box<impl Future<Output = Scheduler>>>, rt: &BoundedRuntime) -> Scheduler {
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        for _ in 0..64 {
            if let Poll::Ready(scheduler) = task.as_mut().poll(&mut cx) {
                assert!(rt.tasks.lock().unwrap().is_empty(), "no orphaned spawned tasks");
                return scheduler;
            }
            rt.pump(&mut cx);
        }
        panic!("driver did not terminate within 64 polls");
    }

    fn admission_limits() -> AdmissionLimits {
        AdmissionLimits { data_commands: 1, control_commands: 2, ..AdmissionLimits::default() }
    }

    fn add_charge(jobs: Vec<JobSpec>) -> (usize, Vec<JobSpec>) {
        let (tx, _) = oneshot::channel();
        let command = Command::Add(jobs, tx);
        let charge = admission::command_bytes(&command).unwrap();
        let Command::Add(jobs, _) = command else { panic!("add") };
        (charge, jobs)
    }

    #[test]
    fn admission_is_shared_eager_and_controls_keep_fifo_under_data_saturation() {
        let (mut driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), admission_limits()).unwrap();
        let peer = handle.clone();
        let accepted = handle.add(vec![spec(1, &[])]); // never polled
        assert_eq!(block_on(peer.add(vec![spec(2, &[])])), Err(DriverError::Full));
        handle.cancel(CancelScope::All).unwrap();
        peer.set_paused(true).unwrap();
        assert_eq!(handle.forget_finished(), Err(DriverError::Full));
        drop(accepted); // only the acknowledgement is abandoned
        assert_eq!(block_on(peer.add(vec![])), Err(DriverError::Full));
        let Envelope { command, permit } = driver.commands.next().now_or_never().flatten().unwrap();
        assert!(matches!(command, Command::Add(..)));
        drop(command);
        // Dequeue alone keeps the reservation until synchronous consumption has finished.
        assert_eq!(block_on(peer.add(vec![])), Err(DriverError::Full));
        drop(permit);
        let next = peer.add(vec![]);
        let cancel = driver.commands.next().now_or_never().flatten().unwrap();
        assert!(matches!(cancel.command, Command::Cancel(CancelScope::All)));
        drop(cancel);
        let pause = driver.commands.next().now_or_never().flatten().unwrap();
        assert!(matches!(pause.command, Command::Pause(true)));
        drop(pause);
        assert!(matches!(driver.commands.next().now_or_never().flatten().unwrap().command, Command::Add(..)));
        drop(next);
        drop(driver);
        assert_eq!(peer.cancel(CancelScope::All), Err(DriverError::Closed));
    }

    #[test]
    fn actual_capacities_and_exact_byte_limits_are_charged_before_acceptance() {
        let jobs = vec![spec(1, &[])];
        let (charge, jobs) = add_charge(jobs);
        let limits = AdmissionLimits { data_bytes: charge, data_payload_bytes: charge, ..admission_limits() };
        let (mut driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), limits).unwrap();
        let first = handle.add(jobs);
        assert_eq!(block_on(handle.add(vec![])), Err(DriverError::Full));
        drop(driver.commands.next().now_or_never().flatten().unwrap());
        assert_eq!(block_on(first), Err(DriverError::Closed)); // reply dropped, capacity released
        let second = handle.add(vec![spec(1, &[])]);
        drop(driver.commands.next().now_or_never().flatten().unwrap());
        drop(second);
        let mut spare = Vec::with_capacity(64);
        spare.push(spec(2, &[]));
        assert_eq!(block_on(handle.add(spare)), Err(DriverError::PayloadLimit));
        let mut job = spec(3, &[]);
        job.deps.reserve(1024);
        assert_eq!(block_on(handle.add(vec![job])), Err(DriverError::PayloadLimit));
        let mut job = spec(4, &[]);
        job.lane.0.reserve(1024);
        assert_eq!(block_on(handle.add(vec![job])), Err(DriverError::PayloadLimit));
        let mut job = spec(5, &[]);
        job.idempotency_key.reserve(1024);
        let rejected = handle.add(vec![job]); // rejection already releases owned data
        let accepted = handle.add(vec![spec(6, &[])]);
        assert_eq!(block_on(rejected), Err(DriverError::PayloadLimit));
        drop(accepted);
        // A separate aggregate-byte bound, independent of the count bound.
        let limits = AdmissionLimits { data_commands: 3, data_bytes: charge, data_payload_bytes: charge, ..AdmissionLimits::default() };
        let (mut driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), limits).unwrap();
        let first = handle.add(vec![spec(1, &[])]);
        assert_eq!(block_on(handle.add(vec![spec(2, &[])])), Err(DriverError::Full));
        drop(driver.commands.next().now_or_never().flatten().unwrap());
        drop(first);
        let next = handle.add(vec![spec(2, &[])]);
        drop(driver.commands.next().now_or_never().flatten().unwrap());
        drop(next);
    }

    #[test]
    fn requeue_and_cancel_capacities_and_control_bytes_are_bounded() {
        let (tx, _) = oneshot::channel();
        let keys = vec![(JobId(1), String::from("key"))];
        let charge = admission::command_bytes(&Command::Requeue(keys.clone(), tx)).unwrap();
        let limits = AdmissionLimits { data_payload_bytes: charge, ..admission_limits() };
        let (mut driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), limits).unwrap();
        let accepted = handle.requeue(keys);
        drop(driver.commands.next().now_or_never().flatten().unwrap());
        drop(accepted);
        let mut key = String::from("k");
        key.reserve(1024);
        assert_eq!(block_on(handle.requeue(vec![(JobId(1), key)])), Err(DriverError::PayloadLimit));
        let mut keys = Vec::with_capacity(1024);
        keys.push((JobId(1), String::new()));
        assert_eq!(block_on(handle.requeue(keys)), Err(DriverError::PayloadLimit));
        let control = admission::command_bytes(&Command::Cancel(CancelScope::All)).unwrap();
        let limits = AdmissionLimits { control_commands: 8, control_bytes: control, control_payload_bytes: control, ..AdmissionLimits::default() };
        let (mut driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), limits).unwrap();
        let ids = Vec::with_capacity(1);
        assert_eq!(handle.cancel(CancelScope::Jobs(ids)), Err(DriverError::PayloadLimit));
        handle.cancel(CancelScope::All).unwrap();
        assert_eq!(handle.set_paused(true), Err(DriverError::Full));
        drop(driver.commands.next().now_or_never().flatten().unwrap());
        handle.forget_finished().unwrap();
        assert!(driver_with_admission(Scheduler::new(Limits::default()), AdmissionLimits { data_bytes: usize::MAX, ..limits }).is_err());
    }

    #[test]
    fn permanent_resource_policy_errors_are_never_reported_as_full() {
        for data in [true, false] {
            let limits = if data {
                AdmissionLimits { data_commands: 0, ..AdmissionLimits::default() }
            } else {
                AdmissionLimits { control_commands: 0, ..AdmissionLimits::default() }
            };
            let (mut driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), limits).unwrap();
            if data {
                assert_eq!(block_on(handle.add(vec![])), Err(DriverError::PayloadLimit));
                assert_eq!(block_on(handle.requeue(vec![])), Err(DriverError::PayloadLimit));
            } else {
                assert_eq!(handle.cancel(CancelScope::All), Err(DriverError::PayloadLimit));
                assert_eq!(handle.set_paused(true), Err(DriverError::PayloadLimit));
            }
            assert!(driver.commands.next().now_or_never().is_none(), "nothing enqueued");
            let invalid = if data {
                AdmissionLimits { data_bytes: 1, data_payload_bytes: 2, ..AdmissionLimits::default() }
            } else {
                AdmissionLimits { control_bytes: 1, control_payload_bytes: 2, ..AdmissionLimits::default() }
            };
            assert!(matches!(driver_with_admission(Scheduler::new(Limits::default()), invalid), Err(DriverError::PayloadLimit)));
            // Defensive reserve remains permanent even if an internal caller bypasses valid().
            let admission = Admission::new(invalid);
            let command = if data {
                let (tx, _) = oneshot::channel();
                Command::Add(vec![], tx)
            } else {
                Command::Pause(true)
            };
            assert!(matches!(admission.reserve(&command), Err(DriverError::PayloadLimit)));
        }
    }

    #[test]
    fn raw_count_and_capacity_policy_reject_before_enqueue() {
        use crate::scheduler::{MAX_JOBS, StorageLimits};
        let (mut driver, handle) = driver(Scheduler::new(Limits::default()));
        assert_eq!(block_on(handle.add(vec![spec(1, &[]); MAX_JOBS + 1])), Err(DriverError::PayloadLimit));
        assert_eq!(block_on(handle.requeue(vec![(JobId(1), String::new()); MAX_JOBS + 1])), Err(DriverError::PayloadLimit));
        assert_eq!(handle.cancel(CancelScope::Jobs(vec![JobId(1); MAX_JOBS + 1])), Err(DriverError::PayloadLimit));
        let outer = crate::payload::CAPACITY_FACTOR * MAX_JOBS + 1;
        assert_eq!(block_on(handle.add(Vec::with_capacity(outer))), Err(DriverError::PayloadLimit));
        assert_eq!(block_on(handle.requeue(Vec::with_capacity(outer))), Err(DriverError::PayloadLimit));
        assert_eq!(handle.cancel(CancelScope::Jobs(Vec::with_capacity(outer))), Err(DriverError::PayloadLimit));
        let mut hostile = spec(1, &[]);
        hostile.idempotency_key = String::with_capacity(StorageLimits::HARD.key_bytes * crate::payload::CAPACITY_FACTOR + 1);
        assert_eq!(block_on(handle.add(vec![hostile])), Err(DriverError::PayloadLimit));
        let key = String::with_capacity(StorageLimits::HARD.key_bytes * crate::payload::CAPACITY_FACTOR + 1);
        assert_eq!(block_on(handle.requeue(vec![(JobId(1), key)])), Err(DriverError::PayloadLimit));
        assert!(driver.commands.next().now_or_never().is_none());
    }

    #[test]
    fn full_supported_batches_are_one_atomic_add_without_runner_starts() {
        use crate::scheduler::MAX_JOBS;
        for (count, width, pushed) in [(10_000, 8, false), (10_000, 90, true), (10_000, 90, false), (MAX_JOBS, 0, true)] {
            let mut jobs = Vec::new();
            for id in 0..count as u64 {
                let deps = if pushed {
                    let mut deps = Vec::new();
                    for dep in 0..id.min(width) {
                        deps.push(JobId(dep));
                    }
                    deps
                } else {
                    (0..id.min(width)).map(JobId).collect()
                };
                jobs.push(JobSpec { deps, ..spec(id, &[]) });
            }
            let mut scheduler = Scheduler::new(Limits::default());
            scheduler.set_paused(true);
            let (driver, handle) = driver_with_admission(scheduler, admission_limits()).unwrap();
            let accepted = handle.add(jobs);
            // Exactly one accepted command occupies the pool, regardless of batch size.
            assert_eq!(block_on(handle.add(vec![])), Err(DriverError::Full));
            let rt = Arc::new(BoundedRuntime::default());
            let runner = Arc::new(ControlledRunner::default());
            let mut events = Vec::new();
            let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |e| events.push(e)));
            let mut cx = Context::from_waker(futures::task::noop_waker_ref());
            assert!(task.as_mut().poll(&mut cx).is_pending());
            assert_eq!(block_on(accepted), Ok(Ok(())));
            assert!(rt.tasks.lock().unwrap().is_empty());
            assert!(runner.attempts.lock().unwrap().is_empty());
            drop(handle);
            let scheduler = finish_bounded(&mut task, &rt);
            drop(task);
            assert_eq!(scheduler.summary().cancelled, count);
            assert_eq!(events.iter().filter(|e| matches!(e, Event::Queued { .. })).count(), count);
            assert_eq!(events.iter().filter(|e| matches!(e, Event::Cancelled { .. })).count(), count);
            assert!(runner.attempts.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn invalid_last_job_still_rejects_the_whole_batch_through_handle() {
        let mut scheduler = Scheduler::new(Limits::default());
        scheduler.set_paused(true);
        let before = format!("{scheduler:?}");
        let (driver, handle) = driver(scheduler);
        let mut jobs: Vec<JobSpec> = (0..10_000).map(|id| spec(id, &[])).collect();
        jobs.last_mut().unwrap().deps.push(JobId(9_999));
        let verdict = handle.add(jobs);
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner::default());
        let mut events = Vec::new();
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |e| events.push(e)));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert_eq!(block_on(verdict), Ok(Err(SchedulerError::Cycle(JobId(9_999)))));
        drop(handle);
        let scheduler = finish_bounded(&mut task, &rt);
        drop(task);
        assert_eq!(format!("{scheduler:?}"), before);
        assert!(events.is_empty());
        assert!(runner.attempts.lock().unwrap().is_empty());
    }

    #[test]
    fn max_job_cancel_selection_is_reserved_when_data_is_full_and_keeps_scope() {
        use crate::scheduler::{JobState, MAX_JOBS};
        let mut scheduler = Scheduler::new(Limits::default());
        scheduler.set_paused(true);
        let (driver, handle) = driver_with_admission(scheduler, admission_limits()).unwrap();
        let accepted = handle.add((0..MAX_JOBS as u64).map(|id| spec(id, &[])).collect());
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), move |e| sink.lock().unwrap().push(e)));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert_eq!(block_on(accepted), Ok(Ok(())));
        let occupying = handle.add(vec![]);
        assert_eq!(block_on(handle.add(vec![])), Err(DriverError::Full));
        // 100k push-built roots, with a duplicate to leave one real job outside the selection.
        let mut roots = Vec::new();
        for id in 0..MAX_JOBS {
            roots.push(JobId((id % (MAX_JOBS - 1)) as u64));
        }
        handle.cancel(CancelScope::Jobs(roots)).unwrap();
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert_eq!(block_on(occupying), Ok(Ok(())));
        let cancelled = events.lock().unwrap().iter().filter(|e| matches!(e, Event::Cancelled { .. })).count();
        assert_eq!(cancelled, MAX_JOBS - 1, "CancelAll would cancel the unselected final job");
        assert!(!events.lock().unwrap().contains(&Event::Cancelled { job: JobId((MAX_JOBS - 1) as u64) }));
        drop(handle);
        let scheduler = finish_bounded(&mut task, &rt);
        assert_eq!(scheduler.state(JobId((MAX_JOBS - 1) as u64)), Some(&JobState::Cancelled));
        assert!(runner.attempts.lock().unwrap().is_empty());
    }

    #[test]
    fn unpolled_acknowledgements_do_not_hold_senders_or_cancel_accepted_work() {
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let abandoned = handle.add(vec![spec(2, &[])]);
        drop(abandoned);
        let accepted = handle.add(vec![spec(1, &[])]);
        drop(handle);
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner::default());
        runner.release.store(true, Ordering::SeqCst);
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |_| {}));
        let scheduler = finish_bounded(&mut task, &rt);
        assert_eq!(scheduler.summary().done, 2);
        assert_eq!(block_on(accepted), Ok(Ok(())));
        let (driver, handle) = driver_with_admission(Scheduler::new(Limits::default()), admission_limits()).unwrap();
        let admission = Arc::clone(&handle.admission);
        let accepted = handle.add(vec![spec(1, &[])]);
        drop(driver); // drains queued envelopes and releases their permits
        let (tx, _) = oneshot::channel();
        let permit = admission.reserve(&Command::Add(vec![], tx)).unwrap();
        drop(permit);
        assert_eq!(block_on(accepted), Err(DriverError::Closed));
        let rejected = handle.add(vec![spec(2, &[])]);
        drop(handle);
        assert_eq!(block_on(rejected), Err(DriverError::Closed));
    }

    #[test]
    fn closed_paused_queue_cancels_dependencies_without_starting() {
        let mut scheduler = Scheduler::new(Limits::default());
        scheduler.add(vec![spec(1, &[]), spec(2, &[1]), spec(3, &[2])]).unwrap();
        scheduler.set_paused(true);
        let (driver, handle) = driver(scheduler);
        drop(handle);
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner::default());
        let mut events = Vec::new();
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |event| events.push(event)));
        let scheduler = finish_bounded(&mut task, &rt);
        drop(task);
        assert!(runner.attempts.lock().unwrap().is_empty());
        assert!(scheduler.is_idle() && scheduler.is_paused());
        assert_eq!(scheduler.summary().cancelled, 3);
        for id in 1..=3 {
            assert_eq!(events.iter().filter(|event| **event == Event::Cancelled { job: JobId(id) }).count(), 1);
        }
    }

    #[test]
    fn closed_paused_queue_cancels_retry_waiting_without_another_attempt() {
        let mut scheduler = Scheduler::new(Limits::default());
        scheduler.add(vec![spec(1, &[]), spec(2, &[1])]).unwrap();
        scheduler.start_ready(0);
        scheduler.report(JobId(1), Outcome::Failed { error: "busy".into(), transient: true }, 0).unwrap();
        assert!(scheduler.next_wakeup_ms().is_some());
        scheduler.set_paused(true);
        let (driver, handle) = driver(scheduler);
        drop(handle);
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner::default());
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |_| {}));
        let scheduler = finish_bounded(&mut task, &rt);
        assert!(runner.attempts.lock().unwrap().is_empty());
        assert_eq!(scheduler.summary().cancelled, 2);
        assert!(scheduler.is_idle());
    }

    #[test]
    fn closed_paused_queue_waits_for_running_cancellation_and_emits_terminal_events() {
        let mut scheduler = Scheduler::new(Limits { global: 1, ..Limits::default() });
        scheduler.add(vec![spec(1, &[]), spec(2, &[1]), spec(3, &[])]).unwrap();
        let (driver, handle) = driver(scheduler);
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner { wait_for_cancel: true, ..ControlledRunner::default() });
        let mut events = Vec::new();
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |event| events.push(event)));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        assert!(task.as_mut().poll(&mut cx).is_pending());
        rt.pump(&mut cx);
        assert_eq!(*runner.attempts.lock().unwrap(), vec![JobId(1)]);
        handle.set_paused(true).unwrap();
        drop(handle);
        // Closing the command stream must yield while awaiting cooperative completion.
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert!(!rt.tasks.lock().unwrap().is_empty());
        let scheduler = finish_bounded(&mut task, &rt);
        drop(task);
        assert_eq!(*runner.attempts.lock().unwrap(), vec![JobId(1)]);
        assert_eq!(scheduler.summary().cancelled, 3);
        assert!(scheduler.is_idle());
        for id in 1..=3 {
            assert_eq!(events.iter().filter(|event| **event == Event::Cancelled { job: JobId(id) }).count(), 1);
        }
    }

    #[test]
    fn closed_unpaused_queue_yields_while_running_then_drains_dependencies() {
        let mut scheduler = Scheduler::new(Limits::default());
        scheduler.add(vec![spec(1, &[]), spec(2, &[1])]).unwrap();
        let (driver, handle) = driver(scheduler);
        drop(handle);
        let rt = Arc::new(BoundedRuntime::default());
        let runner = Arc::new(ControlledRunner::default());
        let mut task = Box::pin(driver.run(Arc::new(Arc::clone(&runner)), rt.clone(), |_| {}));
        let mut cx = Context::from_waker(futures::task::noop_waker_ref());
        // Repeated polls with a closed stream and unfinished work must remain Pending.
        for _ in 0..3 {
            assert!(task.as_mut().poll(&mut cx).is_pending());
            rt.pump(&mut cx);
        }
        assert_eq!(*runner.attempts.lock().unwrap(), vec![JobId(1)]);
        runner.release.store(true, Ordering::SeqCst);
        let scheduler = finish_bounded(&mut task, &rt);
        assert_eq!(*runner.attempts.lock().unwrap(), vec![JobId(1), JobId(2)]);
        assert_eq!(scheduler.summary().done, 2);
        assert!(scheduler.is_idle());
    }

    #[test]
    fn runs_a_graph_with_retries_to_completion() {
        let rt = runtime();
        let mut runner = Scripted::default();
        runner.fails.insert(2, 2);
        let runner = Arc::new(runner);
        let limits = Limits { global: 2, default_per_lane: 2, per_lane: HashMap::new() };
        let (driver, handle) = driver(Scheduler::new(limits));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let jobs: Arc<dyn JobRunner> = Arc::new(Arc::clone(&runner));
        let task = driver.run(jobs, rt.clone(), move |e| sink.lock().unwrap().push(e));
        let scheduler = block_on(async {
            let control = async {
                let r = handle.add(vec![spec(1, &[]), spec(2, &[]), spec(3, &[1, 2]), spec(4, &[]), spec(5, &[])]).await;
                drop(handle);
                r
            };
            let (result, scheduler) = futures::join!(control, task);
            assert_eq!(result, Ok(Ok(())));
            scheduler
        });
        assert!(scheduler.is_idle());
        assert_eq!(scheduler.summary().done, 5);
        let attempts = runner.attempts.lock().unwrap().clone();
        let job2: Vec<&(u64, u32, String)> = attempts.iter().filter(|a| a.0 == 2).collect();
        assert_eq!(job2.len(), 3, "two transient failures, then success");
        assert!(job2.iter().all(|a| a.2 == job2[0].2), "same idempotency key on every attempt");
        assert!(runner.peak.load(Ordering::SeqCst) <= 2, "global limit");
        assert!(rt.now_ms() > 0, "backoff waited on the runtime clock");
        let events = events.lock().unwrap();
        assert_eq!(events.iter().filter(|e| matches!(e, Event::Retrying { .. })).count(), 2);
        assert!(events.contains(&Event::Done { job: JobId(3) }));
    }

    #[test]
    fn cancel_reaches_running_jobs() {
        let rt = runtime();
        let runner = Arc::new(Scripted { hang: vec![1], ..Scripted::default() });
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let jobs: Arc<dyn JobRunner> = Arc::new(Arc::clone(&runner));
        let task = driver.run(jobs, rt, |_| {});
        let scheduler = block_on(async {
            let control = async {
                assert_eq!(handle.add(vec![spec(1, &[]), spec(2, &[1])]).await, Ok(Ok(())));
                // Wait until the job is running, then cancel everything.
                let mut spins = 0u64;
                while runner.attempts.lock().unwrap().is_empty() {
                    spins += 1;
                    assert!(spins < 50_000_000, "the job never started");
                    yield_now().await;
                }
                handle.cancel(CancelScope::All).unwrap();
                drop(handle);
            };
            futures::join!(control, task).1
        });
        assert_eq!(scheduler.summary().cancelled, 2);
        assert!(scheduler.is_idle());
    }

    #[test]
    fn a_panicking_runner_fails_the_job_without_stalling() {
        let rt = runtime();
        let runner = Arc::new(Scripted { panic_on: vec![1], ..Scripted::default() });
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let jobs: Arc<dyn JobRunner> = Arc::new(Arc::clone(&runner));
        let task = driver.run(jobs, rt, |_| {});
        let scheduler = block_on(async {
            let control = async {
                let r = handle.add(vec![spec(1, &[]), spec(2, &[1]), spec(3, &[])]).await;
                drop(handle);
                r
            };
            let (r, s) = futures::join!(control, task);
            assert_eq!(r, Ok(Ok(())));
            s
        });
        assert!(matches!(scheduler.state(JobId(1)), Some(crate::scheduler::JobState::Failed { .. })));
        assert!(matches!(scheduler.state(JobId(2)), Some(crate::scheduler::JobState::Skipped { .. })));
        assert_eq!(scheduler.state(JobId(3)), Some(&crate::scheduler::JobState::Done));
    }

    /// A runtime that drops what it's given: the job must still end, not hang as running.
    struct DroppingRuntime;

    impl Runtime for DroppingRuntime {
        fn spawn(&self, task: TaskFuture<()>) {
            drop(task);
        }
        fn sleep(&self, _ms: u64) -> TaskFuture<()> {
            Box::pin(async {})
        }
        fn now_ms(&self) -> u64 {
            0
        }
    }

    #[test]
    fn dropped_tasks_fail_instead_of_hanging() {
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let jobs: Arc<dyn JobRunner> = Arc::new(Arc::new(Scripted::default()));
        let task = driver.run(jobs, Arc::new(DroppingRuntime), |_| {});
        let scheduler = block_on(async {
            let control = async {
                let r = handle.add(vec![spec(1, &[])]).await;
                drop(handle);
                r
            };
            futures::join!(control, task).1
        });
        assert!(matches!(scheduler.state(JobId(1)), Some(crate::scheduler::JobState::Failed { .. })));
    }

    struct PanicsWhileBuilding;

    impl JobRunner for PanicsWhileBuilding {
        fn run(&self, _start: Start, _cancel: CancelToken) -> TaskFuture<Outcome> {
            panic!("bug before the future exists")
        }
    }

    #[test]
    fn a_runner_panicking_before_its_future_exists_fails_the_job() {
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let task = driver.run(Arc::new(PanicsWhileBuilding), runtime(), |_| panic!("a listener bug"));
        let scheduler = block_on(async {
            let control = async {
                let r = handle.add(vec![spec(1, &[])]).await;
                drop(handle);
                r
            };
            futures::join!(control, task).1
        });
        assert!(matches!(scheduler.state(JobId(1)), Some(crate::scheduler::JobState::Failed { .. })));
    }

    #[test]
    fn pause_and_requeue_through_the_handle() {
        let rt = runtime();
        let runner = Arc::new(Scripted { panic_on: vec![1], ..Scripted::default() });
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let jobs: Arc<dyn JobRunner> = Arc::new(Arc::clone(&runner));
        let task = driver.run(jobs, rt, |_| {});
        let scheduler = block_on(async {
            let control = async {
                handle.set_paused(true).unwrap();
                assert_eq!(handle.add(vec![spec(1, &[]), spec(2, &[])]).await, Ok(Ok(())));
                assert!(runner.attempts.lock().unwrap().is_empty(), "paused: nothing starts");
                handle.set_paused(false).unwrap();
                let mut spins = 0u64;
                while runner.attempts.lock().unwrap().len() < 2 {
                    spins += 1;
                    assert!(spins < 50_000_000, "jobs never started");
                    yield_now().await;
                }
                // Job 1 panicked; once it has failed, requeue it (it fails again, which is fine).
                let mut tries = 0;
                loop {
                    tries += 1;
                    assert!(tries < 1_000_000, "job 1 never failed");
                    if handle.requeue(vec![(JobId(1), "nodecraft:test:1:retry".into())]).await == Ok(Ok(())) {
                        break;
                    }
                    yield_now().await;
                }
                drop(handle);
            };
            futures::join!(control, task).1
        });
        let attempts = runner.attempts.lock().unwrap().clone();
        let keys: Vec<&str> = attempts.iter().filter(|a| a.0 == 1).map(|a| a.2.as_str()).collect();
        assert_eq!(keys, ["nodecraft:test:1", "nodecraft:test:1:retry"], "requeued once, under a new key");
        handle_forget_compiles();
        assert!(scheduler.is_idle());
    }

    fn handle_forget_compiles() {
        let (_driver, handle) = driver(Scheduler::new(Limits::default()));
        assert_eq!(handle.forget_finished(), Ok(()));
    }

    #[test]
    fn bad_additions_are_reported_through_the_handle() {
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let jobs: Arc<dyn JobRunner> = Arc::new(Arc::new(Scripted::default()));
        let task = driver.run(jobs, runtime(), |_| {});
        block_on(async {
            let control = async {
                let r = handle.add(vec![spec(1, &[1])]).await;
                drop(handle);
                r
            };
            let (r, _) = futures::join!(control, task);
            assert_eq!(r, Ok(Err(SchedulerError::Cycle(JobId(1)))));
        });
    }

    #[test]
    fn cancel_token_wakes_waiters() {
        let token = CancelToken::default();
        let waiter = token.cancelled();
        let t2 = token.clone();
        let pool = ThreadPool::new().unwrap();
        pool.spawn_ok(async move { t2.cancel() });
        block_on(waiter);
        assert!(token.is_cancelled());
        block_on(token.cancelled());
    }
}
