use super::*;
use crate::app::web_files::{Detached, WebFiles};
use serde_json::json;
use std::collections::{BTreeMap, VecDeque};

/// The site's storage, shared by every tab of the origin.
#[derive(Default)]
struct Disk {
    slots: BTreeMap<String, Stored>,
    /// Which tab holds each slot's lock.
    locks: BTreeMap<String, u32>,
    /// Answers for the next writes; none is a success.
    failures: VecDeque<String>,
    /// No Web Locks in this browser.
    no_locks: bool,
    inventories: usize,
    removals: Vec<Vec<String>>,
}

/// One tab's view of the [`Disk`]; its locks go when it does.
struct FakeShelf {
    disk: Rc<RefCell<Disk>>,
    tab: u32,
}

impl Drop for FakeShelf {
    fn drop(&mut self) {
        self.disk.borrow_mut().locks.retain(|_, tab| *tab != self.tab);
    }
}

fn ready<T: 'static>(answer: Result<T, String>) -> PickFuture<T> {
    Box::pin(std::future::ready(answer))
}

impl Shelf for FakeShelf {
    fn slots(&mut self) -> PickFuture<Inventory> {
        let mut disk = self.disk.borrow_mut();
        disk.inventories += 1;
        ready(Ok(disk
            .slots
            .iter()
            .map(|(slot, stored)| {
                let bytes = stored.frames.iter().fold(stored.checkpoint.as_ref().map_or(0, Vec::len), |sum, (_, frame)| sum.saturating_add(frame.len()));
                (
                    slot.clone(),
                    Slot {
                        bytes: bytes.saturating_add(usize::from(stored.held)) as u64,
                        hold_bytes: stored.held.then_some(1),
                        dirty: stored.held || !stored.frames.is_empty(),
                        checkpoint: stored.checkpoint.is_some(),
                    },
                )
            })
            .collect()))
    }
    fn read(&mut self, slot: &str) -> PickFuture<Stored> {
        ready(Ok(self.disk.borrow().slots.get(slot).cloned().unwrap_or_default()))
    }
    fn write(&mut self, writes: Vec<Write>) -> PickFuture<()> {
        let mut disk = self.disk.borrow_mut();
        if let Some(why) = disk.failures.pop_front() {
            return ready(Err(why));
        }
        let removed: Vec<_> = writes.iter().filter_map(|write| if let Write::Remove { slot } = write { Some(slot.clone()) } else { None }).collect();
        if !removed.is_empty() {
            disk.removals.push(removed);
        }
        for write in writes {
            match write {
                Write::Checkpoint { slot, bytes } => {
                    disk.slots.insert(slot, Stored { checkpoint: Some(bytes), ..Stored::default() });
                }
                Write::Frame { slot, seq, bytes } => disk.slots.entry(slot).or_default().frames.push((seq, bytes)),
                Write::Hold { slot } => disk.slots.entry(slot).or_default().held = true,
                Write::Remove { slot } => {
                    disk.slots.remove(&slot);
                }
            }
        }
        ready(Ok(()))
    }
    fn claim(&mut self, slot: &str) -> PickFuture<bool> {
        let mut disk = self.disk.borrow_mut();
        if disk.no_locks {
            return ready(Err("this browser can't tell its tabs apart (no Web Locks)".into()));
        }
        let holder = *disk.locks.entry(slot.to_owned()).or_insert(self.tab);
        ready(Ok(holder == self.tab))
    }
    fn release(&mut self, slot: &str) {
        let mut disk = self.disk.borrow_mut();
        if disk.locks.get(slot) == Some(&self.tab) {
            disk.locks.remove(slot);
        }
    }
}

/// A tab opening on `disk`; later tabs' slots sort after earlier ones'.
fn tab(disk: &Rc<RefCell<Disk>>, n: u32) -> NodeCraftApp {
    tab_with_budget(disk, n, MAX_RECOVERY_BYTES)
}

fn tab_with_budget(disk: &Rc<RefCell<Disk>>, n: u32, budget: usize) -> NodeCraftApp {
    let shelf = FakeShelf { disk: Rc::clone(disk), tab: n };
    let recovery = if budget == MAX_RECOVERY_BYTES {
        WebRecovery::new(Box::new(shelf), format!("{n:013x}-00000000"))
    } else {
        WebRecovery::with_budget(Box::new(shelf), format!("{n:013x}-00000000"), budget)
    };
    NodeCraftApp::with_web_files(nodecraft_engine::commands::Services::default(), WebFiles::new(Box::new(Detached)), Some(recovery)).unwrap()
}

fn slot(n: u32) -> String {
    format!("{n:013x}-00000000")
}

fn frame(app: &mut NodeCraftApp, ctx: &egui::Context) {
    frame_at(app, ctx, 0.0);
}

/// A frame `time` seconds into the page's life.
fn frame_at(app: &mut NodeCraftApp, ctx: &egui::Context, time: f64) {
    let input = egui::RawInput { time: Some(time), ..egui::RawInput::default() };
    let mut output = ctx.run_ui(input, |ui| app.web_recovery_frame(ui.ctx()));
    output.textures_delta.clear();
}

fn add_text(app: &mut NodeCraftApp, n: u8) {
    // Clear of the starter project's own ids.
    let node = nodecraft_graph::Node::new(nodecraft_types::NodeId::from_bytes([0x40 + n; 16]), "source.text");
    app.session.dispatch("node.add", &json!({"canvas": app.canvas, "node": node})).unwrap();
}

fn offer(app: &NodeCraftApp) -> Option<&Offer> {
    app.web_recovery.as_ref().and_then(|recovery| recovery.offer.as_ref())
}

/// A tab that made two edits, then went without closing.
fn crashed_tab(disk: &Rc<RefCell<Disk>>, ctx: &egui::Context) -> nodecraft_graph::Project {
    let mut app = tab(disk, 1);
    frame(&mut app, ctx);
    add_text(&mut app, 1);
    frame(&mut app, ctx);
    add_text(&mut app, 2);
    frame(&mut app, ctx);
    app.session.document().clone()
}

#[test]
fn a_reload_offers_the_work_and_recover_restores_it() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let edited = crashed_tab(&disk, &ctx);
    assert_eq!(disk.borrow().slots[&slot(1)].frames.len(), 2, "each edit reached the site's storage");

    let mut app = tab(&disk, 2);
    assert!(app.web_recovery_owns_input(), "nothing edits until the scan is done");
    frame(&mut app, &ctx);
    let found = offer(&app).unwrap().found.as_ref().unwrap();
    assert_eq!((found.changes, found.damaged), (2, false));
    assert!(app.web_recovery_owns_input(), "the question owns input");
    assert!(!app.web_unsaved(), "the starter project isn't work yet");

    app.decide_web_recovery(Choice::Recover);
    assert_eq!(app.session.document(), &edited);
    assert!(!app.web_recovery_owns_input());
    assert!(app.notice.as_deref().unwrap().starts_with("Recovered"));
    assert!(app.web_unsaved(), "the recovered work is unsaved, so leaving asks");
    frame(&mut app, &ctx);
    {
        let disk = disk.borrow();
        assert!(!disk.slots.contains_key(&slot(1)), "the old slot went with the recovery");
        let own = &disk.slots[&slot(2)];
        assert!(own.held && own.frames.is_empty(), "the recovered work is kept in this tab's slot");
    }

    // This tab goes too, before saving: the work is offered again, though no edit followed.
    drop(app);
    let mut app = tab(&disk, 3);
    frame(&mut app, &ctx);
    let found = offer(&app).unwrap().found.as_ref().unwrap();
    assert_eq!(found.changes, 0);
    app.decide_web_recovery(Choice::Recover);
    assert_eq!(app.session.document(), &edited);
}

#[test]
fn discard_removes_the_work() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let _ = crashed_tab(&disk, &ctx);
    let mut app = tab(&disk, 2);
    frame(&mut app, &ctx);
    let starter = app.session.document().clone();
    app.decide_web_recovery(Choice::Discard);
    frame(&mut app, &ctx);
    assert_eq!(app.session.document(), &starter);
    assert!(!disk.borrow().slots.contains_key(&slot(1)));
    drop(app);
    let mut app = tab(&disk, 3);
    frame(&mut app, &ctx);
    assert!(offer(&app).is_none());
}

#[test]
fn another_open_tab_keeps_its_work() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let mut first = tab(&disk, 1);
    frame(&mut first, &ctx);
    add_text(&mut first, 1);
    frame(&mut first, &ctx);

    let mut second = tab(&disk, 2);
    frame(&mut second, &ctx);
    assert!(offer(&second).is_none(), "the first tab is still open");
    assert!(!second.web_recovery_owns_input());
    assert_eq!(disk.borrow().slots[&slot(1)].frames.len(), 1, "and its slot is left alone");
}

#[test]
fn slots_without_work_are_removed() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let mut first = tab(&disk, 1);
    frame(&mut first, &ctx);
    drop(first);
    assert!(disk.borrow().slots.contains_key(&slot(1)));
    let mut second = tab(&disk, 2);
    frame(&mut second, &ctx);
    assert!(offer(&second).is_none(), "an untouched starter project isn't offered");
    assert!(!disk.borrow().slots.contains_key(&slot(1)));
}

#[test]
fn a_clean_close_removes_the_slot() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let mut app = tab(&disk, 1);
    frame(&mut app, &ctx);
    app.web_journal(Journal::discard);
    frame(&mut app, &ctx);
    assert!(disk.borrow().slots.is_empty());
}

#[test]
fn a_failed_write_is_said_once_and_leaves_an_older_consistent_state() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let mut app = tab(&disk, 1);
    frame(&mut app, &ctx);
    add_text(&mut app, 1);
    frame(&mut app, &ctx);
    disk.borrow_mut().failures.push_back("this site's browser storage is full".into());
    add_text(&mut app, 2);
    frame(&mut app, &ctx);
    let notice = app.notice.take().unwrap();
    assert!(notice.contains("this site's browser storage is full") && notice.contains("keeps trying"), "{notice}");
    // Later frames would follow a gap: they wait for the slot to be written whole.
    add_text(&mut app, 3);
    frame(&mut app, &ctx);
    assert_eq!(app.notice, None, "said once");
    assert_eq!(disk.borrow().slots[&slot(1)].frames.len(), 1);

    // The next checkpoint (here: a save's) starts the slot over, and frames follow it.
    app.session.dispatch("file.save_as", &json!({"location": "1/forest.ncraft"})).unwrap();
    add_text(&mut app, 4);
    frame(&mut app, &ctx);
    assert_eq!(disk.borrow().slots[&slot(1)].frames.len(), 1);

    disk.borrow_mut().failures.push_back("this site's browser storage is full".into());
    add_text(&mut app, 5);
    frame(&mut app, &ctx);
    assert!(app.notice.is_some(), "a new failure, after writes landed again, is said again");
}

#[test]
fn the_cap_is_the_desktop_journal_cap() {
    let mut journal = Journal::new(slot(1));
    assert!(journal.checkpoint(&vec![0; MAX_JOURNAL_BYTES + 1]).is_err());
    journal.checkpoint(&[0; 8]).unwrap();
    assert!(journal.append(&vec![0; MAX_JOURNAL_BYTES - 8 + 1]).is_err());
    journal.append(&[0; 8]).unwrap();
    assert_eq!(journal.take().len(), 2);
}

#[test]
fn a_failure_drops_what_was_queued_but_keeps_its_removals() {
    let mut journal = Journal::new(slot(1));
    journal.checkpoint(b"fresh").unwrap();
    journal.append(b"after").unwrap();
    journal.remove(slot(8));
    journal.failed(vec![slot(9)]);
    journal.append(b"next").unwrap();
    assert!(journal.take().is_empty(), "frames would follow a gap");
    journal.rebuild().unwrap();
    let writes = journal.take();
    let [Write::Checkpoint { slot: own, bytes }, Write::Remove { slot: first }, Write::Remove { slot: second }] = writes.as_slice() else {
        panic!("{writes:?}");
    };
    assert_eq!((own, bytes.as_slice()), (&slot(1), b"freshafternext".as_slice()), "the slot again, whole");
    assert_eq!((first, second), (&slot(8), &slot(9)));
    journal.rebuild().unwrap();
    assert!(journal.take().is_empty(), "once written whole, nothing more to rebuild");
}

#[test]
fn a_failed_write_is_tried_again_whole_and_then_recovers() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let edited = {
        let mut app = tab(&disk, 1);
        frame(&mut app, &ctx);
        add_text(&mut app, 1);
        frame(&mut app, &ctx);
        disk.borrow_mut().failures.push_back("this site's browser storage is full".into());
        add_text(&mut app, 2);
        frame_at(&mut app, &ctx, 1.0);
        add_text(&mut app, 3);
        frame_at(&mut app, &ctx, 5.0);
        assert_eq!(disk.borrow().slots[&slot(1)].frames.len(), 1, "too soon to try again");
        frame_at(&mut app, &ctx, 1.0 + RETRY_SECS);
        add_text(&mut app, 4);
        frame_at(&mut app, &ctx, 2.0 + RETRY_SECS);
        assert_eq!(disk.borrow().slots[&slot(1)].frames.len(), 1, "written whole, and frames follow it again");
        app.session.document().clone()
    };
    let mut app = tab(&disk, 2);
    frame(&mut app, &ctx);
    assert!(!offer(&app).unwrap().found.as_ref().unwrap().damaged);
    app.decide_web_recovery(Choice::Recover);
    assert_eq!(app.session.document(), &edited, "every edit, the failed ones included");
}

#[test]
fn a_removal_that_failed_is_tried_again() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let _ = crashed_tab(&disk, &ctx);
    let mut app = tab(&disk, 2);
    frame(&mut app, &ctx);
    disk.borrow_mut().failures.push_back("the browser couldn't write to this site's storage".into());
    app.decide_web_recovery(Choice::Discard);
    frame_at(&mut app, &ctx, 1.0);
    assert!(disk.borrow().slots.contains_key(&slot(1)), "the removal failed");
    frame_at(&mut app, &ctx, 1.0 + RETRY_SECS);
    assert!(!disk.borrow().slots.contains_key(&slot(1)), "and was tried again");
}

#[test]
fn input_is_recoverys_while_it_asks() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let _ = crashed_tab(&disk, &ctx);
    let mut app = tab(&disk, 2);
    let key = egui::Event::Key { key: egui::Key::Delete, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE };
    let input = egui::RawInput { events: vec![key, egui::Event::Text("x".into())], ..egui::RawInput::default() };
    let mut left = None;
    let mut output = ctx.run_ui(input, |ui| {
        app.web_recovery_frame(ui.ctx());
        left = Some(ui.ctx().input(|i| i.events.len()));
    });
    output.textures_delta.clear();
    assert_eq!(left, Some(0), "nothing reaches the canvas behind the question");
}

#[test]
fn a_slot_cut_by_the_cap_is_damaged() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let _ = crashed_tab(&disk, &ctx);
    if let Some(stored) = disk.borrow_mut().slots.get_mut(&slot(1)) {
        stored.truncated = true;
    }
    let mut app = tab(&disk, 2);
    frame(&mut app, &ctx);
    let found = offer(&app).unwrap().found.as_ref().unwrap();
    assert_eq!((found.changes, found.damaged), (2, true));
}

#[test]
fn a_damaged_slot_offers_what_replays() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let _ = crashed_tab(&disk, &ctx);
    if let Some((_, last)) = disk.borrow_mut().slots.get_mut(&slot(1)).and_then(|stored| stored.frames.last_mut()) {
        last.truncate(last.len() / 2);
    }
    let mut app = tab(&disk, 2);
    frame(&mut app, &ctx);
    let found = offer(&app).unwrap().found.as_ref().unwrap();
    assert_eq!((found.changes, found.damaged), (1, true));
}

#[test]
fn an_unreadable_slot_can_be_kept_for_later() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    disk.borrow_mut().slots.insert(slot(1), Stored { checkpoint: Some(b"not a checkpoint".to_vec()), ..Stored::default() });
    let mut app = tab(&disk, 2);
    frame(&mut app, &ctx);
    assert!(offer(&app).unwrap().found.is_err());
    app.decide_web_recovery(Choice::Later);
    frame(&mut app, &ctx);
    assert!(disk.borrow().slots.contains_key(&slot(1)), "kept");
    drop(app);
    let mut app = tab(&disk, 3);
    frame(&mut app, &ctx);
    assert!(offer(&app).is_some(), "and asked about again");
}

#[test]
fn without_web_locks_nothing_is_kept_and_that_is_said() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    disk.borrow_mut().no_locks = true;
    let mut app = tab(&disk, 1);
    frame(&mut app, &ctx);
    assert!(app.notice.as_deref().unwrap().contains("can't keep unsaved work"));
    assert!(!app.web_recovery_owns_input());
    add_text(&mut app, 1);
    frame(&mut app, &ctx);
    assert!(disk.borrow().slots.is_empty());
}

#[test]
fn a_long_title_stays_one_short_line() {
    assert_eq!(quoted("  "), "an untitled project");
    assert_eq!(quoted("Forest\nwalk"), "“Forest walk”");
    assert_eq!(quoted(&"x".repeat(100)), format!("“{}…”", "x".repeat(80)));
}

fn stored_size(stored: &Stored) -> usize {
    stored.checkpoint.as_ref().map_or(0, Vec::len) + stored.frames.iter().map(|(_, bytes)| bytes.len()).sum::<usize>() + usize::from(stored.held)
}

#[test]
fn cleanup_reaches_clean_and_checkpointless_slots_beyond_the_offer_window() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    crashed_tab(&disk, &ctx);
    let mut clean = disk.borrow().slots[&slot(1)].clone();
    clean.frames.clear();
    for n in 2..=20 {
        disk.borrow_mut().slots.insert(slot(n), clean.clone());
    }
    disk.borrow_mut().slots.insert(slot(0), Stored { frames: vec![(1, b"orphan frame".to_vec())], ..Stored::default() });
    let mut app = tab(&disk, 30);
    frame(&mut app, &ctx);
    let disk = disk.borrow();
    assert_eq!(disk.removals.len(), 1, "one atomic cleanup batch, including entries past the offer window");
    assert_eq!(disk.removals.first().unwrap().len(), 20);
    assert_eq!(disk.slots.keys().cloned().collect::<Vec<_>>(), [slot(1), slot(30)], "recoverable work outside the window stays under budget");
    assert!(!disk.locks.contains_key(&slot(1)), "inventory does not reserve unoffered work");
}

#[test]
fn total_budget_prunes_oldest_unlocked_work_and_reports_the_count() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    crashed_tab(&disk, &ctx);
    let stored = disk.borrow().slots[&slot(1)].clone();
    let base = stored.checkpoint.as_ref().unwrap().len();
    let budget = 3 * stored_size(&stored) + base;
    for n in 2..=12 {
        disk.borrow_mut().slots.insert(slot(n), stored.clone());
    }
    let mut app = tab_with_budget(&disk, 20, budget);
    frame(&mut app, &ctx);
    assert_eq!(offer(&app).unwrap().slot, slot(12), "the newest recovery copy remains offered");
    assert!(app.notice.as_deref().unwrap().contains("Removed 9 old recovery copies"));
    let disk = disk.borrow();
    assert_eq!(disk.slots.keys().cloned().collect::<Vec<_>>(), [slot(10), slot(11), slot(12), slot(20)]);
    assert!(disk.slots.values().map(stored_size).sum::<usize>() <= budget);
    assert!(!disk.locks.contains_key(&slot(10)) && !disk.locks.contains_key(&slot(11)), "other pages may offer the retained copies");
    assert_eq!(disk.locks.get(&slot(12)), Some(&20), "the offered copy stays protected");
    assert_eq!(disk.removals.len(), 1, "all old entries, including those outside MAX_SCAN, are removed together");
}

#[test]
fn an_old_open_tab_is_never_pruned_even_if_it_exceeds_the_budget() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    crashed_tab(&disk, &ctx);
    let stored = disk.borrow().slots[&slot(1)].clone();
    for n in 2..=4 {
        disk.borrow_mut().slots.insert(slot(n), stored.clone());
    }
    disk.borrow_mut().locks.insert(slot(1), 77);
    let mut app = tab_with_budget(&disk, 20, 0);
    frame(&mut app, &ctx);
    let disk = disk.borrow();
    assert_eq!(disk.slots.keys().cloned().collect::<Vec<_>>(), [slot(1), slot(4), slot(20)], "open tabs and the offered copy are protected even over budget");
    assert_eq!(disk.locks.get(&slot(1)), Some(&77));
    assert_eq!(stored_size(&disk.slots[&slot(1)]), stored_size(&stored));
}

#[test]
fn growing_work_prunes_old_copies_in_its_write_batch() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    crashed_tab(&disk, &ctx);
    let stored = disk.borrow().slots[&slot(1)].clone();
    disk.borrow_mut().slots.insert(slot(2), stored.clone());
    let base = stored.checkpoint.as_ref().unwrap().len();
    let inventories = disk.borrow().inventories;
    let mut app = tab_with_budget(&disk, 20, 2 * stored_size(&stored) + base);
    frame(&mut app, &ctx);
    assert_eq!(disk.borrow().inventories, inventories + 1, "small writes reuse the complete startup inventory");
    app.decide_web_recovery(Choice::Later);
    add_text(&mut app, 3);
    frame(&mut app, &ctx);
    assert!(app.notice.as_deref().unwrap().contains("Removed 1 old recovery copy"));
    let disk = disk.borrow();
    assert!(!disk.slots.contains_key(&slot(1)), "oldest abandoned work makes room for the growing tab");
    assert!(disk.slots.contains_key(&slot(2)), "the previously offered copy is still claimed until the page exits");
    assert_eq!(disk.slots[&slot(20)].frames.len(), 1, "the new frame landed with the cleanup");
    assert_eq!(disk.inventories, inventories + 2, "refresh only at the budget boundary");
}

#[test]
fn failed_budget_pruning_keeps_copies_and_releases_temporary_claims() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    crashed_tab(&disk, &ctx);
    let stored = disk.borrow().slots[&slot(1)].clone();
    disk.borrow_mut().slots.insert(slot(2), stored.clone());
    let base = stored.checkpoint.as_ref().unwrap().len();
    let mut app = tab_with_budget(&disk, 20, 2 * stored_size(&stored) + base);
    frame(&mut app, &ctx);
    app.decide_web_recovery(Choice::Later);
    disk.borrow_mut().failures.push_back("this site's browser storage is full".into());
    add_text(&mut app, 3);
    frame_at(&mut app, &ctx, 1.0);
    assert!(app.notice.as_deref().unwrap().contains("keeps trying"));
    {
        let disk = disk.borrow();
        assert!(disk.slots.contains_key(&slot(1)), "pruning and publication both rolled back");
        assert!(disk.slots[&slot(20)].frames.is_empty());
        assert!(!disk.locks.contains_key(&slot(1)), "a failed transaction does not reserve its prune candidates");
    }
    frame_at(&mut app, &ctx, 1.0 + RETRY_SECS);
    assert!(!disk.borrow().slots.contains_key(&slot(1)), "retry inventories storage again and prunes atomically");
    assert!(app.notice.as_deref().unwrap().contains("Removed 1 old recovery copy"));
}

#[test]
fn periodic_inventory_catches_another_tabs_growth_and_close_without_own_writes() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    let mut first = tab(&disk, 1);
    frame(&mut first, &ctx);
    add_text(&mut first, 1);
    frame(&mut first, &ctx);
    let stored = disk.borrow().slots[&slot(1)].clone();
    let base = stored.checkpoint.as_ref().unwrap().len();
    let mut second = tab_with_budget(&disk, 2, stored_size(&stored) + base);
    frame(&mut second, &ctx);
    assert!(offer(&second).is_none(), "the other tab was open during our initial inventory");
    add_text(&mut first, 2);
    frame_at(&mut first, &ctx, 1.0);
    drop(first);
    frame_at(&mut second, &ctx, INVENTORY_SECS - 1.0);
    assert!(disk.borrow().slots.contains_key(&slot(1)), "the bounded refresh interval has not elapsed");
    frame_at(&mut second, &ctx, INVENTORY_SECS);
    assert!(!disk.borrow().slots.contains_key(&slot(1)), "closed-tab growth is noticed even with no local writes or quota error");
    assert!(disk.borrow().slots[&slot(2)].frames.is_empty(), "this tab did not need to edit to trigger cleanup");
    assert!(second.notice.as_deref().unwrap().contains("Removed 1 old recovery copy"));
}

#[test]
fn a_scan_cleanup_failure_does_not_claim_other_tabs_recovery_copies() {
    let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
    crashed_tab(&disk, &ctx);
    let stored = disk.borrow().slots[&slot(1)].clone();
    disk.borrow_mut().slots.insert(slot(2), stored.clone());
    disk.borrow_mut().failures.push_back("storage unavailable".into());
    let mut app = tab_with_budget(&disk, 20, 0);
    frame(&mut app, &ctx);
    let disk = disk.borrow();
    assert!(disk.slots.contains_key(&slot(1)) && disk.slots.contains_key(&slot(2)));
    assert!(!disk.locks.contains_key(&slot(1)) && !disk.locks.contains_key(&slot(2)));
    assert!(app.notice.as_deref().unwrap().contains("can't keep unsaved work"));
}

#[test]
#[ignore = "manual public-safe #726 evidence; requires NODECRAFT_REVIEW_EVIDENCE"]
fn render_browser_recovery_budget_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    for pruning in [false, true] {
        let (disk, ctx) = (Rc::new(RefCell::new(Disk::default())), egui::Context::default());
        crashed_tab(&disk, &ctx);
        let stored = disk.borrow().slots[&slot(1)].clone();
        let base = stored.checkpoint.as_ref().unwrap().len();
        for n in 2..=12 {
            disk.borrow_mut().slots.insert(slot(n), stored.clone());
        }
        let budget = if pruning { 3 * stored_size(&stored) + base } else { MAX_RECOVERY_BYTES };
        let app = tab_with_budget(&disk, 20, budget);
        let mut h = egui_kittest::Harness::builder().wgpu().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(
            |ui, app: &mut NodeCraftApp| {
                // Native evidence drives the browser-only frame hook explicitly.
                app.web_recovery_frame(ui.ctx());
                app.show(ui);
            },
            app,
        );
        crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
        h.run();
        h.render().unwrap().save(folder.join(if pruning { "after-budget-cleanup.png" } else { "before-budget-cleanup.png" })).unwrap();
    }
}
