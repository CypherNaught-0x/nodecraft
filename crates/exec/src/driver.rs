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

/// The driver has stopped (all handles dropped and the queue drained, or the driver was dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriverGone;

impl std::fmt::Display for DriverGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the run queue has stopped")
    }
}

impl std::error::Error for DriverGone {}

/// Sends commands to a running driver. Clone it freely; the driver finishes once every handle is
/// dropped and its jobs have ended.
#[derive(Clone)]
pub struct Handle(mpsc::UnboundedSender<Command>);

impl Handle {
    /// Adds jobs; resolves to the scheduler's verdict on them.
    pub async fn add(&self, jobs: Vec<JobSpec>) -> Result<Result<(), SchedulerError>, DriverGone> {
        let (tx, rx) = oneshot::channel();
        self.0.unbounded_send(Command::Add(jobs, tx)).map_err(|_| DriverGone)?;
        rx.await.map_err(|_| DriverGone)
    }

    pub fn cancel(&self, scope: CancelScope) -> Result<(), DriverGone> {
        self.0.unbounded_send(Command::Cancel(scope)).map_err(|_| DriverGone)
    }

    /// Retries failed, cancelled or skipped jobs under new idempotency keys (see
    /// [`Scheduler::requeue`]).
    pub async fn requeue(&self, jobs: Vec<(JobId, String)>) -> Result<Result<(), SchedulerError>, DriverGone> {
        let (tx, rx) = oneshot::channel();
        self.0.unbounded_send(Command::Requeue(jobs, tx)).map_err(|_| DriverGone)?;
        rx.await.map_err(|_| DriverGone)
    }

    /// Drops finished jobs from the queue (call when the queue panel is cleared, or periodically
    /// in a long session).
    pub fn forget_finished(&self) -> Result<(), DriverGone> {
        self.0.unbounded_send(Command::Forget).map_err(|_| DriverGone)
    }

    /// Stops or resumes starting jobs (budget reached, user pause); running jobs carry on.
    pub fn set_paused(&self, paused: bool) -> Result<(), DriverGone> {
        self.0.unbounded_send(Command::Pause(paused)).map_err(|_| DriverGone)
    }
}

/// A driver ready to run: spawn [`Driver::run`] on the runtime and keep the [`Handle`].
pub struct Driver {
    scheduler: Scheduler,
    commands: mpsc::UnboundedReceiver<Command>,
}

/// Creates a driver around `scheduler` and the handle that controls it.
pub fn driver(scheduler: Scheduler) -> (Driver, Handle) {
    let (tx, rx) = mpsc::unbounded();
    (Driver { scheduler, commands: rx }, Handle(tx))
}

impl Driver {
    /// Runs until every handle is dropped and no job is left, calling `on_event` for each
    /// scheduler event. Returns the scheduler for inspection.
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
            futures::select! {
                done = done_rx.next() => {
                    if let Some((job, outcome)) = done {
                        tokens.remove(&job);
                        // `NotRunning` can only follow a scheduler bug; the job is ignored either way.
                        let _ = scheduler.report(job, outcome, runtime.now_ms());
                    }
                }
                command = commands.next() => match command {
                    Some(Command::Add(jobs, reply)) => {
                        let _ = reply.send(scheduler.add(jobs));
                    }
                    Some(Command::Requeue(jobs, reply)) => {
                        let _ = reply.send(scheduler.requeue(&jobs));
                    }
                    Some(Command::Pause(paused)) => scheduler.set_paused(paused),
                    Some(Command::Forget) => scheduler.forget_finished(),
                    Some(Command::Cancel(scope)) => {
                        for job in scheduler.cancel(scope) {
                            if let Some(token) = tokens.get(&job) {
                                token.cancel();
                            }
                        }
                    }
                    None => open = false,
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
