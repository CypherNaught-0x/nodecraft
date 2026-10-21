//! The sound device: a bounded buffer of decoded sound that the device's callback plays from.
//!
//! The callback runs on the platform's realtime thread. It only takes the buffer's lock (held
//! by the UI for a short copy at most), resamples [`PCM_RATE`] stereo to the device's rate and
//! channels, and plays silence when the buffer runs dry; it never panics and never allocates.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use nodecraft_media::video::PCM_RATE;
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering},
};

/// Decoded frames buffered at most: 8 s; more is dropped (the player never asks for that much).
pub(crate) const MAX_FRAMES: usize = 8 * PCM_RATE as usize;
/// Frames copied in per lock, so the callback never waits long.
const PUSH_FRAMES: usize = 4_096;

const NO_DEVICE: &str = "No sound output device found";
const CANT_OPEN: &str = "Can't open the sound device";

/// How long `frames` frames at [`PCM_RATE`] last, in microseconds.
pub(crate) fn us(frames: u64) -> u64 {
    u64::try_from(u128::from(frames).saturating_mul(1_000_000) / u128::from(PCM_RATE)).unwrap_or(u64::MAX)
}

/// Stereo [`PCM_RATE`] frames waiting to play, and how many have.
pub(crate) struct Ring {
    frames: VecDeque<[i16; 2]>,
    /// Decoded frames per device frame.
    step: f64,
    /// How far the device is between the front frame and the next.
    frac: f64,
    played: u64,
}

impl Ring {
    pub(crate) fn new(device_rate: u32) -> Self {
        let step = if device_rate == 0 { 1.0 } else { f64::from(PCM_RATE) / f64::from(device_rate) };
        Self { frames: VecDeque::with_capacity(MAX_FRAMES), step, frac: 0.0, played: 0 }
    }

    /// Adds the whole frames of interleaved stereo `samples`, as many as there's room for.
    pub(crate) fn push(&mut self, samples: &[i16]) -> usize {
        let room = MAX_FRAMES.saturating_sub(self.frames.len());
        let before = self.frames.len();
        self.frames.extend(samples.as_chunks::<2>().0.iter().take(room).copied());
        self.frames.len().saturating_sub(before)
    }

    /// The next device frame, left and right in -1..=1; `None` when there's nothing to play.
    pub(crate) fn next(&mut self) -> Option<[f32; 2]> {
        let a = *self.frames.front()?;
        let b = self.frames.get(1).copied().unwrap_or(a);
        let t = self.frac as f32;
        let mix = |a: i16, b: i16| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) / 32_768.0;
        let out = [mix(a[0], b[0]), mix(a[1], b[1])];
        self.frac += self.step;
        while self.frac >= 1.0 {
            if self.frames.pop_front().is_none() {
                self.frac = 0.0;
                break;
            }
            self.played = self.played.saturating_add(1);
            self.frac -= 1.0;
        }
        Some(out)
    }

    /// Forgets what's waiting, and how much has played (a seek).
    pub(crate) fn reset(&mut self) {
        self.frames.clear();
        self.frac = 0.0;
        self.played = 0;
    }

    pub(crate) fn buffered(&self) -> usize {
        self.frames.len()
    }

    pub(crate) fn played(&self) -> u64 {
        self.played
    }
}

/// Plays one device frame from `ring` into each `channels`-wide frame of `data`.
pub(crate) fn fill<T: SizedSample + FromSample<f32>>(data: &mut [T], channels: usize, ring: &Mutex<Ring>) {
    let mut ring = ring.lock().unwrap_or_else(PoisonError::into_inner);
    for frame in data.chunks_mut(channels.max(1)) {
        let [left, right] = ring.next().unwrap_or([0.0, 0.0]);
        match frame {
            [mono] => *mono = T::from_sample((left + right) * 0.5),
            [l, r, rest @ ..] => {
                *l = T::from_sample(left);
                *r = T::from_sample(right);
                for other in rest {
                    *other = T::EQUILIBRIUM;
                }
            }
            [] => {}
        }
    }
}

/// An open sound device playing from a [`Ring`]; closing it is dropping it.
pub(crate) struct Output {
    ring: Arc<Mutex<Ring>>,
    failed: Arc<AtomicBool>,
    _stream: cpal::Stream,
}

impl Output {
    /// Opens the default output device, or says why it can't.
    pub(crate) fn open() -> Result<Self, &'static str> {
        let device = cpal::default_host().default_output_device().ok_or(NO_DEVICE)?;
        let supported = config(&device)?;
        let config = supported.config();
        let ring = Arc::new(Mutex::new(Ring::new(config.sample_rate)));
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config, &ring, &failed),
            SampleFormat::I16 => build::<i16>(&device, config, &ring, &failed),
            SampleFormat::I32 => build::<i32>(&device, config, &ring, &failed),
            SampleFormat::U16 => build::<u16>(&device, config, &ring, &failed),
            SampleFormat::U8 => build::<u8>(&device, config, &ring, &failed),
            SampleFormat::F64 => build::<f64>(&device, config, &ring, &failed),
            _ => Err(CANT_OPEN),
        }?;
        stream.play().map_err(|_| CANT_OPEN)?;
        Ok(Self { ring, failed, _stream: stream })
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues interleaved stereo [`PCM_RATE`] `samples` to play after what's waiting.
    pub(crate) fn push(&self, samples: &[i16]) {
        for piece in samples.chunks(PUSH_FRAMES * 2) {
            self.ring().push(piece);
        }
    }

    /// Decoded frames played since it opened or was last reset.
    pub(crate) fn played(&self) -> u64 {
        self.ring().played()
    }

    /// Decoded frames waiting to play.
    pub(crate) fn buffered(&self) -> u64 {
        u64::try_from(self.ring().buffered()).unwrap_or(u64::MAX)
    }

    pub(crate) fn reset(&self) {
        self.ring().reset();
    }

    /// The device went away or broke; playing again opens it afresh.
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// The device's stereo [`PCM_RATE`] config in a common format if it has one, else its default.
fn config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, &'static str> {
    let preferred = device.supported_output_configs().ok().and_then(|ranges| {
        ranges
            .filter(|r| r.channels() > 0 && matches!(r.sample_format(), SampleFormat::F32 | SampleFormat::I16))
            .filter_map(|r| r.try_with_sample_rate(PCM_RATE))
            .min_by_key(|c| (c.channels() != 2, c.sample_format() != SampleFormat::F32))
    });
    match preferred {
        Some(config) => Ok(config),
        None => device.default_output_config().map_err(|_| CANT_OPEN),
    }
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    ring: &Arc<Mutex<Ring>>,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, &'static str> {
    let channels = usize::from(config.channels);
    let (ring, failed) = (Arc::clone(ring), Arc::clone(failed));
    let data = move |data: &mut [T], _: &cpal::OutputCallbackInfo| fill(data, channels, &ring);
    let error = move |error: cpal::Error| {
        if fatal(error.kind()) {
            failed.store(true, Ordering::Relaxed);
        }
    };
    device.build_output_stream::<T, _, _>(config, data, error, None).map_err(|_| CANT_OPEN)
}

/// Whether the stream stops playing after an error of this kind.
fn fatal(kind: cpal::ErrorKind) -> bool {
    !matches!(kind, cpal::ErrorKind::DeviceChanged | cpal::ErrorKind::Xrun | cpal::ErrorKind::RealtimeDenied)
}
