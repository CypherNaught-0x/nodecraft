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
    fn slots(&mut self) -> PickFuture<Vec<String>> {
        ready(Ok(self.disk.borrow().slots.keys().cloned().collect()))
    }
    fn read(&mut self, slot: &str) -> PickFuture<Stored> {
        ready(Ok(self.disk.borrow().slots.get(slot).cloned().unwrap_or_default()))
    }
    fn write(&mut self, writes: Vec<Write>) -> PickFuture<()> {
        let mut disk = self.disk.borrow_mut();
        if let Some(why) = disk.failures.pop_front() {
            return ready(Err(why));
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
}

/// A tab opening on `disk`; later tabs' slots sort after earlier ones'.
fn tab(disk: &Rc<RefCell<Disk>>, n: u32) -> NodeCraftApp {
    let shelf = FakeShelf { disk: Rc::clone(disk), tab: n };
    let recovery = WebRecovery::new(Box::new(shelf), format!("{n:013x}-00000000"));
    NodeCraftApp::with_web_files(nodecraft_engine::commands::Services::default(), WebFiles::new(Box::new(Detached)), Some(recovery)).unwrap()
}

fn slot(n: u32) -> String {
    format!("{n:013x}-00000000")
}

fn frame(app: &mut NodeCraftApp, ctx: &egui::Context) {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.web_recovery_frame(ui.ctx()));
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
    assert!(notice.contains("this site's browser storage is full"), "{notice}");
    // Later frames would follow a gap: they wait for the next checkpoint.
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
fn a_failure_keeps_a_queued_checkpoint_and_what_follows_it() {
    let mut journal = Journal::new(slot(1));
    journal.append(b"lost").unwrap();
    journal.remove(slot(9));
    journal.checkpoint(b"fresh").unwrap();
    journal.append(b"after").unwrap();
    journal.failed();
    let kept = journal.take();
    assert!(matches!(kept.as_slice(), [Write::Checkpoint { .. }, Write::Frame { seq: 1, .. }]), "{kept:?}");
    journal.failed();
    journal.append(b"next").unwrap();
    assert!(journal.take().is_empty(), "with no checkpoint queued, frames wait for the next");
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
