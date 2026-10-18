//! Safe single-thread FIFO polling; wakers never retain task futures or Session state.
use futures::task::{ArcWake, waker};
use nodecraft_exec::driver::{Runtime, TaskFuture};
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll, Waker},
};
const SLOTS: usize = 256;
type Task = (u64, TaskFuture<()>);
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
#[derive(Default)]
struct Ready {
    queue: VecDeque<(usize, u64)>,
    live: Vec<u64>,
}
struct Wake {
    ready: Arc<Mutex<Ready>>,
    slot: usize,
    generation: u64,
}
impl ArcWake for Wake {
    fn wake_by_ref(this: &Arc<Self>) {
        let mut r = lock(&this.ready);
        if r.live.get(this.slot) == Some(&this.generation) && !r.queue.contains(&(this.slot, this.generation)) {
            r.queue.push_back((this.slot, this.generation));
        }
    }
}
struct Timer {
    generation: u64,
    deadline: u64,
    waker: Waker,
}
struct Timers {
    now: u64,
    serial: u64,
    entries: Vec<Option<Timer>>,
}
/// Shared static fault owner; callbacks never panic to signal admission refusal.
#[derive(Clone, Default)]
pub struct Fault(Arc<Mutex<Option<&'static str>>>);
impl Fault {
    pub fn set(&self, reason: &'static str) {
        lock(&self.0).get_or_insert(reason);
    }
    pub fn get(&self) -> Option<&'static str> {
        *lock(&self.0)
    }
}
/// Tasks are removed from locks before polling, and returned only if still pending.
pub struct ManualRuntime {
    ready: Arc<Mutex<Ready>>,
    tasks: Mutex<Vec<Option<Task>>>,
    timers: Arc<Mutex<Timers>>,
    serial: Mutex<u64>,
    polls: Mutex<u64>,
    driver_waker: Mutex<Option<Waker>>,
    pub fault: Fault,
}
impl Default for ManualRuntime {
    fn default() -> Self {
        Self {
            ready: Arc::new(Mutex::new(Ready { queue: VecDeque::with_capacity(SLOTS), live: vec![0; SLOTS] })),
            tasks: Mutex::new((0..SLOTS).map(|_| None).collect()),
            timers: Arc::new(Mutex::new(Timers { now: 0, serial: 0, entries: (0..SLOTS).map(|_| None).collect() })),
            serial: Mutex::new(0),
            polls: Mutex::new(0),
            driver_waker: Mutex::new(None),
            fault: Fault::default(),
        }
    }
}
impl Runtime for ManualRuntime {
    fn spawn(&self, task: TaskFuture<()>) {
        let mut tasks = lock(&self.tasks);
        let index = lock(&self.ready).live.iter().position(|g| *g == 0);
        let next = lock(&self.serial).checked_add(1);
        if let (Some(index), Some(next)) = (index, next) {
            *lock(&self.serial) = next;
            if let Some(slot) = tasks.get_mut(index) {
                *slot = Some((next, task));
            }
            let mut r = lock(&self.ready);
            if let Some(slot) = r.live.get_mut(index) {
                *slot = next;
            }
            r.queue.push_back((index, next));
        } else {
            self.fault.set("task_capacity");
            drop(tasks);
            drop(task);
        }
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let deadline = lock(&self.timers).now.checked_add(ms);
        if deadline.is_none_or(|d| d > 60_000) {
            self.fault.set("virtual_duration");
        }
        Box::pin(Sleep {
            timers: self.timers.clone(),
            fault: self.fault.clone(),
            deadline: deadline.unwrap_or(u64::MAX),
            entry: None,
            zero: ms == 0,
            yielded: false,
        })
    }
    fn now_ms(&self) -> u64 {
        lock(&self.timers).now
    }
}
impl ManualRuntime {
    pub fn poll_one(&self) -> bool {
        self.poll_selected(None)
    }
    /// Fixture-only readiness barrier: the controller's first task is Driver slot0.
    /// Preserve its wake while allowing an already-ready worker to finish its channel send.
    pub(super) fn poll_worker(&self) -> bool {
        self.poll_selected(Some(0))
    }
    fn poll_selected(&self, excluded: Option<usize>) -> bool {
        let next = {
            let mut ready = lock(&self.ready);
            let index = ready.queue.iter().position(|(slot, _)| Some(*slot) != excluded);
            index.and_then(|i| ready.queue.remove(i))
        };
        let Some((slot, generation)) = next else { return false };
        let task = {
            let mut t = lock(&self.tasks);
            match t.get_mut(slot) {
                Some(s) if s.as_ref().is_some_and(|(g, _)| *g == generation) => s.take(),
                _ => None,
            }
        };
        let Some((_, mut task)) = task else { return true };
        let mut polls = lock(&self.polls);
        *polls = polls.saturating_add(1);
        if *polls > 100_000 {
            self.fault.set("poll_capacity");
            drop(polls);
            drop(task);
            return true;
        }
        drop(polls);
        let wake = Arc::new(Wake { ready: self.ready.clone(), slot, generation });
        let w = waker(wake);
        if slot == 0 {
            *lock(&self.driver_waker) = Some(w.clone());
        }
        let pending = task.as_mut().poll(&mut Context::from_waker(&w)).is_pending();
        if pending && lock(&self.ready).live.get(slot) == Some(&generation) {
            if let Some(s) = lock(&self.tasks).get_mut(slot) {
                *s = Some((generation, task));
            }
        } else {
            let mut r = lock(&self.ready);
            if let Some(s) = r.live.get_mut(slot) {
                *s = 0;
            }
            r.queue.retain(|p| *p != (slot, generation));
        }
        true
    }
    pub(super) fn driver_timer_deadline(&self) -> Option<u64> {
        let driver = lock(&self.driver_waker).as_ref()?.clone();
        lock(&self.timers).entries.iter().flatten().filter(|timer| timer.waker.will_wake(&driver)).map(|timer| timer.deadline).min()
    }
    pub fn advance(&self) -> bool {
        self.advance_with(None)
    }
    pub fn advance_with(&self, external: Option<u64>) -> bool {
        let mut timers = lock(&self.timers);
        let Some(next) = timers.entries.iter().flatten().map(|t| t.deadline).chain(external).min() else { return false };
        if next > 60_000 {
            self.fault.set("virtual_duration");
            return false;
        }
        timers.now = next.max(timers.now);
        let now = timers.now;
        let mut wakes = Vec::with_capacity(SLOTS);
        for slot in &mut timers.entries {
            if slot.as_ref().is_some_and(|t| t.deadline <= now)
                && let Some(t) = slot.take()
            {
                wakes.push(t.waker);
            }
        }
        drop(timers);
        for w in wakes {
            w.wake();
        }
        true
    }
    pub fn idle(&self) -> bool {
        lock(&self.ready).live.iter().all(|g| *g == 0) && lock(&self.timers).entries.iter().all(Option::is_none)
    }
    pub fn polls(&self) -> u64 {
        *lock(&self.polls)
    }
    pub fn stop(&self) {
        let tasks = {
            let mut t = lock(&self.tasks);
            std::mem::replace(&mut *t, (0..SLOTS).map(|_| None).collect())
        };
        drop(tasks);
        let mut r = lock(&self.ready);
        r.queue.clear();
        r.live.fill(0);
    }
}
struct Sleep {
    timers: Arc<Mutex<Timers>>,
    fault: Fault,
    deadline: u64,
    entry: Option<(usize, u64)>,
    zero: bool,
    yielded: bool,
}
impl Future for Sleep {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.zero && !self.yielded {
            self.yielded = true;
            cx.waker().wake_by_ref();
            return Poll::Pending;
        }
        let mut t = lock(&self.timers);
        if t.now >= self.deadline {
            return Poll::Ready(());
        }
        if let Some((slot, generation)) = self.entry
            && let Some(Some(e)) = t.entries.get_mut(slot)
            && e.generation == generation
        {
            e.waker = cx.waker().clone();
            return Poll::Pending;
        }
        let slot = t.entries.iter().position(Option::is_none);
        let next = t.serial.checked_add(1);
        if let (Some(slot), Some(next)) = (slot, next) {
            t.serial = next;
            if let Some(e) = t.entries.get_mut(slot) {
                *e = Some(Timer { generation: next, deadline: self.deadline, waker: cx.waker().clone() });
            }
            drop(t);
            self.entry = Some((slot, next));
        } else {
            self.fault.set("timer_capacity");
        }
        Poll::Pending
    }
}
impl Drop for Sleep {
    fn drop(&mut self) {
        if let Some((slot, g)) = self.entry {
            let mut t = lock(&self.timers);
            if let Some(e) = t.entries.get_mut(slot)
                && e.as_ref().is_some_and(|e| e.generation == g)
            {
                *e = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn drain(r: &ManualRuntime) {
        for _ in 0..1000 {
            if r.poll_one() {
                continue;
            }
            if r.advance() {
                continue;
            }
            return;
        }
        panic!("bounded control");
    }
    #[test]
    fn simultaneous_sleeps_advance_to_earliest_not_sum() {
        let r = Arc::new(ManualRuntime::default());
        let seen = Arc::new(Mutex::new(Vec::new()));
        for ms in [10, 5, 10] {
            let c = r.clone();
            let s = seen.clone();
            r.spawn(Box::pin(async move {
                c.sleep(ms).await;
                lock(&s).push(c.now_ms());
            }));
        }
        drain(&r);
        assert_eq!(*lock(&seen), [5, 10, 10]);
        assert!(r.idle());
    }
    #[test]
    fn dropped_timer_cannot_advance_time() {
        let r = ManualRuntime::default();
        let mut sleep = r.sleep(50);
        let w = futures::task::noop_waker();
        assert!(sleep.as_mut().poll(&mut Context::from_waker(&w)).is_pending());
        drop(sleep);
        assert!(!r.advance());
        assert_eq!(r.now_ms(), 0);
    }
    #[test]
    fn zero_sleep_yields_and_spawn_during_poll_keeps_parent_slot() {
        let r = Arc::new(ManualRuntime::default());
        let trace = Arc::new(Mutex::new(Vec::new()));
        let c = r.clone();
        let t = trace.clone();
        r.spawn(Box::pin(async move {
            lock(&t).push(1);
            let t2 = t.clone();
            c.spawn(Box::pin(async move {
                lock(&t2).push(2);
            }));
            c.sleep(0).await;
            lock(&t).push(3);
        }));
        drain(&r);
        assert_eq!(*lock(&trace), [1, 2, 3]);
        assert_eq!(r.now_ms(), 0);
        assert!(r.idle());
    }
    #[test]
    fn task_bound_is_fault_and_pending_work_is_not_success() {
        let r = ManualRuntime::default();
        for _ in 0..257 {
            r.spawn(Box::pin(futures::future::pending()));
        }
        assert_eq!(r.fault.get(), Some("task_capacity"));
        assert!(!r.idle());
        r.stop();
        assert!(r.idle());
    }
    /// Separate dependency-API control. Real Run frontiers use unchanged empty-deps specs;
    /// this does not claim a naturally occurring Run-generated Skipped event.
    #[test]
    fn direct_driver_dependency_skip_never_starts_blocked_job() {
        use nodecraft_exec::{
            driver::{CancelToken, JobRunner, driver},
            scheduler::{Event, JobId, JobSpec, JobState, Lane, Limits, Outcome, RetryPolicy, Scheduler, Start},
        };
        struct Fail(Arc<Mutex<Vec<JobId>>>);
        impl JobRunner for Fail {
            fn run(&self, s: Start, _: CancelToken) -> TaskFuture<Outcome> {
                lock(&self.0).push(s.job);
                Box::pin(async { Outcome::Failed { error: "public dependency refusal".into(), transient: false } })
            }
        }
        let runtime = Arc::new(ManualRuntime::default());
        let calls = Arc::new(Mutex::new(Vec::with_capacity(2)));
        let runner = Arc::new(Fail(calls.clone()));
        let mut scheduler = Scheduler::new(Limits::default());
        scheduler
            .add(vec![
                JobSpec {
                    id: JobId(101),
                    deps: vec![],
                    lane: Lane::new("local"),
                    retry: RetryPolicy { max_attempts: 1, base_delay_ms: 1, max_delay_ms: 1 },
                    idempotency_key: "direct-source-101".into(),
                },
                JobSpec {
                    id: JobId(102),
                    deps: vec![JobId(101)],
                    lane: Lane::new("local"),
                    retry: RetryPolicy { max_attempts: 1, base_delay_ms: 1, max_delay_ms: 1 },
                    idempotency_key: "direct-source-102".into(),
                },
            ])
            .unwrap();
        let (d, h) = driver(scheduler);
        drop(h);
        let end = Arc::new(Mutex::new(None));
        let result = end.clone();
        let events = Arc::new(Mutex::new(Vec::with_capacity(8)));
        let sink = events.clone();
        let r = runtime.clone();
        runtime.spawn(Box::pin(async move {
            let s = d.run(runner, r, move |e| lock(&sink).push(e)).await;
            *lock(&result) = Some(s);
        }));
        for _ in 0..1000 {
            if runtime.idle() {
                break;
            }
            assert!(runtime.poll_one() || runtime.advance());
        }
        assert!(runtime.idle());
        assert_eq!(*lock(&calls), vec![JobId(101)]);
        assert!(lock(&events).iter().any(|e| matches!(e, Event::Skipped { job: JobId(102), because: JobId(101) })));
        assert!(matches!(lock(&end).as_ref().unwrap().state(JobId(102)), Some(JobState::Skipped { because: JobId(101) })));
    }
    /// A separate actual Handle API control. Closed is ambiguous, unlike Full/PayloadLimit;
    /// it never authorizes releasing an unrelated paid owner.
    #[test]
    fn dropping_receiver_after_add_construction_loses_acknowledgement() {
        use nodecraft_exec::{
            driver::{DriverError, driver},
            scheduler::{JobId, JobSpec, Lane, Limits, RetryPolicy, Scheduler},
        };
        let (driver, handle) = driver(Scheduler::new(Limits::default()));
        let ack = handle.add(vec![JobSpec {
            id: JobId(99),
            deps: vec![],
            lane: Lane::new("local"),
            retry: RetryPolicy { max_attempts: 1, base_delay_ms: 1, max_delay_ms: 1 },
            idempotency_key: "explicit-ack-owner".into(),
        }]);
        drop(driver);
        assert_eq!(futures::executor::block_on(ack), Err(DriverError::Closed));
    }

    #[test]
    fn timer_capacity_and_virtual_duration_fault_without_fake_success() {
        let runtime = Arc::new(ManualRuntime::default());
        let r = runtime.clone();
        runtime.spawn(Box::pin(async move {
            let sleeps: Vec<_> = (0..257).map(|_| r.sleep(1)).collect();
            futures::future::join_all(sleeps).await;
        }));
        assert!(runtime.poll_one());
        assert_eq!(runtime.fault.get(), Some("timer_capacity"));
        assert!(!runtime.idle());
        runtime.stop();
        assert!(runtime.idle());
        assert!(!runtime.advance());
        let runtime = Arc::new(ManualRuntime::default());
        let r = runtime.clone();
        runtime.spawn(Box::pin(async move {
            r.sleep(60_001).await;
        }));
        assert!(runtime.poll_one());
        assert!(!runtime.advance());
        assert_eq!(runtime.fault.get(), Some("virtual_duration"));
        assert!(!runtime.idle());
        runtime.stop();
        assert!(runtime.idle());
    }
    #[test]
    fn stale_waker_cannot_poll_a_reused_task_slot() {
        let runtime = ManualRuntime::default();
        let old = Arc::new(Mutex::new(None));
        let saved = old.clone();
        runtime.spawn(Box::pin(futures::future::poll_fn(move |cx| {
            *lock(&saved) = Some(cx.waker().clone());
            Poll::Ready(())
        })));
        assert!(runtime.poll_one());
        runtime.spawn(Box::pin(futures::future::pending()));
        assert!(runtime.poll_one());
        lock(&old).as_ref().unwrap().wake_by_ref();
        assert!(!runtime.poll_one());
        assert!(!runtime.idle());
        runtime.stop();
        assert!(runtime.idle());
    }
}
