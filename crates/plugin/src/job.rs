//! The order of a job's messages, checked by the host for every message it receives.
//!
//! `invoke` (host) → `accept` → `progress`* (never going back) → exactly one of `result` or
//! `error`. A host `cancel` is terminal at once: whatever the guest sends for that job afterwards
//! is discarded, and `cancel_ack` only confirms it. Anything else (an unknown or finished job, a
//! second `accept`, progress before `accept`, a result for another epoch) is a sequence error,
//! recorded by the host as a bounded diagnostic and never applied.
use crate::error::{Code, Result, err};
use crate::frame::Message;
use crate::id::Token;
use std::collections::BTreeMap;

/// Most jobs a tracker follows at once (the host's active-plus-draining ceiling).
pub const MAX_JOBS: usize = 64;

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
    jobs: BTreeMap<Token, (State, u64)>,
}

impl JobTracker {
    /// The host sends `invoke` for `job` under `grant_epoch`.
    pub fn invoked(&mut self, job: Token, grant_epoch: u64) -> Result<()> {
        if self.jobs.contains_key(&job) {
            return err(Code::Sequence, "a job ID is reused while the job is open");
        }
        if self.jobs.len() >= MAX_JOBS {
            return err(Code::TooLarge, "too many jobs are open");
        }
        self.jobs.insert(job, (State::Invoked, grant_epoch));
        Ok(())
    }

    /// The host cancels `job`: from now on its messages are discarded.
    pub fn cancelled(&mut self, job: &Token) -> Result<()> {
        match self.jobs.get_mut(job) {
            Some((state, _)) => {
                *state = State::Cancelled;
                Ok(())
            }
            None => err(Code::Sequence, "cancel for a job that isn't open"),
        }
    }

    /// Checks a message from the guest. Terminal messages close the job.
    pub fn observe(&mut self, message: &Message) -> Result<Verdict> {
        let Some(job) = message.job_id() else {
            return err(Code::Sequence, "a lifecycle message inside a job stream");
        };
        let Some((state, epoch)) = self.jobs.get(job).copied() else {
            return err(Code::Sequence, "a message for a job that isn't open");
        };
        let close = |jobs: &mut BTreeMap<Token, (State, u64)>| {
            jobs.remove(job);
        };
        match (state, message) {
            (State::Cancelled, Message::CancelAck { .. } | Message::Error { .. }) => {
                close(&mut self.jobs);
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
            (State::Running { .. }, Message::Result { grant_epoch, .. }) => {
                close(&mut self.jobs);
                if *grant_epoch == epoch { Ok(Verdict::Apply) } else { err(Code::Sequence, "a result for another grant epoch") }
            }
            // A guest may refuse a job before accepting it.
            (State::Invoked | State::Running { .. }, Message::Error { .. }) => {
                close(&mut self.jobs);
                Ok(Verdict::Apply)
            }
            _ => err(Code::Sequence, "a message out of order for its job"),
        }
    }

    fn set(&mut self, job: &Token, state: State) {
        if let Some(entry) = self.jobs.get_mut(job) {
            entry.0 = state;
        }
    }

    pub fn open(&self) -> usize {
        self.jobs.len()
    }
}
