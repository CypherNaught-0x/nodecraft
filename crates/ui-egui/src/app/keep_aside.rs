//! Keeping an earlier session's journal aside at startup, off the UI thread (#1053).
//!
//! A journal left by a session that didn't close cleanly holds unsaved work, and this session's
//! first checkpoint replaces it (#408), so before the session may take the journal over its
//! bytes are copied to a `<journal>.unrecovered-<n>` slot. For a large project that is a full
//! replay and a multi-GiB copy, so it runs on a thread of its own while the window shows its
//! progress, and the session starts without storage ([`Session::attach_storage`] gives it the
//! storage once the copy is settled). Throughout, the journal's lock is held by the storage the
//! job owns, so a second window still finds the journal owned. The replay that decides whether
//! the journal holds work is also the one the recovery question is built from: the kept copy is
//! byte for byte that journal.
//!
//! "Decide later" (or quitting) cancels it. Nothing is lost either way: the copy is written
//! under a `.partial` name and renamed into its slot only once complete and on disk, the live
//! journal is only read, and a cancel or failure removes only the partial copy. The window
//! then runs without project storage, and the next launch keeps the journal aside again.
//!
//! [`Session::attach_storage`]: nodecraft_engine::Session::attach_storage
use super::{NodeCraftApp, recovery::RecoveryOffer, storage};
use nodecraft_format::{
    NativeStorage, ProjectStorage,
    asset_source::{StreamMetrics, TransferCancel},
    journal::Recovered,
    space::{SpaceInventory, SpaceProbe},
};
use nodecraft_types::{Error, ErrorKind};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

/// Work handed to a [`Spawn`].
pub(super) type Job = Box<dyn FnOnce() + Send>;
/// Runs a [`Job`] off the UI thread, or hands it back when it can't (it then runs inline).
pub(super) type Spawn = Arc<dyn Fn(Job) -> Result<(), Job> + Send + Sync>;

/// The desktop's spawner: a thread of its own.
pub(super) fn thread_spawn() -> Spawn {
    Arc::new(|job: Job| {
        // The job stays reachable here, so a thread that can't start hands it back.
        let slot = Arc::new(Mutex::new(Some(job)));
        let worker = Arc::clone(&slot);
        let started = std::thread::Builder::new().name("nodecraft-keep-aside".into()).spawn(move || {
            let job = worker.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(job) = job {
                job();
            }
        });
        match started {
            Ok(_) => Ok(()),
            Err(_) => slot.lock().unwrap_or_else(PoisonError::into_inner).take().map_or(Ok(()), Err),
        }
    })
}

/// Bytes copied per read, between cancel checks.
const COPY_CHUNK: usize = 1 << 20;
/// How long quitting waits for a cancelled keep-aside to stop (it stops at the next chunk).
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Where the keep-aside is, for its window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// Replaying the journal to see whether it holds work.
    Reading,
    /// Copying it aside.
    Copying { copied: u64, total: u64 },
    /// Looking for earlier copies still to offer.
    Earlier,
}

#[derive(Default)]
pub(super) struct Progress {
    step: AtomicU8,
    copied: AtomicU64,
    total: AtomicU64,
}
impl Progress {
    fn set(&self, step: Step) {
        let (code, copied, total) = match step {
            Step::Reading => (0, 0, 0),
            Step::Copying { copied, total } => (1, copied, total),
            Step::Earlier => (2, 0, 0),
        };
        self.copied.store(copied, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.step.store(code, Ordering::Release);
    }
    pub(super) fn step(&self) -> Step {
        match self.step.load(Ordering::Acquire) {
            1 => Step::Copying { copied: self.copied.load(Ordering::Relaxed), total: self.total.load(Ordering::Relaxed) },
            2 => Step::Earlier,
            _ => Step::Reading,
        }
    }
}

/// What the keep-aside found: where the journal was kept, if it held work, and the question to
/// ask about it (or about an earlier copy).
pub(super) struct Kept {
    pub kept: Option<PathBuf>,
    pub offer: Option<RecoveryOffer>,
    /// Looking for an earlier copy to offer was stopped: the live journal held nothing, so the
    /// session may still take it over, and the copies stay to be offered next time.
    pub skipped: bool,
    /// Why, when it wasn't a cancel (a copy's replay wouldn't fit): said with the notice.
    pub skipped_why: Option<String>,
}

/// Whether there is nothing to keep aside or offer: no journal (or an empty one) and no kept
/// copies. Metadata only, so it is cheap enough for the UI thread at startup.
pub(super) fn nothing_to_keep(recovery: &Path) -> bool {
    let empty = match std::fs::metadata(recovery) {
        Ok(metadata) => metadata.len() == 0,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    empty && storage::kept_journals(recovery).is_empty()
}

/// The live journal's work: `None` when it holds none (confirmed absent or empty, or a replay
/// showing only its checkpoint), else the replay, or why it can't be replayed (kept anyway: only
/// a person should decide unreadable bytes are worthless).
pub(super) fn live_work(length: std::io::Result<u64>, replay: impl FnOnce() -> Result<Recovered, Error>) -> Option<Result<Recovered, Error>> {
    match length {
        Ok(0) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        // A retained handle cannot make an inaccessible/replaced pathname empty.
        // Keep/refuse rather than silently checkpointing over uncertain work.
        Err(error) => return Some(Err(Error::new(ErrorKind::Conflict, format!("the recovery journal can't be inspected ({error})")))),
        Ok(_) => {}
    }
    match replay() {
        Ok(recovered) if recovered.applied > 0 || recovered.damage.is_some() || recovered.truncated_tail || recovered.legacy => Some(Ok(recovered)),
        Ok(_) => None,
        Err(error) => Some(Err(error)),
    }
}

/// Keeps the journal at `recovery` aside if it holds work, with its lock held by `storage`, and
/// works out the question to ask. With `earlier`, when it holds none, looks for an earlier copy
/// still to offer. Stops at `cancel` (an `Err` that [`TransferCancel::is_cancellation`] names),
/// leaving the journal as it was and no partial copy.
pub(super) fn keep(recovery: &Path, storage: &NativeStorage, cancel: &TransferCancel, progress: &Progress, earlier: bool) -> Result<Kept, Error> {
    progress.set(Step::Reading);
    let length = std::fs::metadata(recovery).map(|metadata| metadata.len());
    let probe = || SpaceProbe { recovery_free: storage.recovery_free(), ..SpaceProbe::default() };
    if let Ok(bytes) = &length
        && *bytes > 0
    {
        // The replay copies the journal's checkpoint into the private workspace.
        SpaceInventory::replay_journal(*bytes)?.check(&probe())?;
    }
    let work = live_work(length, || storage.recover_with(cancel, &StreamMetrics::default()));
    if let Some(Err(error)) = &work
        && TransferCancel::is_cancellation(error)
    {
        return Err(error.clone());
    }
    let Some(replayed) = work else {
        if !earlier {
            return Ok(Kept { kept: None, offer: None, skipped: false, skipped_why: None });
        }
        progress.set(Step::Earlier);
        // Stopped (cancelled, or a copy's replay wouldn't fit): the copies stay for next time,
        // and the window takes the journal over all the same.
        return Ok(match RecoveryOffer::earlier(recovery, cancel, &probe) {
            Ok(offer) => Kept { kept: None, offer, skipped: false, skipped_why: None },
            Err(error) => {
                let why = (!TransferCancel::is_cancellation(&error)).then(|| error.message.clone());
                if let Some(why) = &why {
                    log::warn!("earlier unsaved work wasn't checked: {why}");
                }
                Kept { kept: None, offer: None, skipped: true, skipped_why: why }
            }
        });
    };
    let kept = keep_copy(recovery, cancel, progress, &probe)?;
    // The project file it belongs to goes with it; without it, the work is recovered untitled,
    // which loses nothing.
    if let Some(location) = storage::recorded_location(recovery)
        && let Err(error) = storage::record_location(&kept, Some(&location))
    {
        log::warn!("the kept journal's project file wasn't recorded: {error}");
    }
    let offer = RecoveryOffer::from_replay(kept.clone(), replayed);
    Ok(Kept { kept: Some(kept), offer: Some(offer), skipped: false, skipped_why: None })
}

fn unkeepable(_: impl std::fmt::Debug) -> Error {
    Error::new(ErrorKind::Conflict, "an earlier session's recovery journal can't be kept, so it wasn't replaced")
}

/// Copies the journal at `recovery` to the first free `<name>.unrecovered-<n>` slot beside it,
/// on disk before it returns, and returns the slot.
fn keep_copy(recovery: &Path, cancel: &TransferCancel, progress: &Progress, probe: &dyn Fn() -> SpaceProbe) -> Result<PathBuf, Error> {
    let name = recovery.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "session.journal".into());
    for n in 1..=storage::MAX_KEPT {
        let kept = recovery.with_file_name(format!("{name}.unrecovered-{n}"));
        match std::fs::symlink_metadata(&kept) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => continue,
            Err(error) => return Err(unkeepable(error)),
        }
        let total = std::fs::metadata(recovery).map_err(unkeepable)?.len();
        // The replay's own copy is already on disk, so the free space reported has it.
        SpaceInventory::keep_journal(total)?.check(&probe())?;
        progress.set(Step::Copying { copied: 0, total });
        copy_aside(recovery, &kept, cancel, &mut |copied| progress.set(Step::Copying { copied, total }))?;
        return Ok(kept);
    }
    Err(Error::new(
        ErrorKind::Conflict,
        "all recovery safety-copy slots are occupied, so this session's journal was not replaced and earlier work is untouched; move or delete old session.journal.unrecovered-* copies in the recovery folder, then restart",
    ))
}

/// The name a copy is written under until it is complete.
pub(super) fn partial(kept: &Path) -> PathBuf {
    let mut name = kept.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".partial");
    kept.with_file_name(name)
}

/// Copies `from` to `to` through a `.partial` file, a chunk at a time, checking `cancel` and
/// reporting the bytes copied after each, then syncs it and renames it into place. On any
/// failure or cancel the partial file is removed and `to` never appears.
pub(super) fn copy_aside(from: &Path, to: &Path, cancel: &TransferCancel, copied: &mut dyn FnMut(u64)) -> Result<(), Error> {
    let staged = partial(to);
    let result = (|| {
        let mut source = std::fs::File::open(from).map_err(unkeepable)?;
        // A partial copy a crash or a forced quit left behind is replaced.
        let mut target = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&staged).map_err(unkeepable)?;
        let mut buffer = Vec::new();
        buffer.try_reserve_exact(COPY_CHUNK).map_err(unkeepable)?;
        buffer.resize(COPY_CHUNK, 0);
        let mut done = 0u64;
        loop {
            cancel.check()?;
            let n = source.read(&mut buffer).map_err(unkeepable)?;
            if n == 0 {
                break;
            }
            target.write_all(buffer.get(..n).ok_or_else(|| unkeepable("read past the buffer"))?).map_err(unkeepable)?;
            done = done.saturating_add(n as u64);
            copied(done);
        }
        // On disk before the original can be checkpointed over. Windows flushes only files
        // opened for writing, as this one is.
        target.sync_all().map_err(unkeepable)?;
        drop(target);
        cancel.check()?;
        // A rename replaces what is there on some systems: the slot must still be free.
        match std::fs::symlink_metadata(to) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(unkeepable("the recovery slot was taken while copying")),
        }
        std::fs::rename(&staged, to).map_err(unkeepable)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// How a keep-aside ended.
pub(super) enum Outcome {
    /// Settled: the storage, its lock still held and its first checkpoint already written off
    /// the UI thread (#367), for the session to take over; or why that checkpoint failed after
    /// the journal was kept aside.
    Kept(Box<(Result<nodecraft_engine::PreparedStorage, Error>, Kept)>),
    Failed(Error),
    /// "Decide later" or quitting stopped it; the journal is as it was.
    Cancelled,
    /// The job was dropped without running (its runner went away).
    Lost,
}

/// What the startup does once the keep-aside settles: it needs the journal's storage.
#[derive(Default)]
pub(super) struct After {
    pub runs: Option<super::runs::Runs>,
    pub runs_journal: Option<PathBuf>,
    pub open: Option<PathBuf>,
}

/// A keep-aside in flight.
pub(super) struct KeepAside {
    pub journal: PathBuf,
    cancel: TransferCancel,
    progress: Arc<Progress>,
    settled: mpsc::Receiver<Outcome>,
    pub after: After,
}

impl KeepAside {
    /// Starts keeping the journal at `journal` aside on `spawn`, `storage` holding its lock.
    /// Once it is kept, the same thread checkpoints `snapshot` (what the held session shows)
    /// into the storage, so taking it over on the UI thread writes nothing.
    pub(super) fn start(journal: PathBuf, storage: NativeStorage, snapshot: nodecraft_format::Snapshot, spawn: &Spawn) -> Self {
        let cancel = TransferCancel::default();
        let progress = Arc::new(Progress::default());
        let (send, settled) = mpsc::channel();
        let job: Job = {
            let (journal, cancel, progress) = (journal.clone(), cancel.clone(), Arc::clone(&progress));
            Box::new(move || {
                let outcome = match keep(&journal, &storage, &cancel, &progress, true) {
                    Ok(kept) => Outcome::Kept(Box::new((nodecraft_engine::Session::prepare_storage(Box::new(storage), snapshot), kept))),
                    // The storage, and with it the journal's lock, goes before the window hears.
                    Err(error) => {
                        drop(storage);
                        if TransferCancel::is_cancellation(&error) { Outcome::Cancelled } else { Outcome::Failed(error) }
                    }
                };
                let _ = send.send(outcome);
            })
        };
        if let Err(job) = spawn(job) {
            log::warn!("the keep-aside couldn't start a thread; keeping the journal aside inline");
            job();
        }
        Self { journal, cancel, progress, settled, after: After::default() }
    }
    pub(super) fn step(&self) -> Step {
        self.progress.step()
    }
    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(super) fn cancelling(&self) -> bool {
        self.cancel.check().is_err()
    }
    /// The outcome, once the job has ended.
    fn poll(&self) -> Option<Outcome> {
        match self.settled.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Outcome::Lost),
        }
    }
    /// Cancels and waits a bounded while for the job to end (quitting).
    fn stop(&self) -> Option<Outcome> {
        self.cancel();
        match self.settled.recv_timeout(QUIT_WAIT) {
            Ok(outcome) => Some(outcome),
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Outcome::Lost),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        }
    }
}

/// `message` as a sentence ending in one full stop.
fn sentence(message: &str) -> String {
    let trimmed = message.trim_end().trim_end_matches('.');
    format!("{trimmed}.")
}

fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

/// What the window says while the keep-aside runs.
pub(super) fn describe(step: Step, cancelling: bool) -> (String, Option<f32>) {
    if cancelling {
        return ("Stopping… the unsaved work stays where it is.".into(), None);
    }
    match step {
        Step::Reading => ("Reading the last session's record of unsaved work…".into(), None),
        Step::Copying { copied, total } => {
            let fraction = if total == 0 { 1.0 } else { (copied as f64 / total as f64).clamp(0.0, 1.0) as f32 };
            (format!("Copying it to the recovery folder: {} of {}…", size(copied), size(total)), Some(fraction))
        }
        Step::Earlier => ("Looking for earlier unsaved work to offer…".into(), None),
    }
}

impl NodeCraftApp {
    /// Every frame while the startup keep-aside runs: its window, and once it ends, the storage
    /// for the session and the rest of the startup it held back (#1053). Returns whether it is
    /// still running, in which case the window owns this frame's input.
    pub(super) fn keep_aside_frame(&mut self, ctx: &egui::Context) -> bool {
        let Some(job) = &self.keep_aside else {
            return false;
        };
        if let Some(outcome) = job.poll() {
            self.keep_aside_settled(outcome);
            return self.keep_aside.is_some();
        }
        // The step moves on the job's thread without a frame: keep looking.
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        let (step, cancelling) = (job.step(), job.cancelling());
        let (said, fraction) = describe(step, cancelling);
        let t = *self.theme.tokens();
        let mut later = false;
        let (heading, why, later_hover) = if step == Step::Earlier {
            (
                "Looking for unsaved work…",
                "Unsaved work from an earlier session is waiting in the recovery folder. NodeCraft is checking which copy can still be opened.",
                "Stop looking. The copies stay in the recovery folder and are offered the next time NodeCraft starts.",
            )
        } else {
            (
                "Keeping unsaved work safe…",
                "NodeCraft didn't close cleanly last time. Before this window can save projects, it keeps a copy of that session's unsaved work.",
                "Stop now. The unsaved work stays where it is and is offered the next time NodeCraft starts; until then this window can't save projects.",
            )
        };
        egui::Modal::new(egui::Id::new("keep_aside1053")).show(ctx, |ui| {
            ui.set_max_width(t.dialog_width);
            ui.heading(heading);
            ui.label(why);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(said);
            });
            if let Some(fraction) = fraction {
                ui.add(egui::ProgressBar::new(fraction));
            }
            later = ui
                .add_enabled(!cancelling, egui::Button::new("Decide later"))
                .on_hover_text(later_hover)
                .on_disabled_hover_text("Cancelling; it stops in a moment")
                .clicked();
        });
        if later && let Some(job) = &self.keep_aside {
            job.cancel();
            ctx.request_repaint();
        }
        // Nothing behind the window reacts while it runs.
        ctx.input_mut(|i| {
            i.events.clear();
            i.raw.events.clear();
            i.raw.dropped_files.clear();
            i.keys_down.clear();
            i.modifiers = egui::Modifiers::NONE;
            i.smooth_scroll_delta = egui::Vec2::ZERO;
        });
        true
    }

    fn keep_aside_settled(&mut self, outcome: Outcome) {
        let Some(job) = self.keep_aside.take() else {
            return;
        };
        let KeepAside { journal, after, .. } = *job;
        // Whatever happened, the window works from now on, with storage or without it.
        self.session.hold_for_storage(false);
        let mut owns = false;
        let problem = match outcome {
            Outcome::Kept(settled) => {
                let (prepared, kept) = *settled;
                let attached = prepared.and_then(|prepared| self.session.attach_prepared(prepared)).map(|()| kept);
                match attached {
                Ok(kept) => {
                    owns = true;
                    if let Some(kept) = &kept.kept {
                        log::info!("recovery: kept the earlier session's journal as {}", kept.display());
                    }
                    if kept.skipped {
                        // A space refusal says what is short and by how much (sizes, never paths).
                        let why = kept.skipped_why.as_deref().map(|why| format!(" {}", sentence(why))).unwrap_or_default();
                        self.notice = Some(format!(
                            "Earlier unsaved work wasn't checked.{why} It stays in the recovery folder and is offered the next time NodeCraft starts."
                        ));
                    }
                    self.attach_recovery_offer(journal, kept.offer);
                    None
                }
                Err(error) => {
                    log::warn!("recovery: the journal was kept aside but couldn't be taken over: {}", error.message);
                    Some(format!(
                    "Project files are unavailable: {} The last session's unsaved work was kept in the recovery folder. Close this window and reopen NodeCraft to recover it. This window's edits cannot be saved.",
                    sentence(&error.message)
                ))
                }
                }
            }
            Outcome::Failed(error) => {
                log::error!("project storage unavailable: {}", error.message);
                Some(format!(
                    "Project files are unavailable: {} The last session's unsaved work was left where it was. Close this window and reopen NodeCraft to try again. This window's edits cannot be saved.",
                    sentence(&error.message)
                ))
            }
            Outcome::Cancelled => Some(
                "The last session's unsaved work was left where it was; NodeCraft offers it again the next time it starts. This window's edits cannot be saved: close it and reopen NodeCraft to recover that work."
                    .to_owned(),
            ),
            Outcome::Lost => Some(
                "Project files are unavailable: keeping the last session's unsaved work safe stopped before it finished. The work was left where it was. Close this window and reopen NodeCraft to try again. This window's edits cannot be saved."
                    .to_owned(),
            ),
        };
        if let Some(mut problem) = problem {
            // The project named at startup needs the storage to open.
            if let Some(name) = after.open.as_ref().and_then(|path| path.file_name()) {
                problem = format!("{problem} {} wasn't opened.", name.to_string_lossy());
            }
            self.notice = Some(self.remember_storage_problem(problem, false));
        }
        // Only a window that owns project storage (and so its folder's lock) keeps runs.
        if let Some(mut runs) = after.runs {
            if owns && let Some(path) = after.runs_journal {
                runs.open_journal(path);
            }
            runs.install(&mut self.session);
            self.runs = Some(runs);
        }
        if owns {
            // Opened in the background by the next frame, once the window can show it.
            self.files.startup = after.open;
        }
    }

    /// Quitting while the keep-aside runs: it is cancelled, and waited for a bounded while so its
    /// partial copy is removed. If it had already finished, the session takes the storage over
    /// as it would have, so the journal's state is decided as for any session.
    pub(super) fn stop_keep_aside(&mut self) {
        let Some(job) = &self.keep_aside else {
            return;
        };
        match job.stop() {
            Some(outcome) => self.keep_aside_settled(outcome),
            None => {
                log::warn!("the keep-aside didn't stop in time; its partial copy is replaced next launch");
                self.keep_aside = None;
                self.session.hold_for_storage(false);
            }
        }
    }

    /// Whether the startup keep-aside is still running.
    pub fn keeping_aside(&self) -> bool {
        self.keep_aside.is_some()
    }
}

#[cfg(test)]
mod tests;
