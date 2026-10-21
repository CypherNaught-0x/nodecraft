//! Live recompute in the app (#88): after an edit, stale action nodes on the shown canvas are
//! recomputed on a worker thread and their results recorded into the edit's undo step
//! (`engine::live_actions`). At most one recompute runs at a time; while a slider drags, each
//! finished round that's already out of date is dropped and the next one starts from the
//! current document, so the card follows the drag at the speed the work allows.
use super::NodeCraftApp;
use nodecraft_engine::live_actions::{self, LiveOutcome};
use nodecraft_types::{CanvasId, Error, NodeId};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
#[cfg(not(target_arch = "wasm32"))]
use std::time::{Duration, Instant};

/// Longest the app waits, when it closes, for a recompute still running (#594).
#[cfg(not(target_arch = "wasm32"))]
const CLOSE_WAIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct Live {
    running: Option<Receiver<Result<LiveOutcome, Error>>>,
    /// The worker computing `running`, waited for when the app closes (#594).
    worker: Option<std::thread::JoinHandle<()>>,
    /// The canvas and revision last looked at.
    checked: Option<(CanvasId, u64)>,
    /// Nodes that failed, with the input hash they failed with: not retried (or reported
    /// again) until their inputs or settings change.
    failed: BTreeMap<NodeId, nodecraft_types::Blake3>,
    kinds: Option<Arc<nodecraft_nodes::Registry>>,
    /// The canvas as it was when there was nothing to recompute: moving its nodes changes the
    /// revision but not that (#549).
    settled: Option<live_actions::Settled>,
}

impl NodeCraftApp {
    /// Every frame: records a finished recompute, and starts the next one if the document
    /// changed. Repaints while work is running so its result shows as soon as it lands.
    pub(super) fn live_recompute(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.live.running {
            match receiver.try_recv() {
                Ok(Ok(outcome)) => {
                    self.live.running = None;
                    self.live.settled = None;
                    self.live_failures(&outcome.failed);
                    if let Err(error) = live_actions::apply(&mut self.session, outcome) {
                        log::warn!("live result not recorded: {}", error.message);
                    }
                }
                Ok(Err(error)) => {
                    self.live.running = None;
                    log::warn!("live recompute failed: {}", error.message);
                }
                // The worker asks for a repaint when it is done.
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => self.live.running = None,
            }
        }
        let now = (self.canvas, self.session.revision());
        if self.live.checked == Some(now) {
            return;
        }
        self.live.checked = Some(now);
        if self.live.settled.as_ref().is_some_and(|settled| settled.holds(&self.session, self.canvas)) {
            return;
        }
        self.live.settled = None;
        let job = match live_actions::pending(&self.session, self.canvas, &self.live.failed) {
            Ok(Some(job)) => job,
            Ok(None) => {
                self.live.settled = live_actions::Settled::now(&self.session, self.canvas);
                return;
            }
            Err(error) => {
                log::warn!("live recompute can't plan: {}", error.message);
                return;
            }
        };
        let kinds = match &self.live.kinds {
            Some(kinds) => Arc::clone(kinds),
            None => match nodecraft_nodes::Registry::builtins() {
                Ok(kinds) => Arc::clone(self.live.kinds.insert(Arc::new(kinds))),
                Err(error) => {
                    log::warn!("live recompute unavailable: {}", error.message);
                    return;
                }
            },
        };
        let (send, receive) = channel();
        let repaint = ctx.clone();
        let started = std::thread::Builder::new().name("nodecraft-live".into()).spawn(move || {
            let _ = send.send(live_actions::compute(job, &kinds));
            repaint.request_repaint();
        });
        match started {
            Ok(worker) => {
                self.live.running = Some(receive);
                self.live.worker = Some(worker);
            }
            Err(_) => log::warn!("live recompute: no worker thread"),
        }
    }

    /// Remembers what failed, shown once as a notice, so it isn't retried until its inputs or
    /// settings change.
    fn live_failures(&mut self, failed: &[(NodeId, String, Option<nodecraft_types::Blake3>)]) {
        for (node, reason, hash) in failed {
            let Some(hash) = hash else { continue };
            if self.live.failed.insert(*node, *hash) != Some(*hash) {
                self.notice = Some(reason.clone());
            }
        }
    }
}

/// A worker left running when the process exits can be inside a GPU driver (a 3D capture or a
/// shader), and a driver torn down under it crashes the process (#594). Closing waits for it,
/// for at most [`CLOSE_WAIT`]. (The browser has no worker threads.)
#[cfg(not(target_arch = "wasm32"))]
impl Drop for Live {
    fn drop(&mut self) {
        let Some(worker) = self.worker.take() else { return };
        let deadline = Instant::now() + CLOSE_WAIT;
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            log::warn!("live recompute still running at close");
        }
    }
}

#[cfg(test)]
mod tests;
