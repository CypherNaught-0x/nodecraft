//! Listening to a staged sound: its waveform, and decoded stretches of it to play.
use super::*;

/// The most mono samples a waveform decode keeps: 32 MiB of 16-bit samples.
const MAX_WAVEFORM_SAMPLES: u64 = 16 * 1024 * 1024;
/// The rate a waveform is decoded at, when the sound is short enough to allow it.
const WAVEFORM_RATE: u64 = 8_000;
/// Below this a waveform would miss too much; the sound's too long for one.
const MIN_WAVEFORM_RATE: u64 = 1_000;
const PCM_FRAME_BYTES: usize = 2 * PCM_CHANNELS as usize;

impl VideoService {
    /// The loudest sample in each of `buckets` equal slices of `asset`.
    pub fn waveform(&self, asset: &AudioAsset, buckets: usize) -> Result<VideoJob<Waveform>> {
        self.check_pin(&asset.pin)?;
        if buckets == 0 || buckets > MAX_WAVEFORM_BUCKETS {
            return Err(VideoError::Invalid);
        }
        let rate = waveform_rate(asset.metadata.duration_us)?;
        // Decoding is quick, but an hour of a heavy codec on a slow machine takes a while.
        let deadline = deadline_after(Duration::from_secs(30u64.saturating_add(asset.metadata.duration_us / 30_000_000)))?;
        let asset = asset.clone();
        self.submit(move |ctx, c| ctx.waveform(&asset, buckets, rate, c, deadline))
    }

    /// `length_us` of `asset` from `start_us`, decoded to [`PCM_RATE`] stereo: shorter (or
    /// empty) where the sound ends first.
    pub fn pcm(&self, asset: &AudioAsset, start_us: u64, length_us: u64) -> Result<VideoJob<PcmChunk>> {
        self.check_pin(&asset.pin)?;
        let frames = pcm_frames(length_us)?;
        let deadline = deadline_after(Duration::from_secs(10))?;
        let asset = asset.clone();
        self.submit(move |ctx, c| ctx.pcm(&asset, start_us, length_us, frames, c, deadline))
    }
}

impl Context {
    fn waveform(&mut self, asset: &AudioAsset, buckets: usize, rate: u64, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<Waveform> {
        self.can_decode(asset, cancel, deadline)?;
        let expected = expected_samples(asset.metadata.duration_us, rate);
        // A little over what its length says, for sounds that run long; refused beyond.
        let cap = usize::try_from(expected.saturating_add(rate).saturating_mul(2)).map_err(|_| VideoError::Limit)?;
        let path = self.pin_path(&asset.pin)?;
        let args = waveform_args(asset.demuxer, &asset.metadata, &path, rate);
        let tool = self.config.tools.ffmpeg.clone();
        let bytes = self.run(&tool, &args, None, cap, cancel, deadline)?;
        check_deadline(deadline)?;
        peaks(&bytes, buckets, expected)
    }

    fn pcm(&mut self, asset: &AudioAsset, start_us: u64, length_us: u64, frames: usize, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<PcmChunk> {
        if start_us >= asset.metadata.duration_us {
            return Ok(PcmChunk::new(start_us, Arc::from(Vec::new())));
        }
        self.can_decode(asset, cancel, deadline)?;
        let bytes = frames.checked_mul(PCM_FRAME_BYTES).ok_or(VideoError::Limit)?;
        // A few frames of slack: resampling may round up.
        let cap = bytes.checked_add(64 * PCM_FRAME_BYTES).ok_or(VideoError::Limit)?;
        let path = self.pin_path(&asset.pin)?;
        let args = pcm_args(asset.demuxer, &asset.metadata, &path, start_us, length_us);
        let tool = self.config.tools.ffmpeg.clone();
        let out = self.run(&tool, &args, None, cap, cancel, deadline)?;
        check_deadline(deadline)?;
        Ok(PcmChunk::new(start_us, pcm_samples(&out, frames)?))
    }

    /// Refuses a sound the configured ffmpeg can't open or decode.
    fn can_decode(&mut self, asset: &AudioAsset, cancel: &Arc<AtomicBool>, deadline: Instant) -> Result<()> {
        let tool = self.config.tools.ffmpeg.clone();
        let (demuxer, codec) = (asset.demuxer, asset.metadata.codec);
        let mut needs = vec![(format!("demuxer={demuxer}"), format!("Demuxer {demuxer}")), (format!("decoder={codec}"), format!("Decoder {codec}"))];
        if asset.metadata.channels == 1 {
            needs.push(("filter=pan".to_owned(), "Filter pan".to_owned()));
        }
        for (topic, required) in needs {
            let reply = self.run(&tool, &process::args(&["-hide_banner", "-h", &topic]), None, 65536, cancel, deadline)?;
            if !reply.windows(required.len()).any(|b| b == required.as_bytes()) {
                return Err(VideoError::Unsupported);
            }
        }
        Ok(())
    }
}

/// The rate to decode a waveform of a `duration_us` sound at: as high as fits the sample cap.
pub(super) fn waveform_rate(duration_us: u64) -> Result<u64> {
    if duration_us > MAX_WAVEFORM_US {
        return Err(VideoError::Limit);
    }
    let seconds = duration_us.div_ceil(1_000_000).max(1);
    let rate = (MAX_WAVEFORM_SAMPLES / seconds).min(WAVEFORM_RATE);
    if rate < MIN_WAVEFORM_RATE { Err(VideoError::Limit) } else { Ok(rate) }
}

/// How many samples at `rate` a `duration_us` sound has.
fn expected_samples(duration_us: u64, rate: u64) -> u64 {
    u64::try_from(u128::from(duration_us).saturating_mul(u128::from(rate)) / 1_000_000).unwrap_or(u64::MAX)
}

/// The stereo frames in `length_us` at [`PCM_RATE`], refusing an empty or overlong request.
pub(super) fn pcm_frames(length_us: u64) -> Result<usize> {
    if length_us == 0 || length_us > MAX_PCM_US {
        return Err(VideoError::Invalid);
    }
    let frames = expected_samples(length_us, u64::from(PCM_RATE)).max(1);
    usize::try_from(frames).map_err(|_| VideoError::Limit)
}

/// Where `start_us` into the sound is, in seconds from the file's start.
fn seek(m: &AudioMetadata, start_us: u64) -> String {
    let offset = u64::try_from(m.stream_start.saturating_sub(m.format_start)).unwrap_or(0);
    seconds(offset.saturating_add(start_us))
}

fn seconds(us: u64) -> String {
    format!("{}.{:06}", us / 1_000_000, us % 1_000_000)
}

fn common_args(demuxer: &'static str) -> Vec<std::ffi::OsString> {
    let mut args = process::args(&["-hide_banner", "-loglevel", "error", "-nostdin", "-max_alloc", "134217728"]);
    args.extend(probe::audio_input_args(demuxer));
    args
}

fn output_args(m: &AudioMetadata, channels: &[&str], rate: &str) -> Vec<std::ffi::OsString> {
    let map = format!("0:{}", m.stream);
    let mut args = process::args(&["-map", &map, "-vn", "-sn", "-dn"]);
    args.extend(process::args(channels));
    args.extend(process::args(&["-ar", rate, "-c:a", "pcm_s16le", "-f", "s16le", "pipe:1"]));
    args
}

pub(super) fn waveform_args(demuxer: &'static str, m: &AudioMetadata, path: &Path, rate: u64) -> Vec<std::ffi::OsString> {
    let mut args = common_args(demuxer);
    args.extend(process::args(&["-i"]));
    args.push(path.as_os_str().to_owned());
    args.extend(output_args(m, &["-ac", "1"], &rate.to_string()));
    args
}

pub(super) fn pcm_args(demuxer: &'static str, m: &AudioMetadata, path: &Path, start_us: u64, length_us: u64) -> Vec<std::ffi::OsString> {
    let mut args = common_args(demuxer);
    // Seeking an audio input decodes from just before and drops samples up to the time.
    args.extend(process::args(&["-ss", &seek(m, start_us), "-i"]));
    args.push(path.as_os_str().to_owned());
    args.extend(process::args(&["-t", &seconds(length_us)]));
    // FFmpeg's own upmix plays a mono sound 3 dB quieter on each side: copy it to both instead.
    let channels: &[&str] = if m.channels == 1 { &["-af", "pan=stereo|c0=c0|c1=c0"] } else { &["-ac", "2"] };
    args.extend(output_args(m, channels, &PCM_RATE.to_string()));
    args
}

/// The peak of each of `buckets` slices of mono s16le `bytes`, laid over at least
/// `expected` samples: a sound that decodes short leaves its last slices silent.
pub(super) fn peaks(bytes: &[u8], buckets: usize, expected: u64) -> Result<Waveform> {
    if buckets == 0 || buckets > MAX_WAVEFORM_BUCKETS {
        return Err(VideoError::Invalid);
    }
    let mut peaks = vec![0u8; buckets];
    let count = u64::try_from(bytes.len() / 2).map_err(|_| VideoError::Limit)?;
    let total = count.max(expected).max(1);
    let slices = u64::try_from(buckets).map_err(|_| VideoError::Limit)?;
    for (i, sample) in (0u64..).zip(bytes.as_chunks::<2>().0) {
        let level = loudness(i16::from_le_bytes(*sample));
        let bucket = usize::try_from(u128::from(i).saturating_mul(u128::from(slices)) / u128::from(total)).unwrap_or(usize::MAX);
        if let Some(peak) = peaks.get_mut(bucket) {
            *peak = (*peak).max(level);
        }
    }
    Ok(Waveform { peaks })
}

/// A sample's distance from silence, from 0 to 255; any sound at all is at least 1.
fn loudness(sample: i16) -> u8 {
    let level = u32::from(sample.unsigned_abs());
    u8::try_from((level * 255).div_ceil(32768)).unwrap_or(u8::MAX)
}

/// The whole frames of s16le stereo `bytes`, no more than `frames` of them.
pub(super) fn pcm_samples(bytes: &[u8], frames: usize) -> Result<Arc<[i16]>> {
    let whole = (bytes.len() / PCM_FRAME_BYTES).min(frames);
    let used = bytes.get(..whole.saturating_mul(PCM_FRAME_BYTES)).ok_or(VideoError::Process)?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(used.len() / 2).map_err(|_| VideoError::Limit)?;
    samples.extend(used.as_chunks::<2>().0.iter().map(|s| i16::from_le_bytes(*s)));
    Ok(Arc::from(samples))
}

#[cfg(test)]
mod tests;
