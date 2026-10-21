//! Native, bounded video decoding and encoding through explicitly configured external tools.
//! No FFmpeg binary is downloaded or bundled. See `docs/media-video.md` for trust and limits.
// The browser retains typed configuration validation, but has no native consumer for its fields.
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_imports))]
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::Duration,
};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
mod probe;
mod timeline;
pub use timeline::{ComposeCaption, ComposeClip, ComposeSpec, MAX_CAPTIONS, MAX_CLIPS, MAX_DURATION_US, MAX_VOLUME, MIN_TRANSITION_US, Source};
#[cfg(not(target_arch = "wasm32"))]
mod process;
mod quota;
use quota::Lease;

/// Public-safe errors; tool stderr, paths and input metadata are never included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoError {
    Invalid,
    Limit,
    Busy,
    Unavailable,
    Unsupported,
    InvalidMetadata,
    NoFrame,
    Cancelled,
    Timeout,
    Process,
    Io,
    Consumed,
    Closed,
}
impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "invalid video request",
            Self::Limit => "video service limit exceeded",
            Self::Busy => "video service is busy",
            Self::Unavailable => "configured video tools are unavailable",
            Self::Unsupported => "video capability is unsupported",
            Self::InvalidMetadata => "video metadata is invalid or unsupported",
            Self::NoFrame => "no video frame at this time",
            Self::Cancelled => "video operation cancelled",
            Self::Timeout => "video operation timed out",
            Self::Process => "video tool failed",
            Self::Io => "video storage failed",
            Self::Consumed => "video result already consumed",
            Self::Closed => "video service is closed",
        })
    }
}
impl std::error::Error for VideoError {}
type Result<T> = std::result::Result<T, VideoError>;

/// Lower-only service limits. These are logical retained extents, not process RSS.
#[derive(Clone, Copy, Debug)]
pub struct VideoLimits {
    pub(crate) retained: usize,
    pub(crate) disk: u64,
    pub(crate) cache: usize,
    pub(crate) jobs: usize,
    pub(crate) records: usize,
    pub(crate) input: usize,
    pub(crate) output: u64,
    pub(crate) preview_pixels: u64,
}
impl Default for VideoLimits {
    fn default() -> Self {
        Self {
            retained: 512 * 1024 * 1024,
            disk: 1024 * 1024 * 1024,
            cache: 64 * 1024 * 1024,
            jobs: 32,
            records: 1024,
            input: 256 * 1024 * 1024,
            output: 512 * 1024 * 1024,
            preview_pixels: 4_194_304,
        }
    }
}
impl VideoLimits {
    pub fn lower_retained(mut self, bytes: usize) -> Result<Self> {
        if bytes == 0 || bytes > self.retained {
            return Err(VideoError::Invalid);
        }
        self.retained = bytes;
        Ok(self)
    }
    pub fn lower_disk(mut self, bytes: u64) -> Result<Self> {
        if bytes == 0 || bytes > self.disk {
            return Err(VideoError::Invalid);
        }
        self.disk = bytes;
        Ok(self)
    }
    pub fn lower_jobs(mut self, count: usize) -> Result<Self> {
        if count == 0 || count > self.jobs {
            return Err(VideoError::Invalid);
        }
        self.jobs = count;
        Ok(self)
    }
    pub fn lower_input(mut self, bytes: usize) -> Result<Self> {
        if bytes == 0 || bytes > self.input {
            return Err(VideoError::Invalid);
        }
        self.input = bytes;
        Ok(self)
    }
    /// Caps each rendered file, so an encode stops once it outgrows what the caller keeps.
    pub fn lower_output(mut self, bytes: u64) -> Result<Self> {
        if bytes == 0 || bytes > self.output {
            return Err(VideoError::Invalid);
        }
        self.output = bytes;
        Ok(self)
    }
}
#[derive(Clone, Debug)]
pub struct ToolPaths {
    pub(crate) ffmpeg: PathBuf,
    pub(crate) ffprobe: PathBuf,
}
impl ToolPaths {
    pub fn new(ffmpeg: PathBuf, ffprobe: PathBuf) -> Result<Self> {
        for p in [&ffmpeg, &ffprobe] {
            if !p.is_absolute() || (p.as_os_str().len() > 4096 || p.as_os_str().as_encoded_bytes().contains(&0)) {
                return Err(VideoError::Invalid);
            }
        }
        Ok(Self { ffmpeg, ffprobe })
    }
}
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub(crate) tools: ToolPaths,
    pub(crate) scratch: PathBuf,
    pub(crate) limits: VideoLimits,
}
impl ServiceConfig {
    pub fn new(tools: ToolPaths, scratch_parent: PathBuf, limits: VideoLimits) -> Result<Self> {
        if !scratch_parent.is_absolute() || scratch_parent.as_os_str().len() > 4096 {
            return Err(VideoError::Invalid);
        }
        Ok(Self { tools, scratch: scratch_parent, limits })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorPolicy {
    RequireDeclared,
    #[default]
    AssumeBt709ForUnspecified,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorProfile {
    Bt709,
    Smpte170m,
    Bt470bg,
}
#[derive(Clone, Debug)]
pub struct VideoColorInfo {
    pub original: [Option<String>; 4],
    pub profile: ColorProfile,
    pub full_range: bool,
    pub assumed_fields: u8,
}
#[derive(Clone, Debug)]
pub struct VideoMetadata {
    pub width: u32,
    pub height: u32,
    pub duration_us: u64,
    pub stream: u32,
    pub rotation: u16,
    pub sar: (u32, u32),
    pub frame_rate: Option<(u64, u64)>,
    pub color: VideoColorInfo,
    pub(crate) format_start: i128,
    pub(crate) stream_start: i128,
    pub(crate) timebase: (u64, u64),
    pub(crate) frames: Option<u64>,
    /// First decodable audio stream: its index and FFmpeg decoder name.
    pub(crate) audio: Option<(u32, &'static str)>,
}
impl VideoMetadata {
    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }
}
/// A sound-only file's first decodable audio stream.
#[derive(Clone, Debug)]
pub struct AudioMetadata {
    pub duration_us: u64,
    pub(crate) stream: u32,
    /// FFmpeg decoder name.
    pub(crate) codec: &'static str,
    pub(crate) format_start: i128,
    pub(crate) stream_start: i128,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameRequest {
    pub(crate) time_us: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) upscale: bool,
}
impl FrameRequest {
    pub fn new(time_us: u64, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 || width > 2048 || height > 2048 {
            return Err(VideoError::Invalid);
        }
        Ok(Self { time_us, width, height, upscale: false })
    }
    pub fn with_upscale(mut self, upscale: bool) -> Self {
        self.upscale = upscale;
        self
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RenderSpec {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) fps: (u32, u32),
    pub(crate) count: u32,
    pub(crate) background: [u8; 3],
}
impl RenderSpec {
    pub fn new(width: u32, height: u32, fps: (u32, u32), count: u32, background: [u8; 3]) -> Result<Self> {
        if width == 0
            || height == 0
            || !width.is_multiple_of(2)
            || !height.is_multiple_of(2)
            || width > 4096
            || height > 4096
            || u64::from(width) * u64::from(height) > 16_777_216
            || fps.0 == 0
            || fps.0 > 120000
            || fps.1 == 0
            || fps.1 > 1001
            || u64::from(fps.0) * 10 < u64::from(fps.1)
            || u64::from(fps.0) > 120 * u64::from(fps.1)
            || count == 0
            || count > 432000
            || u64::from(count) * u64::from(fps.1) > 3600 * u64::from(fps.0)
        {
            return Err(VideoError::Invalid);
        }
        Ok(Self { width, height, fps, count, background })
    }
    pub(crate) fn bytes(self) -> Result<usize> {
        usize::try_from(u64::from(self.width) * u64::from(self.height) * 4).map_err(|_| VideoError::Limit)
    }
}

/// Pollable one-shot; Drop requests cancellation, never joins or touches the filesystem.
pub struct VideoJob<T> {
    rx: Receiver<Result<T>>,
    pub(crate) cancel: Arc<AtomicBool>,
    consumed: bool,
    _slot: Arc<Lease>,
}
impl<T> VideoJob<T> {
    pub fn try_result(&mut self) -> Result<Option<T>> {
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
                Err(VideoError::Closed)
            }
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release)
    }
}
impl<T> Drop for VideoJob<T> {
    fn drop(&mut self) {
        self.cancel()
    }
}
#[cfg(not(target_arch = "wasm32"))]
pub use native::{
    AudioAsset, FramePacket, FrameSender, RenderedVideo, ScrubSession, ShutdownReport, ShutdownTicket, ShutdownWaitError, StillAsset, TrySendFrameError,
    VideoAsset, VideoChunk, VideoFrame, VideoOpening, VideoService,
};
#[cfg(target_arch = "wasm32")]
mod unsupported;
#[cfg(target_arch = "wasm32")]
pub use unsupported::*;
