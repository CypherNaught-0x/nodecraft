//! The order of a job's messages, checked by the host for every message it receives.
//!
//! `invoke` (host) → `accept` → `progress`* (never going back) → exactly one of `result` or
//! `error`. A host `cancel` is terminal at once: whatever the guest sends for that job afterwards
//! is discarded, and `cancel_ack` only confirms it. Anything else (an unknown or finished job, a
//! second `accept`, progress before `accept`, a result for another epoch) is a sequence error,
//! recorded by the host as a bounded diagnostic and never applied. A result with more outputs or
//! bytes than the invocation's output budget is refused (`too_large`).
//!
//! Cancelled jobs don't count against [`MAX_JOBS`]: the host forgets one once its sandbox is
//! reset after the grace period ([`JobTracker::forget`], [`JobTracker::reset`]), and the oldest
//! cancelled entries are dropped first if too many wait for an acknowledgement.
use crate::error::{Code, Result, err};
use crate::frame::Message;
use crate::id::Token;
use std::collections::BTreeMap;

/// Most running jobs a tracker follows at once.
pub const MAX_JOBS: usize = 64;
/// Most cancelled jobs kept waiting for their acknowledgement.
pub const MAX_CANCELLED: usize = 64;

/// What one job may return, from its `invoke`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputBudget {
    pub items: u32,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Invoked,
    Running {
        progress: u32,
    },
    /// The host cancelled; the guest may still be sending.
    Cancelled,
}

/// What the host does with a guest message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Act on it.
    Apply,
    /// The job was cancelled: drop it (and release any staged output).
    Discard,
}

/// Jobs in flight for one package instance.
#[derive(Debug, Default)]
pub struct JobTracker {
    jobs: BTreeMap<Token, Entry>,
    /// Cancelled jobs, oldest first.
    cancelled: std::collections::VecDeque<Token>,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    state: State,
    epoch: u64,
    budget: OutputBudget,
}

impl JobTracker {
    /// The host sends `invoke` for `job` under `grant_epoch`, with its output budget.
    pub fn invoked(&mut self, job: Token, grant_epoch: u64, budget: OutputBudget) -> Result<()> {
        if self.jobs.contains_key(&job) {
            return err(Code::Sequence, "a job ID is reused while the job is open");
        }
        if self.open() >= MAX_JOBS {
            return err(Code::TooLarge, "too many jobs are running");
        }
        self.jobs.insert(job, Entry { state: State::Invoked, epoch: grant_epoch, budget });
        Ok(())
    }

    /// The host cancels `job`: from now on its messages are discarded.
    pub fn cancelled(&mut self, job: &Token) -> Result<()> {
        let Some(entry) = self.jobs.get_mut(job) else {
            return err(Code::Sequence, "cancel for a job that isn't open");
        };
        if entry.state == State::Cancelled {
            return Ok(());
        }
        entry.state = State::Cancelled;
        self.cancelled.push_back(job.clone());
        while self.cancelled.len() > MAX_CANCELLED {
            if let Some(oldest) = self.cancelled.pop_front() {
                self.jobs.remove(&oldest);
            }
        }
        Ok(())
    }

    /// The host gave up on `job` (its sandbox was reset after the grace period).
    pub fn forget(&mut self, job: &Token) {
        self.jobs.remove(job);
        self.cancelled.retain(|j| j != job);
    }

    /// The sandbox was reset: no job is open any more.
    pub fn reset(&mut self) {
        self.jobs.clear();
        self.cancelled.clear();
    }

    /// Checks a message from the guest. Terminal messages close the job.
    pub fn observe(&mut self, message: &Message) -> Result<Verdict> {
        let Some(job) = message.job_id() else {
            return err(Code::Sequence, "a lifecycle message inside a job stream");
        };
        let Some(entry) = self.jobs.get(job).copied() else {
            return err(Code::Sequence, "a message for a job that isn't open");
        };
        match (entry.state, message) {
            (State::Cancelled, Message::CancelAck { .. } | Message::Error { .. }) => {
                self.forget(job);
                Ok(Verdict::Discard)
            }
            (State::Cancelled, Message::Accept { .. } | Message::Progress { .. } | Message::Result { .. }) => Ok(Verdict::Discard),
            (State::Invoked, Message::Accept { .. }) => {
                self.set(job, State::Running { progress: 0 });
                Ok(Verdict::Apply)
            }
            (State::Running { progress }, Message::Progress { value, .. }) if *value >= progress => {
                self.set(job, State::Running { progress: *value });
                Ok(Verdict::Apply)
            }
            (State::Running { .. }, Message::Progress { .. }) => err(Code::Sequence, "progress went backwards"),
            (State::Running { .. }, Message::Result { grant_epoch, outputs, .. }) => {
                self.forget(job);
                if *grant_epoch != entry.epoch {
                    return err(Code::Sequence, "a result for another grant epoch");
                }
                let bytes = outputs.iter().try_fold(0u64, |sum, o| sum.checked_add(o.bytes));
                let fits = u32::try_from(outputs.len()).is_ok_and(|n| n <= entry.budget.items) && bytes.is_some_and(|b| b <= entry.budget.bytes);
                if fits { Ok(Verdict::Apply) } else { err(Code::TooLarge, "a result beyond its output budget") }
            }
            // A guest may refuse a job before accepting it.
            (State::Invoked | State::Running { .. }, Message::Error { .. }) => {
                self.forget(job);
                Ok(Verdict::Apply)
            }
            _ => err(Code::Sequence, "a message out of order for its job"),
        }
    }

    fn set(&mut self, job: &Token, state: State) {
        if let Some(entry) = self.jobs.get_mut(job) {
            entry.state = state;
        }
    }

    /// Jobs running (not cancelled).
    pub fn open(&self) -> usize {
        self.jobs.len().saturating_sub(self.cancelled.len())
    }
}
