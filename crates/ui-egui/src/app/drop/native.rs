//! Native acquisition has one process-wide, never-replaced worker and occupied request.
use super::{MAX_DROP_BYTES, MAX_DROP_TOTAL_BYTES};
use nodecraft_engine::import::MAX_FILES;
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock, PoisonError, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
mod read;
const PATH_BYTES: usize = 32 * 1024;
const NAME_BYTES: usize = 320;
const DEADLINE: Duration = Duration::from_secs(10);
const STACK_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Busy,
    Unavailable,
    Allocation,
    Cancelled,
    Timeout,
    Unexpected,
}
impl Failure {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::Busy => "File reading is still finishing. Try again later, or restart the app if it remains unavailable.",
            Self::Unavailable => "File reading is unavailable. Restart the app to try again.",
            Self::Allocation => "There isn't enough memory to prepare this drop. Try fewer or smaller files.",
            Self::Cancelled => "File reading cancelled; nothing was added.",
            Self::Timeout => "File reading timed out; nothing was added. Try a local file, or retry later.",
            Self::Unexpected => "File reading failed unexpectedly; nothing was added.",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reason {
    PathLong,
    Nonregular,
    Unreadable,
    TooBig,
    Aggregate,
    Changed,
    Interrupted,
    Many,
}
impl Reason {
    fn message(self) -> &'static str {
        match self {
            Self::PathLong => "the file path is longer than the 32 KiB drop limit",
            Self::Nonregular => "only files can be added, not folders or devices",
            Self::Unreadable => "the file can't be read",
            Self::TooBig => "files over 16 MiB can't be added",
            Self::Aggregate => "a drop reads at most 48 MiB",
            Self::Changed => "the file changed or ended while it was being read; try again",
            Self::Interrupted => "the file kept interrupting reads; try again",
            Self::Many => "at most 32 files are added at once",
        }
    }
}
#[derive(Default)]
pub(super) struct Summary {
    first: Option<(Option<String>, Reason)>,
    count: usize,
}
impl Summary {
    fn add(&mut self, name: Option<String>, reason: Reason, count: usize) -> Result<(), Failure> {
        if count == 0 {
            return Ok(());
        }
        self.count = self.count.checked_add(count).ok_or(Failure::Unexpected)?;
        if self.first.is_none() {
            self.first = Some((name, reason));
        }
        Ok(())
    }
    pub(super) fn notice(&self, imported: &nodecraft_engine::import::Imported) -> Option<String> {
        let Some((name, reason)) = &self.first else { return imported.notice() };
        let name = name.as_deref().unwrap_or("Additional files");
        let more = self.count.saturating_sub(1).saturating_add(imported.skipped.len());
        Some(if more == 0 {
            format!("{name} wasn't added: {}.", reason.message())
        } else {
            format!("{name} wasn't added: {} (and {more} more files weren't).", reason.message())
        })
    }
}
enum Source {
    Path(PathBuf),
    Skip(Reason),
}
struct FileRequest {
    name: String,
    source: Source,
}
pub(super) struct ReadFile {
    pub name: String,
    pub bytes: Vec<u8>,
}
trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}
struct Monotonic;
impl Clock for Monotonic {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
trait OpenedFile: Read + Send {
    fn metadata(&self) -> io::Result<(bool, u64)>;
}
trait Acquirer: Send + Sync {
    fn open(&self, path: &Path) -> io::Result<Box<dyn OpenedFile>>;
}
struct Filesystem;
impl OpenedFile for std::fs::File {
    fn metadata(&self) -> io::Result<(bool, u64)> {
        let m = std::fs::File::metadata(self)?;
        Ok((m.is_file(), m.len()))
    }
}
impl Acquirer for Filesystem {
    fn open(&self, path: &Path) -> io::Result<Box<dyn OpenedFile>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        // A FIFO swapped in for the file would block a plain open until a writer appears. With
        // O_NONBLOCK the open returns at once and the regular-file check rejects it; reads of a
        // regular file ignore the flag. Other Unix special files can still block (decision record).
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NONBLOCK);
        // Windows refuses to open a folder as a file, so its regular-file check never runs; a
        // stat (which never blocks on special files) tells a folder from an unreadable file.
        let file =
            options.open(path).map_err(|error| if std::fs::metadata(path).is_ok_and(|m| m.is_dir()) { io::ErrorKind::IsADirectory.into() } else { error })?;
        Ok(Box::new(file))
    }
}
struct Lease(Arc<AtomicBool>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
struct State {
    cancelled: AtomicBool,
    deadline: Instant,
    clock: Arc<dyn Clock>,
}
impl State {
    fn check(&self) -> Result<(), Failure> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(Failure::Cancelled)
        } else if self.clock.now() >= self.deadline {
            Err(Failure::Timeout)
        } else {
            Ok(())
        }
    }
}
struct Control {
    state: Arc<State>,
    lease: Arc<Lease>,
    result: Mutex<Option<Result<Batch, Failure>>>,
}
struct Job {
    files: Vec<FileRequest>,
    extra: usize,
    control: Arc<Control>,
}
struct Shared {
    mailbox: Mutex<Option<Job>>,
    wake: Condvar,
    busy: Arc<AtomicBool>,
    alive: AtomicBool,
    stop: AtomicBool,
    backend: Arc<dyn Acquirer>,
    clock: Arc<dyn Clock>,
}
#[derive(Clone)]
pub(super) struct Service(Arc<Shared>);
pub(super) struct Ticket {
    control: Arc<Control>,
    consumed: bool,
}
pub(super) struct Batch {
    pub files: Vec<ReadFile>,
    pub skipped: Summary,
    state: Arc<State>,
    _lease: Arc<Lease>,
}
impl Batch {
    pub(super) fn check(&self) -> Result<(), Failure> {
        self.state.check()
    }
}
pub(super) enum Poll {
    Pending,
    Ready(Batch),
    Failed(Failure),
}
impl Ticket {
    pub(super) fn check(&self) -> Result<(), Failure> {
        self.control.state.check()
    }
    pub(super) fn poll(&mut self) -> Poll {
        if self.consumed {
            return Poll::Failed(Failure::Cancelled);
        }
        if let Err(e) = self.control.state.check() {
            return Poll::Failed(e);
        }
        let result = match self.control.result.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(TryLockError::WouldBlock) => return Poll::Pending,
            Err(TryLockError::Poisoned(_)) => return Poll::Failed(Failure::Unexpected),
        };
        if let Err(e) = self.control.state.check() {
            drop(result);
            return Poll::Failed(e);
        }
        match result {
            Some(Ok(batch)) => {
                self.consumed = true;
                Poll::Ready(batch)
            }
            Some(Err(e)) => Poll::Failed(e),
            None => Poll::Pending,
        }
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.consumed {
            self.control.state.cancelled.store(true, Ordering::Release);
        }
    }
}
impl Service {
    pub(super) fn global() -> Result<Self, Failure> {
        static GLOBAL: OnceLock<Result<Service, Failure>> = OnceLock::new();
        GLOBAL.get_or_init(|| Self::spawn(Arc::new(Filesystem), Arc::new(Monotonic))).clone()
    }
    fn spawn(backend: Arc<dyn Acquirer>, clock: Arc<dyn Clock>) -> Result<Self, Failure> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(None),
            wake: Condvar::new(),
            busy: Arc::new(AtomicBool::new(false)),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            backend,
            clock,
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("nodecraft-native-drop".into())
            .stack_size(STACK_BYTES)
            .spawn(move || worker_loop(worker))
            .map_err(|_| Failure::Unavailable)?;
        Ok(Self(shared))
    }
    pub(super) fn start(&self, dropped: &[egui::DroppedFileHandle]) -> Result<Ticket, Failure> {
        if !self.0.alive.load(Ordering::Acquire) {
            return Err(Failure::Unavailable);
        }
        self.0.busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).map_err(|_| Failure::Busy)?;
        let lease = Arc::new(Lease(Arc::clone(&self.0.busy)));
        let deadline = self.0.clock.now().checked_add(DEADLINE).ok_or(Failure::Unexpected)?;
        let state = Arc::new(State { cancelled: AtomicBool::new(false), deadline, clock: Arc::clone(&self.0.clock) });
        state.check()?;
        let mut files = Vec::new();
        files.try_reserve_exact(dropped.len().min(MAX_FILES)).map_err(|_| Failure::Allocation)?;
        if files.capacity() > MAX_FILES {
            return Err(Failure::Allocation);
        }
        for handle in dropped.iter().take(MAX_FILES) {
            state.check()?;
            let path = handle.path();
            let len = path.as_os_str().as_encoded_bytes().len();
            let mut name = String::new();
            name.try_reserve_exact(NAME_BYTES).map_err(|_| Failure::Allocation)?;
            if name.capacity() > NAME_BYTES {
                return Err(Failure::Allocation);
            }
            let source = if len > PATH_BYTES {
                name.push_str("A file");
                Source::Skip(Reason::PathLong)
            } else {
                let display = path.file_name().map(|n| n.to_string_lossy());
                for c in display.as_deref().unwrap_or("A file").chars().take(80) {
                    name.push(c);
                }
                let mut owned = PathBuf::new();
                owned.try_reserve_exact(len).map_err(|_| Failure::Allocation)?;
                if owned.capacity() > PATH_BYTES {
                    return Err(Failure::Allocation);
                }
                owned.push(path);
                Source::Path(owned)
            };
            state.check()?;
            files.push(FileRequest { name, source });
        }
        let control = Arc::new(Control { state, lease, result: Mutex::new(None) });
        let job = Job { files, extra: dropped.len().saturating_sub(MAX_FILES), control: Arc::clone(&control) };
        // Waits for the mailbox rather than refusing a batch already admitted (#483): the worker
        // holds it only to check or take the slot, and nothing that holds it does I/O, so the
        // wait is bounded. The occupied lease above is what refuses a second batch.
        let mut slot = self.0.mailbox.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_some() || !self.0.alive.load(Ordering::Acquire) {
            drop(slot);
            drop(job);
            return Err(Failure::Unavailable);
        }
        *slot = Some(job);
        drop(slot);
        self.0.wake.notify_one();
        Ok(Ticket { control, consumed: false })
    }
}
struct Exit(Arc<Shared>);
impl Drop for Exit {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::Release);
        let job = self.0.mailbox.lock().unwrap_or_else(PoisonError::into_inner).take();
        drop(job);
    }
}
fn worker_loop(shared: Arc<Shared>) {
    let _exit = Exit(Arc::clone(&shared));
    loop {
        let job = {
            let mut slot = shared.mailbox.lock().unwrap_or_else(PoisonError::into_inner);
            while slot.is_none() && !shared.stop.load(Ordering::Acquire) {
                slot = shared.wake.wait(slot).unwrap_or_else(PoisonError::into_inner);
            }
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            slot.take()
        };
        let Some(job) = job else { continue };
        // No shared lock spans backend I/O, file Drop, bounded body work or rejected payload Drop.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read::acquire(&shared, job.files, job.extra, &job.control)))
            .unwrap_or(Err(Failure::Unexpected));
        let result = match job.control.state.check() {
            Ok(()) => result,
            Err(e) => {
                drop(result);
                Err(e)
            }
        };
        let old = {
            let mut slot = job.control.result.lock().unwrap_or_else(PoisonError::into_inner);
            slot.replace(result)
        };
        drop(old);
        // The control (and possibly last occupied lease) ends here, before idle wait.
    }
}
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(super) struct TestService {
    pub service: Service,
}
#[cfg(test)]
impl TestService {
    pub(super) fn filesystem() -> Self {
        Self { service: Service::spawn(Arc::new(Filesystem), Arc::new(Monotonic)).unwrap() }
    }
}
#[cfg(test)]
impl Drop for TestService {
    fn drop(&mut self) {
        self.service.0.stop.store(true, Ordering::Release);
        self.service.0.wake.notify_one();
        let end = Instant::now() + Duration::from_secs(10);
        while self.service.0.alive.load(Ordering::Acquire) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(1));
        }
        if !std::thread::panicking() {
            assert!(!self.service.0.alive.load(Ordering::Acquire), "bounded test worker cleanup");
        }
    }
}
