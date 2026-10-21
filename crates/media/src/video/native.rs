use super::quota::{Budget, Usage};
use super::*;
use crate::ContentHash;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
    sync::{
        Mutex, Weak,
        mpsc::{SyncSender, TrySendError},
    },
    thread,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
static SERVICE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
struct Control {
    counters: Arc<process::Counters>,
    closed: Arc<AtomicBool>,
    budget: Arc<Budget>,
    id: u64,
    report: Mutex<ShutdownReport>,
    render_active: AtomicBool,
}
struct Pin {
    control: Arc<Control>,
    id: u64,
}
struct Record {
    id: u64,
    pin: Weak<Pin>,
    payload: Option<(PathBuf, Lease)>,
    creating: bool,
    retry: Instant,
    delay: Duration,
}
struct RecordRegistry {
    records: Vec<Record>,
    next: u64,
}
struct CreationGuard {
    registry: Arc<Mutex<RecordRegistry>>,
    id: u64,
}
impl Drop for CreationGuard {
    fn drop(&mut self) {
        let mut registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(record) = registry.records.iter_mut().find(|r| r.id == self.id) {
            record.creating = false;
        }
    }
}
struct DirectoryGuard(Option<PathBuf>);
impl Drop for DirectoryGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_dir(path);
        }
    }
}
const MAX_CACHE_ENTRIES: usize = 256;
/// The one fixed VP9 encoding every output uses.
const VP9: &[&str] = &[
    "-c:v",
    "libvpx-vp9",
    "-threads:v",
    "2",
    "-row-mt",
    "0",
    "-tile-columns",
    "0",
    "-tile-rows",
    "0",
    "-deadline",
    "good",
    "-cpu-used",
    "4",
    "-crf",
    "32",
    "-b:v",
    "4000000",
    "-g",
    "120",
    "-lag-in-frames",
    "0",
    "-pix_fmt",
    "yuv420p",
];
/// No input metadata survives into an output; colour is always declared BT.709 limited range.
const TAGS: &[&str] = &[
    "-map_metadata",
    "-1",
    "-map_chapters",
    "-1",
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-colorspace",
    "bt709",
    "-color_range",
    "tv",
    "-f",
    "webm",
];
#[derive(Default)]
struct FrameCache {
    entries: VecDeque<Cached>,
    bytes: usize,
}
impl FrameCache {
    fn preallocated() -> Result<Self> {
        let mut entries = VecDeque::new();
        entries.try_reserve_exact(MAX_CACHE_ENTRIES).map_err(|_| VideoError::Limit)?;
        if entries.capacity() != MAX_CACHE_ENTRIES {
            return Err(VideoError::Limit);
        }
        Ok(Self { entries, bytes: 0 })
    }
}
struct RenderAdmission {
    control: Arc<Control>,
    writer_ready: AtomicBool,
    _lease: Lease,
}
impl Drop for RenderAdmission {
    fn drop(&mut self) {
        self.control.render_active.store(false, Ordering::Release);
    }
}
/// Immutable staged input, tied to one service. Clones share the same file/resource pin.
#[derive(Clone)]
pub struct VideoAsset {
    pin: Arc<Pin>,
    metadata: Arc<VideoMetadata>,
    hash: ContentHash,
    mov: bool,
}
impl VideoAsset {
    pub fn metadata(&self) -> &VideoMetadata {
        &self.metadata
    }
    pub fn hash(&self) -> ContentHash {
        self.hash
    }
}
/// An immutable staged sound-only file, tied to one service.
#[derive(Clone)]
pub struct AudioAsset {
    pin: Arc<Pin>,
    metadata: Arc<AudioMetadata>,
    hash: ContentHash,
    demuxer: &'static str,
}
impl AudioAsset {
    pub fn metadata(&self) -> &AudioMetadata {
        &self.metadata
    }
    pub fn hash(&self) -> ContentHash {
        self.hash
    }
}
/// An immutable staged still picture: raw RGBA the caller decoded, so FFmpeg never parses an image.
#[derive(Clone)]
pub struct StillAsset {
    pin: Arc<Pin>,
    width: u32,
    height: u32,
    hash: ContentHash,
}
impl StillAsset {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn hash(&self) -> ContentHash {
        self.hash
    }
}
impl Source {
    fn pin(&self) -> &Arc<Pin> {
        match self {
            Self::Video(a) => &a.pin,
            Self::Audio(a) => &a.pin,
            Self::Still(s) => &s.pin,
        }
    }
}
/// One bounded asynchronously read artifact chunk. Clones share its accounted storage.
#[derive(Clone)]
pub struct VideoChunk {
    data: Arc<FrameData>,
}
impl VideoChunk {
    pub fn bytes(&self) -> &[u8] {
        &self.data.pixels
    }
}
struct FrameData {
    pixels: Vec<u8>,
    _lease: Lease,
}
#[derive(Clone)]
pub struct VideoFrame {
    data: Arc<FrameData>,
    pub width: u32,
    pub height: u32,
    pub requested_time_us: u64,
}
impl VideoFrame {
    pub fn pixels(&self) -> &[u8] {
        &self.data.pixels
    }
    pub fn shares_pixels(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }
}
#[derive(Clone)]
pub struct RenderedVideo {
    asset: VideoAsset,
    frames: u32,
    bytes: u64,
}
impl RenderedVideo {
    pub fn frames(&self) -> u32 {
        self.frames
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn asset(&self) -> &VideoAsset {
        &self.asset
    }
    pub fn hash(&self) -> ContentHash {
        self.asset.hash
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ShutdownReport {
    pub children_stopped: bool,
    pub external_pins: usize,
    pub owned_files: usize,
    pub cleanup_remaining: bool,
}
pub struct ShutdownTicket {
    control: Arc<Control>,
}
pub struct ShutdownWaitError {
    pub error: VideoError,
    pub ticket: ShutdownTicket,
}
impl ShutdownTicket {
    pub fn try_result(&self) -> Result<ShutdownReport> {
        self.control.report.lock().map(|r| *r).map_err(|_| VideoError::Closed)
    }
    pub fn wait_blocking(self, deadline: Duration) -> std::result::Result<ShutdownReport, ShutdownWaitError> {
        let Some(until) = Instant::now().checked_add(deadline) else {
            return Err(ShutdownWaitError { error: VideoError::Invalid, ticket: self });
        };
        loop {
            match self.try_result() {
                Ok(r) if !r.cleanup_remaining => return Ok(r),
                Err(e) => return Err(ShutdownWaitError { error: e, ticket: self }),
                _ => {}
            }
            if Instant::now() >= until {
                return Err(ShutdownWaitError { error: VideoError::Timeout, ticket: self });
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for ShutdownTicket {
    fn drop(&mut self) {
        self.control.closed.store(true, Ordering::Release)
    }
}
pub struct VideoOpening {
    rx: Receiver<Result<VideoService>>,
    cancel: Arc<AtomicBool>,
    consumed: bool,
}
impl VideoOpening {
    pub fn try_result(&mut self) -> Result<Option<VideoService>> {
        if self.consumed {
            return Err(VideoError::Consumed);
        }
        match self.rx.try_recv() {
            Ok(r) => {
                self.consumed = true;
                r.map(Some)
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.consumed = true;
                Err(VideoError::Unavailable)
            }
        }
    }
}
impl Drop for VideoOpening {
    fn drop(&mut self) {
        if !self.consumed {
            self.cancel.store(true, Ordering::Release)
        }
    }
}
type Work = Box<dyn FnOnce(&mut Context) + Send>;
pub struct VideoService {
    tx: SyncSender<Work>,
    control: Arc<Control>,
}
impl Drop for VideoService {
    fn drop(&mut self) {
        self.control.closed.store(true, Ordering::Release)
    }
}
impl VideoService {
    pub fn begin_open(config: ServiceConfig) -> Result<VideoOpening> {
        let deadline = deadline_after(Duration::from_secs(10))?;
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&cancel);
        thread::Builder::new()
            .name("video-supervisor".into())
            .spawn(move || {
                let result = Context::open(config, Arc::clone(&cancelled), deadline);
                match result {
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                    Ok(mut context) => {
                        let (work_tx, work_rx) = mpsc::sync_channel::<Work>(context.control.budget.limits.jobs);
                        let work_rx = Arc::new(Mutex::new(work_rx));
                        let mut workers = Vec::new();
                        let mut failed = false;
                        for index in 0..2 {
                            let mut worker = context.clone();
                            let rx = Arc::clone(&work_rx);
                            match thread::Builder::new().name(format!("video-operation-{index}")).spawn(move || {
                                loop {
                                    let message = match rx.lock() {
                                        Ok(queue) => queue.recv_timeout(Duration::from_millis(10)),
                                        Err(_) => {
                                            worker.control.closed.store(true, Ordering::Release);
                                            break;
                                        }
                                    };
                                    match message {
                                        Ok(job) => job(&mut worker),
                                        Err(mpsc::RecvTimeoutError::Timeout) => {
                                            if worker.control.closed.load(Ordering::Acquire) {
                                                break;
                                            }
                                        }
                                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                    }
                                }
                            }) {
                                Ok(handle) => workers.push(handle),
                                Err(_) => {
                                    failed = true;
                                    break;
                                }
                            }
                        }
                        let startup_error = if failed { Some(VideoError::Unavailable) } else { check_deadline(deadline).err() };
                        if let Some(error) = startup_error {
                            context.control.closed.store(true, Ordering::Release);
                            drop(work_tx);
                            let _ = tx.send(Err(error));
                        } else {
                            let service = VideoService { tx: work_tx, control: Arc::clone(&context.control) };
                            if tx.send(Ok(service)).is_err() {
                                context.control.closed.store(true, Ordering::Release);
                            }
                        }
                        context.serve(workers, work_rx);
                    }
                }
            })
            .map_err(|_| VideoError::Unavailable)?;
        Ok(VideoOpening { rx, cancel, consumed: false })
    }
    fn submit<T: Send + 'static>(&self, work: impl FnOnce(&mut Context, &Arc<AtomicBool>) -> Result<T> + Send + 'static) -> Result<VideoJob<T>> {
        if self.control.closed.load(Ordering::Acquire) {
            return Err(VideoError::Closed);
        }
        let slot = Arc::new(self.control.budget.reserve(Usage { jobs: 1, bytes: 1024, ..Usage::default() })?);
        let work_slot = Arc::clone(&slot);
        let (tx, rx) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let c = Arc::clone(&cancel);
        let job = Box::new(move |ctx: &mut Context| {
            let _work_slot = work_slot;
            let result = if c.load(Ordering::Acquire) || ctx.control.closed.load(Ordering::Acquire) { Err(VideoError::Cancelled) } else { work(ctx, &c) };
            let result = if c.load(Ordering::Acquire) || ctx.control.closed.load(Ordering::Acquire) { Err(VideoError::Cancelled) } else { result };
            let _ = tx.send(result);
        });
        match self.tx.try_send(job) {
            Ok(()) => Ok(VideoJob { rx, cancel, consumed: false, _slot: slot }),
            Err(TrySendError::Full(_)) => Err(VideoError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(VideoError::Closed),
        }
    }
    pub fn import(&self, source: Arc<[u8]>, policy: ColorPolicy) -> std::result::Result<VideoJob<VideoAsset>, (VideoError, Arc<[u8]>)> {
        self.stage(source, move |ctx, bytes, c, deadline| ctx.import(bytes, policy, c, deadline))
    }
    /// Stage a sound-only file (WAV, Ogg, FLAC, MP3, AAC, M4A, or the sound of a video file).
    pub fn import_audio(&self, source: Arc<[u8]>) -> std::result::Result<VideoJob<AudioAsset>, (VideoError, Arc<[u8]>)> {
        self.stage(source, |ctx, bytes, c, deadline| ctx.import_audio(bytes, c, deadline))
    }
    /// Stage a decoded `width`×`height` RGBA picture to hold on a timeline.
    pub fn stage_still(&self, rgba: Arc<[u8]>, width: u32, height: u32) -> std::result::Result<VideoJob<StillAsset>, (VideoError, Arc<[u8]>)> {
        let expected = u64::from(width).checked_mul(u64::from(height)).and_then(|p| p.checked_mul(4));
        if width == 0 || height == 0 || width > 8192 || height > 8192 || expected != u64::try_from(rgba.len()).ok() {
            return Err((VideoError::Invalid, rgba));
        }
        self.stage(rgba, move |ctx, bytes, c, deadline| {
            let (pin, _, hash) = ctx.write_staged(bytes, &[b"still", &width.to_le_bytes(), &height.to_le_bytes()], c, deadline)?;
            Ok(StillAsset { pin, width, height, hash })
        })
    }
    fn stage<T: Send + 'static>(
        &self,
        source: Arc<[u8]>,
        work: impl FnOnce(&mut Context, &[u8], &Arc<AtomicBool>, Instant) -> Result<T> + Send + 'static,
    ) -> std::result::Result<VideoJob<T>, (VideoError, Arc<[u8]>)> {
        if source.len() > self.control.budget.limits.input {
            return Err((VideoError::Limit, source));
        }
        let lease = match self.control.budget.reserve(Usage { bytes: source.len().saturating_add(64), ..Usage::default() }) {
            Ok(v) => v,
            Err(e) => return Err((e, source)),
        };
        let deadline = match deadline_after(Duration::from_secs(10)) {
            Ok(value) => value,
            Err(error) => return Err((error, source)),
        };
        let retained = Arc::clone(&source);
        match self.submit(move |ctx, c| {
            let _input = lease;
            work(ctx, &retained, c, deadline)
        }) {
            Ok(job) => Ok(job),
            Err(e) => Err((e, source)),
        }
    }
    /// Read at most 1 MiB without exposing the scratch path or doing filesystem I/O on this thread.
    pub fn read_chunk(&self, asset: &VideoAsset, offset: u64, length: usize) -> Result<VideoJob<VideoChunk>> {
        self.check(asset)?;
        if length == 0 || length > 1024 * 1024 {
            return Err(VideoError::Invalid);
        }
        let deadline = deadline_after(Duration::from_secs(15))?;
        let asset = asset.clone();
        self.submit(move |ctx, c| {
            check_deadline(deadline)?;
            use std::io::{Seek, SeekFrom};
            let lease = ctx.control.budget.reserve(Usage { bytes: length.saturating_add(64), ..Usage::default() })?;
            let mut file = File::open(ctx.path(&asset)?).map_err(|_| VideoError::Io)?;
            let size = file.metadata().map_err(|_| VideoError::Io)?.len();
            let end = offset.checked_add(u64::try_from(length).map_err(|_| VideoError::Limit)?).ok_or(VideoError::Limit)?;
            if end > size {
                return Err(VideoError::Invalid);
            }
            if c.load(Ordering::Acquire) {
                return Err(VideoError::Cancelled);
            }
            let mut pixels = Vec::new();
            pixels.try_reserve_exact(length).map_err(|_| VideoError::Limit)?;
            if pixels.capacity() != length {
                return Err(VideoError::Limit);
            }
            pixels.resize(length, 0);
            file.seek(SeekFrom::Start(offset)).map_err(|_| VideoError::Io)?;
            file.read_exact(&mut pixels).map_err(|_| VideoError::Io)?;
            check_deadline(deadline)?;
            Ok(VideoChunk { data: Arc::new(FrameData { pixels, _lease: lease }) })
        })
    }
    pub fn frame(&self, asset: &VideoAsset, request: FrameRequest) -> Result<VideoJob<VideoFrame>> {
        self.check(asset)?;
        let deadline = deadline_after(Duration::from_secs(15))?;
        let asset = asset.clone();
        self.submit(move |ctx, c| ctx.frame(&asset, request, c, deadline))
    }
    fn check(&self, a: &VideoAsset) -> Result<()> {
        self.check_pin(&a.pin)
    }
    fn check_pin(&self, pin: &Pin) -> Result<()> {
        if !Arc::ptr_eq(&pin.control, &self.control) {
            return Err(VideoError::Invalid);
        }
        Ok(())
    }
    pub fn render(&self, spec: RenderSpec) -> Result<(FrameSender, VideoJob<RenderedVideo>)> {
        if self.control.render_active.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Err(VideoError::Busy);
        }
        let bytes = spec
            .bytes()?
            .checked_mul(3)
            .and_then(|n| n.checked_add(spec.bytes().ok()?.checked_div(4)?.checked_mul(3)?))
            .and_then(|n| n.checked_add(65536 + 1024))
            .ok_or(VideoError::Limit);
        let lease = bytes.and_then(|bytes| self.control.budget.reserve(Usage { bytes, ..Usage::default() }));
        let lease = match lease {
            Ok(lease) => lease,
            Err(error) => {
                self.control.render_active.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let admission = Arc::new(RenderAdmission { control: Arc::clone(&self.control), writer_ready: AtomicBool::new(false), _lease: lease });
        let deadline = deadline_after(Duration::from_secs(600))?;
        let (tx, rx) = mpsc::sync_channel(2);
        let control = Arc::clone(&self.control);
        let finish = Arc::new(AtomicBool::new(false));
        let worker_finish = Arc::clone(&finish);
        let worker_admission = Arc::clone(&admission);
        let job = self.submit(move |ctx, c| ctx.render(spec, rx, worker_finish, c, deadline, worker_admission))?;
        let cancel = Arc::clone(&job.cancel);
        Ok((FrameSender { tx, spec, next: 0, cancel, control, finish, finished: false, admission }, job))
    }
    pub fn begin_shutdown(self) -> ShutdownTicket {
        self.control.closed.store(true, Ordering::Release);
        ShutdownTicket { control: Arc::clone(&self.control) }
    }
    /// Monotonic observations at actual process-spawn/capture allocation sites.
    pub fn statistics(&self) -> (usize, usize) {
        (self.control.counters.spawned.load(Ordering::Relaxed), self.control.counters.captured.load(Ordering::Relaxed))
    }
    pub fn usage(&self) -> Result<(usize, u64, usize, usize)> {
        let u = self.control.budget.used.lock().map_err(|_| VideoError::Closed)?;
        Ok((u.bytes, u.disk, u.jobs, u.records))
    }
}
pub struct FramePacket {
    pub index: u32,
    pub rgba: Arc<[u8]>,
}
pub struct TrySendFrameError {
    pub error: VideoError,
    pub packet: FramePacket,
}
struct QueuedFrame {
    packet: FramePacket,
    _admission: Arc<RenderAdmission>,
}
pub struct FrameSender {
    tx: SyncSender<QueuedFrame>,
    spec: RenderSpec,
    next: u32,
    cancel: Arc<AtomicBool>,
    control: Arc<Control>,
    finish: Arc<AtomicBool>,
    finished: bool,
    admission: Arc<RenderAdmission>,
}
impl FrameSender {
    pub fn try_send(&mut self, packet: FramePacket) -> std::result::Result<(), TrySendFrameError> {
        if packet.index != self.next || self.next >= self.spec.count || self.spec.bytes().ok() != Some(packet.rgba.len()) {
            return Err(TrySendFrameError { error: VideoError::Invalid, packet });
        }
        if self.cancel.load(Ordering::Acquire) || self.control.closed.load(Ordering::Acquire) {
            return Err(TrySendFrameError { error: VideoError::Closed, packet });
        }
        match self.tx.try_send(QueuedFrame { packet, _admission: Arc::clone(&self.admission) }) {
            Ok(()) => {
                self.next += 1;
                Ok(())
            }
            Err(e) => {
                let (error, msg) = match e {
                    TrySendError::Full(m) => (VideoError::Busy, m),
                    TrySendError::Disconnected(m) => (VideoError::Closed, m),
                };
                Err(TrySendFrameError { error, packet: msg.packet })
            }
        }
    }
    pub fn finish(mut self) -> std::result::Result<(), Self> {
        if self.next != self.spec.count {
            return Err(self);
        }
        self.finish.store(true, Ordering::Release);
        self.finished = true;
        Ok(())
    }
}
impl Drop for FrameSender {
    fn drop(&mut self) {
        if !self.finished {
            self.cancel.store(true, Ordering::Release)
        }
    }
}
#[derive(Default)]
pub struct ScrubSession {
    active: Option<VideoJob<VideoFrame>>,
    pending: Option<(VideoAsset, FrameRequest)>,
}
impl ScrubSession {
    pub fn request(&mut self, asset: &VideoAsset, request: FrameRequest) {
        if let Some(job) = &self.active {
            job.cancel()
        }
        self.pending = Some((asset.clone(), request));
    }
    pub fn poll(&mut self, service: &VideoService) -> Result<Option<VideoFrame>> {
        if self.pending.is_some() {
            self.active.take();
            if let Some((a, r)) = self.pending.take() {
                match service.frame(&a, r) {
                    Ok(job) => self.active = Some(job),
                    Err(VideoError::Busy) => {
                        self.pending = Some((a, r));
                        return Ok(None);
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        if let Some(j) = &mut self.active {
            match j.try_result() {
                Ok(Some(frame)) => {
                    self.active = None;
                    return Ok(Some(frame));
                }
                Err(e) => {
                    self.active = None;
                    return Err(e);
                }
                _ => {}
            }
        }
        Ok(None)
    }
}
struct Cached {
    _lease: Lease,
    id: u64,
    request: FrameRequest,
    frame: VideoFrame,
}
#[derive(Clone)]
struct Context {
    _base: Arc<Lease>,
    config: ServiceConfig,
    dir: PathBuf,
    control: Arc<Control>,
    registry: Arc<Mutex<RecordRegistry>>,
    cache: Arc<Mutex<FrameCache>>,
}
impl Context {
    fn open(config: ServiceConfig, closed: Arc<AtomicBool>, deadline: Instant) -> Result<Self> {
        check_deadline(deadline)?;
        for path in [&config.tools.ffmpeg, &config.tools.ffprobe] {
            if !fs::metadata(path).map_err(|_| VideoError::Unavailable)?.is_file() {
                return Err(VideoError::Unavailable);
            }
        }
        let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = None;
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| VideoError::Io)?.as_nanos();
        for n in 0..8 {
            let p = config.scratch.join(format!("nodecraft-video-{}-{nonce}-{id}-{n}", std::process::id()));
            match fs::create_dir(&p) {
                Ok(()) => {
                    dir = Some(p);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(VideoError::Io),
            }
        }
        let dir = dir.ok_or(VideoError::Io)?;
        let mut directory_guard = DirectoryGuard(Some(dir.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|_| VideoError::Io)?;
        }
        let control = Arc::new(Control {
            counters: Arc::new(process::Counters::default()),
            closed,
            budget: Arc::new(Budget { used: Mutex::new(Usage::default()), limits: config.limits }),
            id,
            report: Mutex::new(ShutdownReport { cleanup_remaining: true, ..ShutdownReport::default() }),
            render_active: AtomicBool::new(false),
        });
        let base = match control.budget.reserve(Usage {
            bytes: 65536usize
                .saturating_add(config.limits.records.saturating_mul(std::mem::size_of::<Record>()))
                .saturating_add(config.limits.jobs.saturating_mul(1024))
                .saturating_add(MAX_CACHE_ENTRIES.saturating_mul(std::mem::size_of::<Cached>())),
            ..Usage::default()
        }) {
            Ok(l) => l,
            Err(e) => {
                let _ = fs::remove_dir(&dir);
                return Err(e);
            }
        };
        let mut records = Vec::new();
        records.try_reserve_exact(config.limits.records).map_err(|_| VideoError::Limit)?;
        if records.capacity() != config.limits.records {
            return Err(VideoError::Limit);
        }
        let cache = FrameCache::preallocated()?;
        let mut ctx = Self {
            _base: Arc::new(base),
            config,
            dir,
            control,
            registry: Arc::new(Mutex::new(RecordRegistry { records, next: 0 })),
            cache: Arc::new(Mutex::new(cache)),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        for (tool, arg) in [(ctx.config.tools.ffmpeg.clone(), "-version"), (ctx.config.tools.ffprobe.clone(), "-version")] {
            ctx.run(&tool, &process::args(&[arg]), None, 65536, &cancel, deadline)?;
        }
        for (topic, required) in [
            ("demuxer=mov", "enable_drefs"),
            ("demuxer=mov", "use_absolute_path"),
            ("demuxer=matroska", "Demuxer"),
            ("filter=colorspace", "itrc"),
            ("filter=scale", "out_color_matrix"),
            ("filter=transpose", "clock"),
            ("encoder=libvpx-vp9", "lag-in-frames"),
            ("decoder=h264", "Decoder"),
            ("decoder=vp8", "Decoder"),
            ("decoder=vp9", "Decoder"),
        ] {
            let tool = ctx.config.tools.ffmpeg.clone();
            let reply = ctx.run(&tool, &process::args(&["-hide_banner", "-h", topic]), None, 65536, &cancel, deadline)?;
            if !reply.windows(required.len()).any(|b| b == required.as_bytes()) {
                let _ = fs::remove_dir(&ctx.dir);
                return Err(VideoError::Unsupported);
            }
        }
        check_deadline(deadline)?;
        directory_guard.0 = None;
        Ok(ctx)
    }
    fn serve(&mut self, mut workers: Vec<thread::JoinHandle<()>>, queue: Arc<Mutex<Receiver<Work>>>) {
        let mut directory_retry = Instant::now();
        let mut directory_delay = Duration::from_millis(100);
        loop {
            self.cleanup();
            if self.control.closed.load(Ordering::Acquire) {
                self.cancel_queued(&queue);
                let evicted = {
                    let mut cache = self.cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    cache.bytes = 0;
                    std::mem::take(&mut cache.entries)
                };
                drop(evicted);
            }
            let mut index = 0;
            while index < workers.len() {
                if workers.get(index).is_some_and(thread::JoinHandle::is_finished) {
                    let handle = workers.swap_remove(index);
                    if handle.join().is_err() {
                        self.control.closed.store(true, Ordering::Release);
                    }
                } else {
                    index += 1;
                }
            }
            if self.control.closed.load(Ordering::Acquire) && workers.is_empty() {
                if let Ok(mut report) = self.control.report.lock() {
                    report.children_stopped = true;
                }
                let empty = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).records.is_empty();
                if empty && Instant::now() >= directory_retry {
                    match fs::remove_dir(&self.dir) {
                        Ok(()) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                        Err(_) => {
                            directory_retry = Instant::now() + directory_delay;
                            directory_delay = directory_delay.saturating_mul(2).min(Duration::from_secs(5));
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        if let Ok(mut report) = self.control.report.lock() {
            report.cleanup_remaining = false;
        }
    }
    fn cancel_queued(&mut self, queue: &Mutex<Receiver<Work>>) {
        for _ in 0..self.config.limits.jobs {
            let message = match queue.try_lock() {
                Ok(queue) => queue.try_recv(),
                Err(std::sync::TryLockError::Poisoned(poison)) => poison.into_inner().try_recv(),
                Err(std::sync::TryLockError::WouldBlock) => break,
            };
            match message {
                // All production Work wrappers observe closed before calling their operation.
                Ok(job) => job(self),
                Err(_) => break,
            }
        }
    }
    fn cleanup(&mut self) {
        for _ in 0..16 {
            let task = {
                let mut registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                registry
                    .records
                    .iter_mut()
                    .find(|r| !r.creating && r.pin.strong_count() == 0 && r.retry <= Instant::now() && r.payload.is_some())
                    .and_then(|r| r.payload.take().map(|payload| (r.id, payload)))
            };
            let Some((id, (path, lease))) = task else {
                break;
            };
            let deleted = match fs::remove_file(&path) {
                Ok(()) => true,
                Err(error) => error.kind() == std::io::ErrorKind::NotFound,
            };
            let mut removed = None;
            {
                let mut registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(index) = registry.records.iter().position(|r| r.id == id) {
                    if deleted {
                        removed = Some(registry.records.swap_remove(index));
                    } else if let Some(record) = registry.records.get_mut(index) {
                        record.retry = Instant::now() + record.delay;
                        record.delay = record.delay.saturating_mul(2).min(Duration::from_secs(5));
                        record.payload = Some((path, lease));
                        continue;
                    }
                }
            }
            drop(removed);
            drop(lease);
        }
        let registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(mut report) = self.control.report.lock() {
            report.external_pins = registry.records.iter().filter(|r| r.pin.strong_count() > 0).count();
            report.owned_files = registry.records.len();
            report.cleanup_remaining = true;
        }
    }
    fn run(
        &mut self,
        tool: &Path,
        args: &[std::ffi::OsString],
        input: Option<process::Input>,
        cap: usize,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<Vec<u8>> {
        check_deadline(deadline)?;
        let _capture = self.control.budget.reserve(Usage { bytes: cap.saturating_add(65536), ..Usage::default() })?;
        let closed = Arc::clone(&self.control.closed);
        let dir = self.dir.clone();
        process::run(
            tool,
            args,
            &dir,
            input,
            cap,
            process::Watchdog { cancel, closed: &closed, deadline, counters: Some(Arc::clone(&self.control.counters)) },
            &mut || {},
        )
        .map(|r| r.bytes)
    }
    fn record(&mut self, bytes: u64) -> Result<(Arc<Pin>, PathBuf)> {
        let lease = self.control.budget.reserve(Usage { bytes: 4608, disk: bytes, records: 1, ..Usage::default() })?;
        let (pin, path, guard) = {
            let mut registry = self.registry.lock().map_err(|_| VideoError::Closed)?;
            registry.next = registry.next.checked_add(1).ok_or(VideoError::Limit)?;
            let id = registry.next;
            let path = self.dir.join(format!("asset-{id}"));
            let pin = Arc::new(Pin { control: Arc::clone(&self.control), id });
            registry.records.push(Record {
                id,
                pin: Arc::downgrade(&pin),
                payload: Some((path.clone(), lease)),
                creating: true,
                retry: Instant::now(),
                delay: Duration::from_millis(100),
            });
            (pin, path, CreationGuard { registry: Arc::clone(&self.registry), id })
        };
        let created = OpenOptions::new().write(true).create_new(true).open(&path).map_err(|_| VideoError::Io);
        drop(guard);
        created?;
        Ok((pin, path))
    }
    fn path(&self, a: &VideoAsset) -> Result<PathBuf> {
        self.pin_path(&a.pin)
    }
    pub(super) fn pin_path(&self, pin: &Pin) -> Result<PathBuf> {
        self.registry
            .lock()
            .map_err(|_| VideoError::Closed)?
            .records
            .iter()
            .find(|r| r.id == pin.id && pin.control.id == self.control.id)
            .and_then(|r| r.payload.as_ref().map(|(path, _)| path.clone()))
            .ok_or(VideoError::Invalid)
    }
    fn probe(&mut self, path: &Path, mov: bool, policy: ColorPolicy, count: bool, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<VideoMetadata> {
        let _parse = self.control.budget.reserve(Usage { bytes: 2 * 1024 * 1024, ..Usage::default() })?;
        let mut args = process::args(&["-v", "error", "-max_alloc", "134217728"]);
        args.extend(probe::input_args(mov));
        if count {
            args.extend(process::args(&["-count_frames"]));
        }
        args.extend(process::args(&["-show_entries","stream=index,codec_name,codec_type,width,height,pix_fmt,field_order,color_space,color_primaries,color_transfer,color_range,sample_aspect_ratio,time_base,start_pts,start_time,duration_ts,duration,avg_frame_rate,nb_read_frames:stream_tags=rotate:stream_side_data=rotation,displaymatrix:format=start_time,duration","-of","json","-i"]));
        args.push(path.as_os_str().to_owned());
        let tool = self.config.tools.ffprobe.clone();
        let bytes = self.run(&tool, &args, None, 262144, cancel, deadline)?;
        probe::parse(&bytes, policy)
    }
    fn import(&mut self, source: &[u8], policy: ColorPolicy, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<VideoAsset> {
        let mov = match crate::sniff(source).map(|s| s.mime) {
            Some("video/mp4" | "video/quicktime") => true,
            Some("video/webm" | "video/x-matroska") => false,
            _ => return Err(VideoError::Unsupported),
        };
        let (pin, path, hash) = self.write_staged(source, &[], cancel, deadline)?;
        let metadata = self.probe(&path, mov, policy, false, cancel, deadline)?;
        check_deadline(deadline)?;
        Ok(VideoAsset { pin, metadata: Arc::new(metadata), hash, mov })
    }
    fn import_audio(&mut self, source: &[u8], cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<AudioAsset> {
        let demuxer = crate::sniff(source).and_then(|s| probe::audio_demuxer(s.mime)).ok_or(VideoError::Unsupported)?;
        let (pin, path, hash) = self.write_staged(source, &[], cancel, deadline)?;
        let _parse = self.control.budget.reserve(Usage { bytes: 2 * 1024 * 1024, ..Usage::default() })?;
        let mut args = process::args(&["-v", "error", "-max_alloc", "134217728"]);
        args.extend(probe::audio_input_args(demuxer));
        args.extend(process::args(&[
            "-show_entries",
            "stream=index,codec_name,codec_type,sample_rate,channels,start_time,duration:format=start_time,duration",
            "-of",
            "json",
            "-i",
        ]));
        args.push(path.as_os_str().to_owned());
        let tool = self.config.tools.ffprobe.clone();
        let bytes = self.run(&tool, &args, None, 262144, cancel, deadline)?;
        let metadata = probe::parse_audio(&bytes)?;
        check_deadline(deadline)?;
        Ok(AudioAsset { pin, metadata: Arc::new(metadata), hash, demuxer })
    }
    /// Copy `source` into a fresh scratch record, hashing `salt` and then the bytes.
    fn write_staged(&mut self, source: &[u8], salt: &[&[u8]], cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<(Arc<Pin>, PathBuf, ContentHash)> {
        let (pin, path) = self.record(u64::try_from(source.len()).map_err(|_| VideoError::Limit)?)?;
        let mut f = OpenOptions::new().write(true).open(&path).map_err(|_| VideoError::Io)?;
        let mut hash = blake3::Hasher::new();
        for part in salt {
            hash.update(part);
        }
        for chunk in source.chunks(65536) {
            if cancel.load(Ordering::Acquire) || self.control.closed.load(Ordering::Acquire) {
                return Err(VideoError::Cancelled);
            }
            check_deadline(deadline)?;
            f.write_all(chunk).map_err(|_| VideoError::Io)?;
            hash.update(chunk);
        }
        drop(f);
        Ok((pin, path, ContentHash(*hash.finalize().as_bytes())))
    }
    fn frame(&mut self, asset: &VideoAsset, r: FrameRequest, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<VideoFrame> {
        check_deadline(deadline)?;
        if r.time_us >= asset.metadata.duration_us {
            return Err(VideoError::NoFrame);
        }
        {
            let mut cache = self.cache.lock().map_err(|_| VideoError::Closed)?;
            if let Some(index) = cache.entries.iter().position(|v| v.id == asset.pin.id && v.request == r)
                && let Some(hit) = cache.entries.remove(index)
            {
                let frame = hit.frame.clone();
                cache.entries.push_back(hit);
                check_deadline(deadline)?;
                return Ok(frame);
            }
        }
        let cache_request = r;
        let m = &asset.metadata;
        let mut r = r;
        if !r.upscale {
            let dw = (u64::from(m.width) * u64::from(m.sar.0)).div_ceil(u64::from(m.sar.1));
            let dh = u64::from(m.height);
            let (dw, dh) = if matches!(m.rotation, 90 | 270) { (dh, dw) } else { (dw, dh) };
            r.width = r.width.min(u32::try_from(dw.min(2048)).map_err(|_| VideoError::Limit)?);
            r.height = r.height.min(u32::try_from(dh.min(2048)).map_err(|_| VideoError::Limit)?);
        }
        let mut w = u64::from(m.width) * u64::from(m.sar.0);
        let mut h = u64::from(m.height) * u64::from(m.sar.1);
        if matches!(m.rotation, 90 | 270) {
            std::mem::swap(&mut w, &mut h)
        }
        let (w, h) = if w * u64::from(r.height) > h * u64::from(r.width) {
            (u64::from(r.width), (h * u64::from(r.width) / w).max(1))
        } else {
            ((w * u64::from(r.height) / h).max(1), u64::from(r.height))
        };
        let (w, h) = (u32::try_from(w).map_err(|_| VideoError::Limit)?, u32::try_from(h).map_err(|_| VideoError::Limit)?);
        if u64::from(w) * u64::from(h) > self.config.limits.preview_pixels {
            return Err(VideoError::Limit);
        }
        let bytes = usize::try_from(u64::from(w) * u64::from(h) * 4).map_err(|_| VideoError::Limit)?;
        let lease = self.control.budget.reserve(Usage { bytes: bytes.saturating_add(128), ..Usage::default() })?;
        let mut args = process::args(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-max_alloc",
            "134217728",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-copyts",
            "-start_at_zero",
            "-noautorotate",
        ]);
        args.extend(probe::input_args(asset.mov));
        args.push("-i".into());
        args.push(self.path(asset)?.into_os_string());
        let u = m.stream_start - m.format_start + i128::from(r.time_us);
        let time = format!("{}.{:06}", u / 1_000_000, u % 1_000_000);
        let map = format!("0:{}", m.stream);
        args.extend(process::args(&[
            "-ss",
            &time,
            "-map",
            &map,
            "-frames:v",
            "1",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &probe::filter(m, w, h),
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
            "pipe:1",
        ]));
        let tool = self.config.tools.ffmpeg.clone();
        let pixels = self.run(&tool, &args, None, bytes.checked_add(1).ok_or(VideoError::Limit)?, cancel, deadline)?;
        if pixels.is_empty() {
            return Err(VideoError::NoFrame);
        }
        if pixels.len() != bytes {
            return Err(VideoError::Process);
        }
        let frame = VideoFrame { data: Arc::new(FrameData { pixels, _lease: lease }), width: w, height: h, requested_time_us: r.time_us };
        self.cache_frame(asset.pin.id, cache_request, &frame)?;
        check_deadline(deadline)?;
        Ok(frame)
    }
    fn cache_frame(&self, id: u64, request: FrameRequest, frame: &VideoFrame) -> Result<()> {
        let bytes = frame.data.pixels.capacity();
        if bytes.saturating_add(128) <= self.config.limits.cache
            && let Ok(cache_lease) = self.control.budget.reserve(Usage { bytes: 128, ..Usage::default() })
        {
            let entry = Cached { _lease: cache_lease, id, request, frame: frame.clone() };
            loop {
                let evicted = {
                    let mut cache = self.cache.lock().map_err(|_| VideoError::Closed)?;
                    if cache.bytes.saturating_add(bytes.saturating_add(128)) <= self.config.limits.cache && cache.entries.len() < MAX_CACHE_ENTRIES {
                        cache.entries.push_back(entry);
                        cache.bytes += bytes.saturating_add(128);
                        break;
                    }
                    let value = cache.entries.pop_front();
                    if let Some(value) = &value {
                        cache.bytes = cache.bytes.saturating_sub(value.frame.data.pixels.capacity().saturating_add(128));
                    }
                    value
                };
                drop(evicted);
            }
        }
        Ok(())
    }
    /// Run an encoder writing `path`, stopping it once the file outgrows `quota`.
    fn encode(
        &mut self,
        args: &[std::ffi::OsString],
        path: &Path,
        quota: u64,
        input: Option<process::Input>,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<()> {
        let tool = self.config.tools.ffmpeg.clone();
        let dir = self.dir.clone();
        let closed = Arc::clone(&self.control.closed);
        let mut exceeded = false;
        let process_cancel = Arc::new(AtomicBool::new(false));
        let result = process::run(
            &tool,
            args,
            &dir,
            input,
            0,
            process::Watchdog { cancel: &process_cancel, closed: &closed, deadline, counters: Some(Arc::clone(&self.control.counters)) },
            &mut || {
                if cancel.load(Ordering::Acquire) {
                    process_cancel.store(true, Ordering::Release);
                }
                if fs::metadata(path).is_ok_and(|m| m.len() > quota) {
                    exceeded = true;
                    process_cancel.store(true, Ordering::Release);
                }
            },
        );
        if exceeded {
            return Err(VideoError::Limit);
        }
        result?;
        Ok(())
    }
    /// Hash a finished output and shrink its disk reservation to the `size` it took.
    fn seal(&mut self, pin: Arc<Pin>, path: &Path, metadata: VideoMetadata, size: u64, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<VideoAsset> {
        let mut file = File::open(path).map_err(|_| VideoError::Io)?;
        let mut hash = blake3::Hasher::new();
        let mut buf = [0u8; 65536];
        loop {
            if cancel.load(Ordering::Acquire) || self.control.closed.load(Ordering::Acquire) {
                return Err(VideoError::Cancelled);
            }
            check_deadline(deadline)?;
            let n = file.read(&mut buf).map_err(|_| VideoError::Io)?;
            if n == 0 {
                break;
            }
            hash.update(buf.get(..n).ok_or(VideoError::Io)?);
        }
        check_deadline(deadline)?;
        {
            let mut registry = self.registry.lock().map_err(|_| VideoError::Closed)?;
            let record = registry.records.iter_mut().find(|record| record.id == pin.id).ok_or(VideoError::Closed)?;
            let (_, lease) = record.payload.as_mut().ok_or(VideoError::Closed)?;
            lease.shrink_disk(size)?;
        }
        check_deadline(deadline)?;
        Ok(VideoAsset { pin, metadata: Arc::new(metadata), hash: ContentHash(*hash.finalize().as_bytes()), mov: false })
    }
    fn render(
        &mut self,
        spec: RenderSpec,
        rx: Receiver<QueuedFrame>,
        finish: Arc<AtomicBool>,
        cancel: &Arc<AtomicBool>,
        deadline: Instant,
        admission: Arc<RenderAdmission>,
    ) -> Result<RenderedVideo> {
        let quota = self.config.limits.output;
        let (pin, path) = self.record(quota.checked_add(64 * 1024 * 1024).ok_or(VideoError::Limit)?)?;
        let rgb_len = spec.bytes()?.checked_div(4).and_then(|n| n.checked_mul(3)).ok_or(VideoError::Limit)?;
        let scratch = Arc::clone(&admission);
        let c = Arc::clone(cancel);
        let closed = Arc::clone(&self.control.closed);
        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = Arc::clone(&stop);
        let input = process::Input {
            stop,
            write: Box::new(move |out| {
                let mut rgb = Vec::new();
                rgb.try_reserve_exact(rgb_len).map_err(|_| VideoError::Limit)?;
                rgb.resize(rgb_len, 0);
                if rgb.capacity() != rgb_len {
                    return Err(VideoError::Limit);
                }
                scratch.writer_ready.store(true, Ordering::Release);
                let mut next = 0;
                loop {
                    if next == spec.count && finish.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if c.load(Ordering::Acquire) || closed.load(Ordering::Acquire) || writer_stop.load(Ordering::Acquire) {
                        return Err(VideoError::Cancelled);
                    }
                    if Instant::now() >= deadline {
                        return Err(VideoError::Timeout);
                    }
                    match rx.recv_timeout(Duration::from_millis(10)) {
                        Ok(frame) => {
                            if frame.packet.index != next || next >= spec.count {
                                return Err(VideoError::Invalid);
                            }
                            composite(&frame.packet.rgba, &mut rgb, spec.background)?;
                            out.write_all(&rgb).map_err(|_| VideoError::Io)?;
                            next += 1;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            return if next == spec.count && finish.load(Ordering::Acquire) { Ok(()) } else { Err(VideoError::Cancelled) };
                        }
                    }
                }
            }),
        };
        let mut args = process::args(&[
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-y",
            "-max_alloc",
            "134217728",
            "-filter_threads",
            "1",
            "-filter_complex_threads",
            "1",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            &format!("{}x{}", spec.width, spec.height),
            "-framerate",
            &format!("{}/{}", spec.fps.0, spec.fps.1),
            "-i",
            "pipe:0",
            "-map",
            "0:v:0",
            "-an",
            "-sn",
            "-dn",
            "-vf",
            "scale=iw:ih:flags=bilinear:in_range=pc:out_range=pc:out_color_matrix=bt709,format=yuv444p,colorspace=iall=bt709:itrc=srgb:irange=pc:all=bt709:range=tv:format=yuv420p:fast=0:dither=none",
        ]);
        args.extend(process::args(VP9));
        args.extend(process::args(&["-fps_mode", "passthrough", "-frames:v", &spec.count.to_string()]));
        args.extend(process::args(TAGS));
        args.extend(process::args(&["-fs", &quota.to_string()]));
        args.push(path.as_os_str().to_owned());
        self.encode(&args, &path, quota, Some(input), cancel, deadline)?;
        if Instant::now() >= deadline {
            return Err(VideoError::Timeout);
        }
        let size = fs::metadata(&path).map_err(|_| VideoError::Io)?.len();
        if size == 0 || size > quota {
            return Err(VideoError::Limit);
        }
        let metadata = self.probe(&path, false, ColorPolicy::RequireDeclared, true, cancel, deadline)?;
        if metadata.frames != Some(u64::from(spec.count))
            || metadata.width != spec.width
            || metadata.height != spec.height
            || !metadata.frame_rate.is_some_and(|(n, d)| u128::from(n) * u128::from(spec.fps.1) == u128::from(d) * u128::from(spec.fps.0))
        {
            return Err(VideoError::Process);
        }
        let expected = u64::from(spec.count) * u64::from(spec.fps.1) * 1_000_000 / u64::from(spec.fps.0);
        let tolerance = u64::from(spec.fps.1) * 1_000_000 / u64::from(spec.fps.0) + metadata.timebase.0 * 1_000_000 / metadata.timebase.1 + 1;
        if metadata.duration_us.abs_diff(expected) > tolerance {
            return Err(VideoError::Process);
        }
        let asset = self.seal(pin, &path, metadata, size, cancel, deadline)?;
        Ok(RenderedVideo { asset, frames: spec.count, bytes: size })
    }
}
fn linear(v: u8) -> f64 {
    let s = f64::from(v) / 255.;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}
fn encode(v: f64) -> u8 {
    let s = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1. / 2.4) - 0.055 };
    (s.clamp(0., 1.) * 255.).round() as u8
}
fn composite(rgba: &[u8], rgb: &mut [u8], background: [u8; 3]) -> Result<()> {
    if rgba.len() / 4 != rgb.len() / 3 || !rgba.len().is_multiple_of(4) || !rgb.len().is_multiple_of(3) {
        return Err(VideoError::Invalid);
    }
    for (src, dst) in rgba.as_chunks::<4>().0.iter().zip(rgb.as_chunks_mut::<3>().0.iter_mut()) {
        let a = *src.get(3).ok_or(VideoError::Invalid)?;
        for ((s, d), b) in src.iter().take(3).zip(dst.iter_mut()).zip(background) {
            *d = match a {
                0 => b,
                255 => *s,
                _ => encode(linear(*s) * f64::from(a) / 255. + linear(b) * (1. - f64::from(a) / 255.)),
            };
        }
    }
    Ok(())
}

mod compose;
#[cfg(test)]
mod tests;

fn deadline_after(duration: Duration) -> Result<Instant> {
    Instant::now().checked_add(duration).ok_or(VideoError::Invalid)
}
fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline { Err(VideoError::Timeout) } else { Ok(()) }
}
