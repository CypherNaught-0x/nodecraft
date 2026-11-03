//! A template's `limits.concurrency` (#174): the most requests the host sends one plugin
//! provider at once.
//!
//! The scheduler caps how many of one run's jobs a plugin provider holds (the routing binding
//! carries the template's limit as its lane's cap), but runs don't share a scheduler, and jobs
//! followed again after a restart run outside one. A [`RequestGate`] is shared by everything that
//! sends to one plugin provider, so its requests stay within the limit however many runs and
//! restored jobs use it. A request waits for a slot before anything about it is decided, so a
//! wait that is given up sends nothing and bills nothing.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

/// Slots for at most `size` requests at once.
#[derive(Debug)]
pub struct RequestGate {
    size: usize,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    held: usize,
    /// Who is waiting for a slot, by ticket; a wait that is dropped takes itself out.
    waiting: BTreeMap<u64, Waker>,
    next: u64,
}

impl RequestGate {
    /// A gate for `size` requests at once (at least one).
    pub fn new(size: usize) -> Arc<Self> {
        Arc::new(Self { size: size.max(1), state: Mutex::new(State::default()) })
    }

    /// How many requests it lets through at once.
    pub fn size(&self) -> usize {
        self.size
    }

    /// How many requests hold a slot now.
    pub fn held(&self) -> usize {
        self.lock().held
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for a slot; it is given back when the returned permit is dropped.
    pub fn acquire(self: &Arc<Self>) -> Acquire {
        Acquire { gate: Arc::clone(self), ticket: None }
    }

    fn release(&self) {
        let woken: Vec<Waker> = {
            let mut state = self.lock();
            state.held = state.held.saturating_sub(1);
            // Everyone waiting tries again; there are at most a few slots, and a waiter that has
            // gone away has already taken itself out.
            state.waiting.values().cloned().collect()
        };
        for waker in woken {
            waker.wake();
        }
    }
}

/// A held slot.
#[derive(Debug)]
pub struct Permit {
    gate: Arc<RequestGate>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// The wait for a slot, from [`RequestGate::acquire`].
#[derive(Debug)]
pub struct Acquire {
    gate: Arc<RequestGate>,
    ticket: Option<u64>,
}

impl Future for Acquire {
    type Output = Permit;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Permit> {
        let this = &mut *self;
        let mut state = this.gate.lock();
        if state.held < this.gate.size {
            state.held = state.held.saturating_add(1);
            if let Some(ticket) = this.ticket.take() {
                state.waiting.remove(&ticket);
            }
            drop(state);
            return Poll::Ready(Permit { gate: Arc::clone(&this.gate) });
        }
        let ticket = match this.ticket {
            Some(ticket) => ticket,
            None => {
                let ticket = state.next;
                state.next = state.next.wrapping_add(1);
                this.ticket = Some(ticket);
                ticket
            }
        };
        state.waiting.insert(ticket, cx.waker().clone());
        Poll::Pending
    }
}

impl Drop for Acquire {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() {
            self.gate.lock().waiting.remove(&ticket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;

    #[test]
    fn a_gate_lets_through_its_size_and_the_next_once_one_is_given_back() {
        let gate = RequestGate::new(2);
        let a = gate.acquire().now_or_never().expect("first slot");
        let b = gate.acquire().now_or_never().expect("second slot");
        let mut c = Box::pin(gate.acquire());
        let waker = futures::task::noop_waker();
        assert!(c.as_mut().poll(&mut Context::from_waker(&waker)).is_pending(), "a third waits");
        assert_eq!(gate.held(), 2);
        drop(a);
        assert!(matches!(c.as_mut().poll(&mut Context::from_waker(&waker)), Poll::Ready(_)), "the freed slot is taken");
        drop(b);
    }

    #[test]
    fn a_wait_given_up_holds_nothing() {
        let gate = RequestGate::new(1);
        let a = gate.acquire().now_or_never().expect("the slot");
        let mut waiting = Box::pin(gate.acquire());
        let waker = futures::task::noop_waker();
        assert!(waiting.as_mut().poll(&mut Context::from_waker(&waker)).is_pending());
        drop(waiting);
        assert!(gate.lock().waiting.is_empty(), "the dropped wait took itself out");
        drop(a);
        assert_eq!(gate.held(), 0);
        assert!(gate.acquire().now_or_never().is_some(), "the slot is free again");
        assert_eq!(RequestGate::new(0).size(), 1, "a gate always lets one through");
    }
}
