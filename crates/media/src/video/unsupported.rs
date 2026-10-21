//! Browser API parity: no object can be created and no native side effect is attempted.
use super::*;
#[derive(Clone)]
pub enum VideoAsset {}
impl VideoAsset {
    pub fn metadata(&self) -> &VideoMetadata {
        match *self {}
    }
    pub fn hash(&self) -> crate::ContentHash {
        match *self {}
    }
}
#[derive(Clone)]
pub enum AudioAsset {}
impl AudioAsset {
    pub fn metadata(&self) -> &AudioMetadata {
        match *self {}
    }
    pub fn hash(&self) -> crate::ContentHash {
        match *self {}
    }
}
#[derive(Clone)]
pub enum StillAsset {}
impl StillAsset {
    pub fn width(&self) -> u32 {
        match *self {}
    }
    pub fn height(&self) -> u32 {
        match *self {}
    }
    pub fn hash(&self) -> crate::ContentHash {
        match *self {}
    }
}
#[derive(Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub requested_time_us: u64,
    data: Arc<[u8]>,
}
impl VideoFrame {
    pub fn pixels(&self) -> &[u8] {
        &self.data
    }
    pub fn shares_pixels(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }
}
#[derive(Clone)]
pub enum VideoChunk {}
impl VideoChunk {
    pub fn bytes(&self) -> &[u8] {
        match *self {}
    }
}
#[derive(Clone)]
pub enum RenderedVideo {}
impl RenderedVideo {
    pub fn asset(&self) -> &VideoAsset {
        match *self {}
    }
    pub fn frames(&self) -> u32 {
        match *self {}
    }
    pub fn bytes(&self) -> u64 {
        match *self {}
    }
    pub fn hash(&self) -> crate::ContentHash {
        match *self {}
    }
}
#[derive(Clone)]
pub enum RenderedAudio {}
impl RenderedAudio {
    pub fn asset(&self) -> &AudioAsset {
        match *self {}
    }
    pub fn bytes(&self) -> u64 {
        match *self {}
    }
    pub fn hash(&self) -> crate::ContentHash {
        match *self {}
    }
}
pub enum VideoOpening {}
impl VideoOpening {
    pub fn try_result(&mut self) -> Result<Option<VideoService>> {
        match *self {}
    }
}
pub enum VideoService {}
impl VideoService {
    pub fn begin_open(_: ServiceConfig) -> Result<VideoOpening> {
        Err(VideoError::Unsupported)
    }
    pub fn import(&self, source: Arc<[u8]>, _: ColorPolicy) -> std::result::Result<VideoJob<VideoAsset>, (VideoError, Arc<[u8]>)> {
        Err((VideoError::Unsupported, source))
    }
    pub fn import_audio(&self, source: Arc<[u8]>) -> std::result::Result<VideoJob<AudioAsset>, (VideoError, Arc<[u8]>)> {
        Err((VideoError::Unsupported, source))
    }
    pub fn stage_still(&self, rgba: Arc<[u8]>, _: u32, _: u32) -> std::result::Result<VideoJob<StillAsset>, (VideoError, Arc<[u8]>)> {
        Err((VideoError::Unsupported, rgba))
    }
    pub fn frame(&self, _: &VideoAsset, _: FrameRequest) -> Result<VideoJob<VideoFrame>> {
        Err(VideoError::Unsupported)
    }
    pub fn render(&self, _: RenderSpec) -> Result<(FrameSender, VideoJob<RenderedVideo>)> {
        Err(VideoError::Unsupported)
    }
    pub fn compose(&self, _: ComposeSpec) -> Result<VideoJob<RenderedVideo>> {
        Err(VideoError::Unsupported)
    }
    pub fn read_chunk(&self, _: &VideoAsset, _: u64, _: usize) -> Result<VideoJob<VideoChunk>> {
        Err(VideoError::Unsupported)
    }
    pub fn read_audio_chunk(&self, _: &AudioAsset, _: u64, _: usize) -> Result<VideoJob<VideoChunk>> {
        Err(VideoError::Unsupported)
    }
    pub fn scenes(&self, _: &VideoAsset, _: u16) -> Result<VideoJob<Vec<u64>>> {
        Err(VideoError::Unsupported)
    }
    pub fn extract_audio(&self, _: &VideoAsset) -> Result<VideoJob<RenderedAudio>> {
        Err(VideoError::Unsupported)
    }
    pub fn statistics(&self) -> (usize, usize) {
        (0, 0)
    }
    pub fn usage(&self) -> Result<(usize, u64, usize, usize)> {
        Err(VideoError::Unsupported)
    }
    pub fn begin_shutdown(self) -> ShutdownTicket {
        match self {}
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
pub enum FrameSender {}
impl FrameSender {
    pub fn try_send(&mut self, packet: FramePacket) -> std::result::Result<(), TrySendFrameError> {
        Err(TrySendFrameError { error: VideoError::Unsupported, packet })
    }
    pub fn finish(self) -> std::result::Result<(), Self> {
        Err(self)
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ShutdownReport {
    pub children_stopped: bool,
    pub external_pins: usize,
    pub owned_files: usize,
    pub cleanup_remaining: bool,
}
pub enum ShutdownTicket {}
pub struct ShutdownWaitError {
    pub error: VideoError,
    pub ticket: ShutdownTicket,
}
impl ShutdownTicket {
    pub fn try_result(&self) -> Result<ShutdownReport> {
        match *self {}
    }
    pub fn wait_blocking(self, _: Duration) -> std::result::Result<ShutdownReport, ShutdownWaitError> {
        match self {}
    }
}
#[derive(Default)]
pub struct ScrubSession;
impl ScrubSession {
    pub fn request(&mut self, _: &VideoAsset, _: FrameRequest) {}
    pub fn poll(&mut self, _: &VideoService) -> Result<Option<VideoFrame>> {
        Err(VideoError::Unsupported)
    }
}
