//! Unsaved work kept in the browser for recovery (#717). The session's checkpoint and journal
//! frames, which the desktop writes to a recovery file, go to the site's storage here, through a
//! [`Shelf`]: IndexedDB in the browser, a fake in tests. Each tab keeps its own slot and holds it
//! for as long as the page lives; at load, a slot no tab holds is work a closed or crashed tab
//! left, and the newest one holding work is offered back, as the desktop offers it (#408).
//!
//! The session writes synchronously and the shelf asynchronously, so the [`Journal`] queues
//! writes and the app hands them over a batch at a time. A failed write (usually a full quota)
//! is said once; frames then wait for the next checkpoint, which replaces the slot, so what is
//! kept is always a checkpoint and the frames that follow it. Keys never go here: they live in
//! the origin's key storage.
use super::NodeCraftApp;
use super::web_files::{PickFuture, poll};
use crate::theme::Tokens;
use nodecraft_engine::session::DocumentLineage;
use nodecraft_format::Snapshot;
use nodecraft_format::journal::MAX_JOURNAL_BYTES;
use nodecraft_types::{Error, ErrorKind};
use std::cell::RefCell;
use std::rc::Rc;
use std::task::Poll;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub(super) use browser::{IndexedDb, fresh_slot};

/// Slots looked at in one load, newest first.
const MAX_SCAN: usize = 8;

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
}

impl Stored {
    /// The journal as the desktop's recovery file would hold it: the checkpoint and the frames
    /// that follow it without a gap. True if frames were left out.
    fn journal(&self) -> Option<(Vec<u8>, bool)> {
        let mut bytes = self.checkpoint.clone()?;
        let mut cut = false;
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
    fn slots(&mut self) -> PickFuture<Vec<String>>;
    fn read(&mut self, slot: &str) -> PickFuture<Stored>;
    /// Applies `writes` in order, all or none.
    fn write(&mut self, writes: Vec<Write>) -> PickFuture<()>;
    /// Takes `slot` for this tab for as long as the page lives; false if another tab has it.
    fn claim(&mut self, slot: &str) -> PickFuture<bool>;
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// This tab's slot, as the session writes it; the writes wait here for the shelf.
pub(super) struct Journal {
    slot: String,
    /// The slot's size as the session sees it: the checkpoint and frames since.
    bytes: usize,
    seq: u64,
    pending: Vec<Write>,
    /// A write failed: frames wait for the next checkpoint.
    failed: bool,
    /// The browser keeps nothing for this tab.
    off: bool,
}

impl Journal {
    pub(super) fn new(slot: String) -> Self {
        Self { slot, bytes: 0, seq: 0, pending: Vec::new(), failed: false, off: false }
    }

    pub(super) fn checkpoint(&mut self, recovery: &[u8]) -> Result<(), Error> {
        if recovery.len() > MAX_JOURNAL_BYTES {
            return Err(invalid("recovery exceeds size limit"));
        }
        self.bytes = recovery.len();
        self.seq = 0;
        self.failed = false;
        if self.off {
            return Ok(());
        }
        // Whatever this slot still had waiting is replaced.
        let slot = self.slot.clone();
        self.pending.retain(|write| write.slot() != slot);
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(recovery.len()).map_err(|_| Error::new(ErrorKind::Internal, "not enough memory to keep unsaved work"))?;
        bytes.extend_from_slice(recovery);
        self.pending.push(Write::Checkpoint { slot, bytes });
        Ok(())
    }

    pub(super) fn append(&mut self, frame: &[u8]) -> Result<(), Error> {
        if self.bytes.saturating_add(frame.len()) > MAX_JOURNAL_BYTES {
            return Err(invalid("recovery file is full; save a checkpoint"));
        }
        self.bytes = self.bytes.saturating_add(frame.len());
        self.seq = self.seq.saturating_add(1);
        if self.off || self.failed {
            return Ok(());
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(frame.len()).map_err(|_| Error::new(ErrorKind::Internal, "not enough memory to keep unsaved work"))?;
        bytes.extend_from_slice(frame);
        self.pending.push(Write::Frame { slot: self.slot.clone(), seq: self.seq, bytes });
        Ok(())
    }

    /// The session closed cleanly: nothing of this tab's needs keeping.
    pub(super) fn discard(&mut self) {
        let slot = self.slot.clone();
        self.pending.retain(|write| write.slot() != slot);
        self.bytes = 0;
        self.seq = 0;
        if !self.off {
            self.pending.push(Write::Remove { slot });
        }
    }

    /// The checkpoint just made is unsaved work, kept until the next one.
    fn hold(&mut self) {
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

    /// The last batch didn't land. Until this slot's next checkpoint its frames would follow a
    /// checkpoint the shelf may not have, and removing another slot could lose the only copy
    /// of its work; both wait. What comes after a checkpoint already queued goes ahead.
    fn failed(&mut self) {
        let own = self.slot.clone();
        let restart = self.pending.iter().position(|write| matches!(write, Write::Checkpoint { slot, .. } if *slot == own));
        let keep = self.pending.split_off(restart.unwrap_or(self.pending.len()));
        self.pending.retain(|write| matches!(write, Write::Checkpoint { .. }));
        self.pending.extend(keep);
        self.failed = restart.is_none();
    }

    /// Nothing can be kept in this browser.
    fn disable(&mut self) {
        self.off = true;
        self.pending.clear();
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

/// Claims this tab's slot, then looks at the slots no tab holds, newest first: the first with
/// work is offered (and stays claimed by this tab), the ones without are removed.
async fn scan(shelf: Rc<RefCell<Box<dyn Shelf>>>, own: String) -> Result<Option<Offer>, String> {
    let claim = shelf.try_borrow_mut().map_err(|_| busy())?.claim(&own);
    if !claim.await? {
        return Err("another tab has this tab's recovery slot".into());
    }
    let listed = shelf.try_borrow_mut().map_err(|_| busy())?.slots();
    let mut slots = listed.await?;
    slots.retain(|slot| *slot != own);
    // Slot names start with when their tab opened.
    slots.sort();
    let mut empty = Vec::new();
    let mut offer = None;
    for slot in slots.into_iter().rev().take(MAX_SCAN) {
        let claim = shelf.try_borrow_mut().map_err(|_| busy())?.claim(&slot);
        if !claim.await? {
            // Another tab's, still open.
            continue;
        }
        let read = shelf.try_borrow_mut().map_err(|_| busy())?.read(&slot);
        match replay(&read.await?) {
            Some(found) => {
                offer = Some(Offer { slot, found });
                break;
            }
            None => empty.push(Write::Remove { slot }),
        }
    }
    if !empty.is_empty() {
        let removed = shelf.try_borrow_mut().map_err(|_| busy())?.write(empty);
        removed.await?;
    }
    Ok(offer)
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
    scan: Option<PickFuture<Option<Offer>>>,
    offer: Option<Offer>,
    writing: Option<PickFuture<()>>,
    /// A failed write was said; the next is said once one has landed again.
    told: bool,
    /// The recovered document, unsaved work until it is saved.
    recovered: Option<DocumentLineage>,
}

impl WebRecovery {
    /// Recovery over `shelf`, this tab's slot named `slot` (later tabs' names sort after it).
    pub(super) fn new(shelf: Box<dyn Shelf>, slot: String) -> Self {
        let shelf = Rc::new(RefCell::new(shelf));
        let scan: PickFuture<Option<Offer>> = Box::pin(scan(Rc::clone(&shelf), slot.clone()));
        Self { shelf, journal: Rc::new(RefCell::new(Journal::new(slot))), scan: Some(scan), offer: None, writing: None, told: false, recovered: None }
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
        if let Some(mut scan) = recovery.scan.take() {
            match poll(&mut scan, ctx) {
                Poll::Pending => recovery.scan = Some(scan),
                Poll::Ready(Ok(offer)) => recovery.offer = offer,
                Poll::Ready(Err(why)) => {
                    log::warn!("recovery: unavailable: {why}");
                    if let Ok(mut journal) = recovery.journal.try_borrow_mut() {
                        journal.disable();
                    }
                    self.notice = Some(format!("This browser can't keep unsaved work for recovery ({why}). Save the project to a file to keep it."));
                }
            }
        }
        if recovery.writing.is_none() {
            let writes = recovery.journal.try_borrow_mut().map(|mut journal| journal.take()).unwrap_or_default();
            if !writes.is_empty()
                && let Ok(mut shelf) = recovery.shelf.try_borrow_mut()
            {
                recovery.writing = Some(shelf.write(writes));
            }
        }
        if let Some(mut writing) = recovery.writing.take() {
            match poll(&mut writing, ctx) {
                Poll::Pending => recovery.writing = Some(writing),
                Poll::Ready(Ok(())) => {
                    recovery.told = false;
                    // More may have come meanwhile.
                    ctx.request_repaint();
                }
                Poll::Ready(Err(why)) => {
                    log::warn!("recovery: a write failed: {why}");
                    if let Ok(mut journal) = recovery.journal.try_borrow_mut() {
                        journal.failed();
                    }
                    if !std::mem::replace(&mut recovery.told, true) {
                        self.notice = Some(format!(
                            "Unsaved work isn't being kept for recovery: {why}. Save the project to a file to keep it, or free up this site's storage."
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
        // Keys, text, scrolling and drops this frame were the question's; none may edit the
        // document behind it.
        ctx.input_mut(|i| {
            i.events.clear();
            i.raw.events.clear();
            i.raw.dropped_files.clear();
            i.keys_down.clear();
            i.modifiers = egui::Modifiers::NONE;
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        });
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
