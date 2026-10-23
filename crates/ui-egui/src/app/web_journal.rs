//! Unsaved work kept in the browser for recovery (#717). The session's checkpoint and journal
//! frames, which the desktop writes to a recovery file, go to the site's storage here, through a
//! [`Shelf`]: IndexedDB in the browser, a fake in tests. Each tab keeps its own slot and holds it
//! for as long as the page lives; at load, a slot no tab holds is work a closed or crashed tab
//! left, and the newest one holding work is offered back, as the desktop offers it (#408).
//!
//! The session writes synchronously and the shelf asynchronously, so the [`Journal`] queues
//! writes and the app hands them over a batch at a time. A failed write (usually a full quota)
//! is said once. The journal keeps its own copy of the slot, so a little later the slot is
//! written again whole, as one checkpoint, and the frames follow it again. What the shelf holds
//! is always a checkpoint and the frames that follow it. Keys never go here: they live in the
//! origin's key storage.
use super::NodeCraftApp;
use super::web_files::{PickFuture, poll};
use crate::theme::Tokens;
use nodecraft_engine::session::DocumentLineage;
use nodecraft_format::Snapshot;
use nodecraft_format::journal::MAX_JOURNAL_BYTES;
use nodecraft_types::{Error, ErrorKind};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::task::Poll;

#[cfg(target_arch = "wasm32")]
mod browser;
mod maintenance;
#[cfg(target_arch = "wasm32")]
pub(super) use browser::{IndexedDb, fresh_slot};
use maintenance::{Scanned, Written};

/// Slots looked at in one load, newest first.
const MAX_SCAN: usize = 8;
/// Recovery payload budget across tabs. Live tabs and offered/deferred copies keep their locks even
/// when their combined work exceeds it; only abandoned copies may be pruned.
const MAX_RECOVERY_BYTES: usize = 2 * MAX_JOURNAL_BYTES;
/// Refresh independently of this tab's growth, without repeatedly cloning all idle payloads.
const INVENTORY_SECS: f64 = 300.0;
/// Seconds after a failed write before the slot is written again whole.
const RETRY_SECS: f64 = 15.0;

/// One write to the shelf.
#[derive(Debug, PartialEq)]
pub(super) enum Write {
    /// Replaces everything in `slot` with this checkpoint.
    Checkpoint {
        slot: String,
        bytes: Vec<u8>,
    },
    /// Frame `seq` (from 1) after the slot's checkpoint.
    Frame {
        slot: String,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// The slot's checkpoint is itself unsaved work: recovered, and not saved since.
    Hold {
        slot: String,
    },
    Remove {
        slot: String,
    },
}

impl Write {
    fn slot(&self) -> &str {
        match self {
            Self::Checkpoint { slot, .. } | Self::Frame { slot, .. } | Self::Hold { slot } | Self::Remove { slot } => slot,
        }
    }
}

/// A slot as the shelf holds it.
#[derive(Clone, Debug, Default)]
pub(super) struct Stored {
    pub checkpoint: Option<Vec<u8>>,
    /// In sequence order.
    pub frames: Vec<(u64, Vec<u8>)>,
    pub held: bool,
    /// The shelf left part of it out (more than a journal holds).
    pub truncated: bool,
}

/// Inventory counts payload bytes without copying journals or decoding projects.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Slot {
    bytes: u64,
    /// A hold overwrites one key; remember its payload size rather than charging it twice.
    hold_bytes: Option<u64>,
    checkpoint: bool,
    /// A frame or hold exists. A checkpoint alone may be clean, even outside the offer window.
    dirty: bool,
}

type Inventory = BTreeMap<String, Slot>;

impl Stored {
    /// The journal as the desktop's recovery file would hold it: the checkpoint and the frames
    /// that follow it without a gap. True if frames were left out.
    fn journal(&self) -> Option<(Vec<u8>, bool)> {
        let mut bytes = self.checkpoint.clone()?;
        let mut cut = self.truncated;
        for (want, (seq, frame)) in (1u64..).zip(&self.frames) {
            if *seq != want || bytes.len().saturating_add(frame.len()) > MAX_JOURNAL_BYTES {
                cut = true;
                break;
            }
            bytes.extend_from_slice(frame);
        }
        Some((bytes, cut))
    }
}

/// The site's storage for recovery slots. Futures own what they need, so none borrows the shelf.
pub(super) trait Shelf {
    /// The slots stored, in no order.
    fn slots(&mut self) -> PickFuture<Inventory>;
    fn read(&mut self, slot: &str) -> PickFuture<Stored>;
    /// Applies `writes` in order, all or none.
    fn write(&mut self, writes: Vec<Write>) -> PickFuture<()>;
    /// Acquires a new claim; false if any tab, including this shelf, already holds it.
    /// A true result grants authority to release this newly acquired claim.
    fn claim(&mut self, slot: &str) -> PickFuture<bool>;
    /// Releases a claim this shelf owns. Inventory must not reserve abandoned copies forever.
    fn release(&mut self, slot: &str);
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// This tab's slot, as the session writes it; the writes wait here for the shelf.
pub(super) struct Journal {
    slot: String,
    /// The slot as the session sees it: the checkpoint and the frames since, as the desktop's
    /// recovery file holds them. Written again whole after a failure.
    kept: Vec<u8>,
    seq: u64,
    /// The slot's checkpoint is unsaved work (see [`Write::Hold`]).
    held: bool,
    pending: Vec<Write>,
    /// Other slots to remove once this slot is written whole: a removal whose batch failed,
    /// which may be the work this slot now holds.
    deferred: Vec<String>,
    /// A write failed: until the slot is written whole again, frames would follow a gap.
    failed: bool,
    /// The browser keeps nothing for this tab.
    off: bool,
}

fn copy(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len()).map_err(|_| Error::new(ErrorKind::Internal, "not enough memory to keep unsaved work"))?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

impl Journal {
    pub(super) fn new(slot: String) -> Self {
        Self { slot, kept: Vec::new(), seq: 0, held: false, pending: Vec::new(), deferred: Vec::new(), failed: false, off: false }
    }

    pub(super) fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
        if recovery.len() > MAX_JOURNAL_BYTES {
            return Err(invalid("recovery exceeds size limit"));
        }
        self.kept = copy(recovery)?;
        self.seq = 0;
        self.held = false;
        self.failed = false;
        if self.off {
            return Ok(());
        }
        // Whatever this slot still had waiting is replaced.
        let slot = self.slot.clone();
        self.pending.retain(|write| write.slot() != slot);
        let bytes = copy(recovery)?;
        self.pending.push(Write::Checkpoint { slot, bytes });
        self.release_deferred();
        Ok(())
    }

    pub(super) fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
        if self.kept.len().saturating_add(frame.len()) > MAX_JOURNAL_BYTES {
            return Err(invalid("recovery file is full; save a checkpoint"));
        }
        self.kept.try_reserve(frame.len()).map_err(|_| Error::new(ErrorKind::Internal, "not enough memory to keep unsaved work"))?;
        self.kept.extend_from_slice(frame);
        self.seq = self.seq.saturating_add(1);
        if self.off || self.failed {
            return Ok(());
        }
        let bytes = copy(frame)?;
        self.pending.push(Write::Frame { slot: self.slot.clone(), seq: self.seq, bytes });
        Ok(())
    }

    /// The session closed cleanly: nothing of this tab's needs keeping.
    pub(super) fn discard(&mut self) {
        let slot = self.slot.clone();
        self.pending.retain(|write| write.slot() != slot);
        self.kept = Vec::new();
        self.seq = 0;
        self.held = false;
        self.failed = false;
        if !self.off {
            self.pending.push(Write::Remove { slot });
            self.release_deferred();
        }
    }

    /// The checkpoint just made is unsaved work, kept until the next one.
    fn hold(&mut self) {
        self.held = true;
        if !self.off {
            self.pending.push(Write::Hold { slot: self.slot.clone() });
        }
    }

    fn remove(&mut self, slot: String) {
        if !self.off {
            self.pending.push(Write::Remove { slot });
        }
    }

    fn take(&mut self) -> Vec<Write> {
        std::mem::take(&mut self.pending)
    }

    /// Deferred removals go in the batch just queued for this slot, so they land with it.
    fn release_deferred(&mut self) {
        let own = self.slot.clone();
        let empty = self.kept.is_empty();
        for slot in std::mem::take(&mut self.deferred) {
            // This slot's own removal only stands while it holds nothing.
            if slot != own || empty {
                self.pending.push(Write::Remove { slot });
            }
        }
    }

    /// A batch didn't land; `removed` are the slots it was removing. What is queued would follow
    /// it, so it goes too, but for removals: those wait for the slot to be written whole.
    fn failed(&mut self, removed: Vec<String>) {
        if self.off {
            return;
        }
        for write in self.pending.drain(..) {
            if let Write::Remove { slot } = write {
                self.deferred.push(slot);
            }
        }
        self.deferred.extend(removed);
        self.deferred.sort();
        self.deferred.dedup();
        self.failed = true;
    }

    fn is_failed(&self) -> bool {
        self.failed && !self.off
    }

    /// Writes the slot again whole after a failure: one checkpoint holding everything since, its
    /// hold, and the removals that waited for it, in one batch.
    fn rebuild(&mut self) -> Result<(), Error> {
        if !self.is_failed() {
            return Ok(());
        }
        let slot = self.slot.clone();
        // The shelf numbers frames from its checkpoint, and this is a new one.
        self.seq = 0;
        if !self.kept.is_empty() {
            self.pending.push(Write::Checkpoint { slot: slot.clone(), bytes: copy(&self.kept)? });
            if self.held {
                self.pending.push(Write::Hold { slot });
            }
        }
        self.failed = false;
        self.release_deferred();
        Ok(())
    }

    /// Nothing can be kept in this browser.
    fn disable(&mut self) {
        self.off = true;
        self.pending.clear();
        self.deferred.clear();
        self.kept = Vec::new();
    }
}

/// What a left slot held.
pub(super) struct Found {
    snapshot: Snapshot,
    title: String,
    /// Edits replayed after its checkpoint.
    changes: u64,
    /// Replay stopped before an incomplete or damaged record.
    damaged: bool,
}

/// The question put at load.
pub(super) struct Offer {
    slot: String,
    /// What it held, or why it can't be replayed.
    found: Result<Found, String>,
}

/// Whether `stored` holds work worth offering, and what.
fn replay(stored: &Stored) -> Option<Result<Found, String>> {
    let (bytes, cut) = stored.journal()?;
    match nodecraft_format::journal::recover(&bytes) {
        Ok(recovered) => {
            let damaged = cut || recovered.truncated_tail || recovered.damage.is_some();
            (recovered.applied > 0 || damaged || stored.held)
                .then(|| Ok(Found { title: recovered.snapshot.project.title().to_owned(), changes: recovered.applied, damaged, snapshot: recovered.snapshot }))
        }
        Err(error) => Some(Err(error.message)),
    }
}

fn busy() -> String {
    "the browser's storage is busy".into()
}

/// The person's choice.
enum Choice {
    Recover,
    Discard,
    /// Leave it for the next load.
    Later,
}

/// Recovery in the browser: the scan at load, the question, and the writes on their way.
pub(super) struct WebRecovery {
    shelf: Rc<RefCell<Box<dyn Shelf>>>,
    journal: Rc<RefCell<Journal>>,
    scan: Option<PickFuture<Scanned>>,
    offer: Option<Offer>,
    /// This tab's own and initially offered slots stay reserved even after Later or a failed
    /// Recover dismisses the question. At most two slots, held until this shelf drops.
    reserved: Vec<String>,
    /// The last cleanup suffix, so refreshing its count preserves other user guidance.
    prune_notice: Option<String>,
    writing: Option<PickFuture<Written>>,
    inventory: Option<Inventory>,
    inventory_at: f64,
    budget: usize,
    /// The slots the batch being written removes, to try again if it fails.
    removing: Vec<String>,
    /// When the slot is next written whole, after a failed write.
    retry_at: Option<f64>,
    /// A failed write was said; the next is said once one has landed again.
    told: bool,
    /// The recovered document, unsaved work until it is saved.
    recovered: Option<DocumentLineage>,
}

impl WebRecovery {
    /// Recovery over `shelf`, this tab's slot named `slot` (later tabs' names sort after it).
    pub(super) fn new(shelf: Box<dyn Shelf>, slot: String) -> Self {
        Self::with_budget(shelf, slot, MAX_RECOVERY_BYTES)
    }

    fn with_budget(shelf: Box<dyn Shelf>, slot: String, budget: usize) -> Self {
        let shelf = Rc::new(RefCell::new(shelf));
        let scan: PickFuture<Scanned> = Box::pin(maintenance::scan(Rc::clone(&shelf), slot.clone(), budget));
        Self {
            shelf,
            journal: Rc::new(RefCell::new(Journal::new(slot.clone()))),
            scan: Some(scan),
            offer: None,
            reserved: vec![slot],
            prune_notice: None,
            writing: None,
            inventory: None,
            inventory_at: 0.0,
            budget,
            removing: Vec::new(),
            retry_at: None,
            told: false,
            recovered: None,
        }
    }

    /// The journal the session's storage writes to.
    pub(super) fn journal(&self) -> Rc<RefCell<Journal>> {
        Rc::clone(&self.journal)
    }

    /// While the scan runs or the question is open, nothing may edit the document: the answer
    /// may replace it.
    pub(super) fn owns_input(&self) -> bool {
        self.scan.is_some() || self.offer.is_some()
    }

    fn note_pruned(&mut self, notice: &mut Option<String>, count: usize) {
        let next = maintenance::notice(count);
        let previous = self.prune_notice.replace(next.clone());
        let current = notice.take().unwrap_or_default();
        let suffix = previous.as_ref().map(|previous| format!("\n{previous}"));
        let retained =
            if previous.as_ref() == Some(&current) { "" } else { suffix.as_ref().and_then(|suffix| current.strip_suffix(suffix.as_str())).unwrap_or(&current) };
        *notice = Some(if retained.is_empty() { next } else { format!("{retained}\n{next}") });
    }
}

fn quoted(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return "an untitled project".to_owned();
    }
    // A title from stored bytes stays one readable line.
    let short: String = title.chars().take(80).map(|c| if c.is_control() { ' ' } else { c }).collect();
    if title.chars().nth(80).is_some() { format!("“{short}…”") } else { format!("“{short}”") }
}

impl NodeCraftApp {
    /// Every frame: moves the scan and the writes along, and asks the question while it is
    /// open. Input stays with it then (see [`WebRecovery::owns_input`]).
    pub(super) fn web_recovery_frame(&mut self, ctx: &egui::Context) {
        let Some(recovery) = &mut self.web_recovery else { return };
        let owned = recovery.owns_input();
        let now = ctx.input(|i| i.time);
        if let Some(mut scan) = recovery.scan.take() {
            match poll(&mut scan, ctx) {
                Poll::Pending => recovery.scan = Some(scan),
                Poll::Ready(Ok(scanned)) => {
                    if let Some(offer) = &scanned.offer {
                        recovery.reserved.push(offer.slot.clone());
                    }
                    recovery.offer = scanned.offer;
                    recovery.inventory = Some(scanned.inventory);
                    recovery.inventory_at = now + INVENTORY_SECS;
                    if scanned.pruned > 0 {
                        recovery.note_pruned(&mut self.notice, scanned.pruned);
                    }
                }
                Poll::Ready(Err(why)) => {
                    log::warn!("recovery: unavailable: {why}");
                    if let Ok(mut journal) = recovery.journal.try_borrow_mut() {
                        journal.disable();
                    }
                    self.notice = Some(format!("This browser can't keep unsaved work for recovery ({why}). Save the project to a file to keep it."));
                }
            }
        }
        if recovery.writing.is_none()
            && recovery.retry_at.is_some_and(|at| now >= at)
            && let Ok(mut journal) = recovery.journal.try_borrow_mut()
        {
            recovery.retry_at = None;
            if let Err(error) = journal.rebuild() {
                log::warn!("recovery: can't write the slot again: {}", error.message);
            }
        }
        if recovery.writing.is_none() && recovery.scan.is_none() {
            let enabled = recovery.journal.try_borrow().is_ok_and(|journal| !journal.off);
            let refresh = enabled && now >= recovery.inventory_at;
            if refresh {
                recovery.inventory = None;
                recovery.inventory_at = now + INVENTORY_SECS;
            }
            let writes = recovery.journal.try_borrow_mut().map(|mut journal| journal.take()).unwrap_or_default();
            if !writes.is_empty() || refresh {
                recovery.removing = writes.iter().filter_map(|write| if let Write::Remove { slot } = write { Some(slot.clone()) } else { None }).collect();
                recovery.writing = Some(Box::pin(maintenance::write(
                    Rc::clone(&recovery.shelf),
                    writes,
                    recovery.inventory.take(),
                    recovery.reserved.clone(),
                    recovery.budget,
                )));
            }
            if enabled {
                // A host-supplied non-finite clock must not reach Duration's panicking cast.
                let delay = recovery.inventory_at - now;
                let delay = if delay.is_finite() { delay.clamp(0.0, INVENTORY_SECS) } else { INVENTORY_SECS };
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(delay));
            }
        }
        if let Some(mut writing) = recovery.writing.take() {
            match poll(&mut writing, ctx) {
                Poll::Pending => recovery.writing = Some(writing),
                Poll::Ready(Ok(written)) => {
                    recovery.inventory = Some(written.inventory);
                    if written.pruned > 0 {
                        recovery.note_pruned(&mut self.notice, written.pruned);
                    }
                    recovery.removing.clear();
                    recovery.told = false;
                    // More may have come meanwhile.
                    ctx.request_repaint();
                }
                Poll::Ready(Err(why)) => {
                    log::warn!("recovery: a write failed: {why}");
                    if let Ok(mut journal) = recovery.journal.try_borrow_mut() {
                        journal.failed(std::mem::take(&mut recovery.removing));
                    }
                    recovery.retry_at = Some(now + RETRY_SECS);
                    ctx.request_repaint_after(std::time::Duration::from_secs_f64(RETRY_SECS));
                    if !std::mem::replace(&mut recovery.told, true) {
                        self.notice = Some(format!(
                            "Unsaved work isn't being kept for recovery: {why}. Save the project to a file to keep it, or free up this site's storage; NodeCraft keeps trying."
                        ));
                    }
                }
            }
        }
        if let Some(recovered) = &recovery.recovered
            && (!self.session.is_current_document(recovered) || self.session.has_saved_document(recovered))
        {
            recovery.recovered = None;
        }
        let t = *self.theme.tokens();
        self.web_recovery_dialog(ctx, &t);
        if owned {
            // Keys, text, scrolling and drops while the scan runs or the question is open (this
            // frame included, if it was just answered) are recovery's: none may edit the document
            // the answer may replace.
            ctx.input_mut(|i| {
                i.events.clear();
                i.raw.events.clear();
                i.raw.dropped_files.clear();
                i.keys_down.clear();
                i.modifiers = egui::Modifiers::NONE;
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            });
        }
    }

    /// Whether the open document is recovered work not saved since.
    pub(super) fn web_recovered_unsaved(&self) -> bool {
        self.web_recovery
            .as_ref()
            .and_then(|recovery| recovery.recovered.as_ref())
            .is_some_and(|lineage| self.session.is_current_document(lineage) && !self.session.has_saved_document(lineage))
    }

    pub(super) fn web_recovery_owns_input(&self) -> bool {
        self.web_recovery.as_ref().is_some_and(WebRecovery::owns_input)
    }

    fn web_recovery_dialog(&mut self, ctx: &egui::Context, t: &Tokens) {
        let Some(offer) = self.web_recovery.as_ref().and_then(|recovery| recovery.offer.as_ref()) else { return };
        let mut choice = None;
        egui::Modal::new(egui::Id::new("nodecraft.web_recovery")).show(ctx, |ui| {
            ui.set_max_width(t.dialog_width);
            ui.heading("Recover unsaved work?");
            match &offer.found {
                Ok(found) => {
                    let title = quoted(&found.title);
                    let what = match found.changes {
                        0 => format!("Unsaved work on {title} can be recovered."),
                        1 => format!("1 unsaved change to {title} can be recovered."),
                        n => format!("{n} unsaved changes to {title} can be recovered."),
                    };
                    ui.label(format!("NodeCraft didn't close cleanly in this browser last time. {what}"));
                    if found.damaged {
                        ui.label(
                            egui::RichText::new("Recovery stopped at an incomplete or damaged record. Only complete changes before that point are included.")
                                .color(t.danger),
                        );
                    }
                    ui.label(egui::RichText::new("Recover opens that work as an untitled, unsaved project. Discard deletes it.").color(t.text_muted));
                    ui.horizontal(|ui| {
                        if ui.button("Recover").clicked() {
                            choice = Some(Choice::Recover);
                        }
                        if ui.button("Discard").clicked() {
                            choice = Some(Choice::Discard);
                        }
                    });
                }
                Err(why) => {
                    ui.label(format!("NodeCraft didn't close cleanly in this browser last time, but its record of unsaved work can't be replayed ({why})."));
                    ui.label(egui::RichText::new("Keep it to be asked again next time, or discard it.").color(t.text_muted));
                    ui.horizontal(|ui| {
                        if ui.button("Keep").clicked() {
                            choice = Some(Choice::Later);
                        }
                        if ui.button("Discard").clicked() {
                            choice = Some(Choice::Discard);
                        }
                    });
                }
            }
        });
        if let Some(choice) = choice {
            self.decide_web_recovery(choice);
            ctx.request_repaint();
        }
    }

    fn decide_web_recovery(&mut self, choice: Choice) {
        let Some(offer) = self.web_recovery.as_mut().and_then(|recovery| recovery.offer.take()) else { return };
        match (choice, offer.found) {
            (Choice::Recover, Ok(found)) => {
                let title = quoted(&found.title);
                // The session checkpoints the recovered work into this tab's slot at once.
                match self.session.recover(found.snapshot, None) {
                    Ok(()) => {
                        let lineage = self.session.document_lineage();
                        self.web_journal(|journal| {
                            journal.hold();
                            // In the same batch as that checkpoint: both land, or neither.
                            journal.remove(offer.slot);
                        });
                        if let Some(recovery) = &mut self.web_recovery {
                            recovery.recovered = Some(lineage);
                        }
                        self.after_file_open();
                        self.notice = Some(format!("Recovered {title} as an untitled project. Save it to a file to keep it."));
                    }
                    Err(error) => self.notice = Some(format!("That work couldn't be recovered ({}). It is offered again next time.", error.message)),
                }
            }
            (Choice::Discard, _) => {
                self.web_journal(|journal| journal.remove(offer.slot));
                self.notice = Some("The unsaved work was discarded.".into());
            }
            (Choice::Recover | Choice::Later, _) => {}
        }
    }

    fn web_journal(&self, f: impl FnOnce(&mut Journal)) {
        if let Some(recovery) = &self.web_recovery
            && let Ok(mut journal) = recovery.journal.try_borrow_mut()
        {
            f(&mut journal);
        }
    }
}

#[cfg(test)]
mod tests;
