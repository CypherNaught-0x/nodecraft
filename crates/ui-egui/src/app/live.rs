//! Live recompute in the app (#88): after an edit, stale action nodes on the shown canvas are
//! recomputed on a worker thread (in the frame, in the browser) and their results recorded into
//! the edit's undo step (`engine::live_actions`). At most one recompute runs at a time; a round
//! that finishes already out of date is dropped and the next one starts from the current
//! document, so cards follow edits at the speed the work allows.
//!
//! A slider being dragged isn't in the document yet. While it drags, the same worker computes a
//! preview from its value on scaled-down images, and the cards it changes show that preview's
//! results; nothing is recorded. Releasing the slider records the value, and the full-size round
//! that follows replaces the preview.
use super::NodeCraftApp;
use nodecraft_engine::live_actions::{self, Draft, LiveJob, LiveOutcome, LivePreview};
use nodecraft_types::{CanvasId, Error, NodeId};
use std::collections::BTreeMap;
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
    #[cfg(not(target_arch = "wasm32"))]
    worker: Option<std::thread::JoinHandle<()>>,
    /// The canvas and revision last looked at.
    checked: Option<(CanvasId, u64)>,
    /// Nodes that failed, with the input hash they failed with: not retried (or reported
    /// again) until their inputs or settings change.
    failed: BTreeMap<NodeId, nodecraft_types::Blake3>,
    /// The canvas as it was when there was nothing to recompute: moving its nodes changes the
    /// revision but not that (#549).
    settled: Option<live_actions::Settled>,
    /// The drag preview the cards show, until the full-size round lands or the drag ends.
    preview: Option<LivePreview>,
    /// The draft last previewed, on which canvas and revision: not previewed again until it moves.
    previewed: Option<(CanvasId, u64, Draft)>,
    /// The preview images each card last showed in full, kept while the next ones decode.
    held: BTreeMap<NodeId, Vec<Vec<crate::preview::Preview>>>,
}

impl Live {
    /// The preview result shown for `node` on `canvas` instead of its own, while a slider drags
    /// (#88), with the store holding its images.
    pub(super) fn preview(&self, canvas: CanvasId, node: NodeId) -> Option<(&nodecraft_graph::Generation, &nodecraft_format::AssetStore)> {
        let preview = self.preview.as_ref().filter(|preview| preview.canvas == canvas)?;
        Some((preview.results.get(&node)?, &preview.staged))
    }

    /// `rows`, the preview images `node` shows, once they are all ready; until then the last
    /// ones that were, so a drag doesn't blank the card at every step.
    pub(super) fn steady(&mut self, node: NodeId, rows: Vec<Vec<crate::preview::Preview>>) -> Vec<Vec<crate::preview::Preview>> {
        if rows.iter().flatten().all(|image| matches!(image, crate::preview::Preview::Ready { .. })) {
            self.held.insert(node, rows.clone());
            return rows;
        }
        self.held.get(&node).cloned().unwrap_or(rows)
    }

    /// Stops showing a preview: the cards show their own results again.
    fn drop_preview(&mut self) {
        self.preview = None;
        self.previewed = None;
        self.held.clear();
    }
}

impl NodeCraftApp {
    /// Every frame: records a finished recompute, and starts the next one if the document
    /// changed. Repaints while work is running so its result shows as soon as it lands.
    pub(super) fn live_recompute(&mut self, ctx: &egui::Context) {
        if let Some(receiver) = &self.live.running {
            match receiver.try_recv() {
                Ok(Ok(outcome)) if outcome.is_preview() => {
                    self.live.running = None;
                    // One made for a document since changed, or a drag since ended, is dropped:
                    // the round that follows shows the truth.
                    let dragging = self.dragging(ctx).is_some();
                    if let Some(preview) = outcome.into_preview().filter(|p| dragging && p.base == self.session.revision()) {
                        self.live.preview = Some(preview);
                    }
                }
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
        self.live_round(ctx);
        let dragging = self.dragging(ctx);
        // A preview shows until the rounds after the drag have settled (its value recorded and
        // everything it feeds recomputed), or the drag ended with nothing to run.
        if dragging.is_none() && self.live.running.is_none() {
            self.live.drop_preview();
        }
        if self.live.preview.as_ref().is_some_and(|preview| preview.canvas != self.canvas) {
            self.live.drop_preview();
        }
        if self.live.running.is_some() {
            return;
        }
        let Some(draft) = dragging else { return };
        let at = (self.canvas, self.session.revision(), draft);
        if self.live.previewed.as_ref() == Some(&at) {
            return;
        }
        let job = live_actions::preview(&self.session, self.canvas, &at.2);
        self.live.previewed = Some(at);
        match job {
            Ok(Some(job)) => self.live_spawn(ctx, job),
            Ok(None) => {}
            Err(error) => log::debug!("no drag preview: {}", error.message),
        }
    }

    /// The slider dragged on the shown canvas, as a draft, if one is. A draft left behind by a
    /// slider released where it wasn't drawn (scrolled away, zoomed out) isn't a drag.
    fn dragging(&self, ctx: &egui::Context) -> Option<Draft> {
        if !ctx.input(|input| input.pointer.any_down()) {
            return None;
        }
        let canvas = self.session.document().canvases().get(&self.canvas)?;
        self.editors.iter().filter(|(id, _)| canvas.nodes().contains_key(id)).find_map(|(id, editor)| {
            let (name, value) = editor.dragging()?;
            let value = nodecraft_types::Number::new(value).ok()?;
            Some(Draft { node: *id, name: name.to_owned(), value: nodecraft_types::Value::Number(value) })
        })
    }

    /// Starts the round the document needs, if it changed since the last one.
    fn live_round(&mut self, ctx: &egui::Context) {
        if self.live.running.is_some() {
            return;
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
        self.live_spawn(ctx, job);
    }

    /// Computes `job` on a worker thread (in this frame in the browser, which has none).
    fn live_spawn(&mut self, ctx: &egui::Context, job: LiveJob) {
        let (send, receive) = channel();
        // The browser has no worker threads: there the round runs in this frame. A drag's
        // previews are small; a full-size render holds the frame once, as a click's work would.
        #[cfg(target_arch = "wasm32")]
        {
            let _ = send.send(live_actions::compute(job));
            self.live.running = Some(receive);
            ctx.request_repaint();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let repaint = ctx.clone();
            let started = std::thread::Builder::new().name("nodecraft-live".into()).spawn(move || {
                let _ = send.send(live_actions::compute(job));
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
