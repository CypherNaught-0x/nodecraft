use super::*;
use crate::audio::output::Ring;
use std::sync::atomic::AtomicU64;

struct Device {
    ring: Mutex<Ring>,
    callbacks: AtomicU64,
    failed: AtomicBool,
    opens: AtomicU64,
}
impl Default for Device {
    fn default() -> Self {
        Self { ring: Mutex::new(Ring::new(48_000)), callbacks: AtomicU64::new(0), failed: AtomicBool::new(false), opens: AtomicU64::new(0) }
    }
}
struct FakeSink(Arc<Device>);
impl Sink for FakeSink {
    fn push(&mut self, samples: &[i16]) {
        lock(&self.0.ring).push(samples);
    }
    fn played(&self) -> u64 {
        lock(&self.0.ring).played()
    }
    fn buffered(&self) -> u64 {
        lock(&self.0.ring).buffered() as u64
    }
    fn reset(&mut self) {
        lock(&self.0.ring).reset();
    }
    fn failed(&self) -> bool {
        self.0.failed.load(Ordering::Relaxed)
    }
    fn callbacks(&self) -> u64 {
        self.0.callbacks.load(Ordering::Relaxed)
    }
}
struct Decode {
    frames: u64,
    pending: bool,
    requests: Mutex<Vec<u64>>,
    cancelled: Arc<AtomicU64>,
}
impl Decode {
    fn new(frames: u64, pending: bool) -> Self {
        Self { frames, pending, requests: Mutex::new(Vec::new()), cancelled: Arc::new(AtomicU64::new(0)) }
    }
}
struct Job {
    frames: usize,
    pending: bool,
    cancelled: Arc<AtomicU64>,
}
impl Drop for Job {
    fn drop(&mut self) {
        if self.pending {
            self.cancelled.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl ChunkJob for Job {
    fn poll(&mut self, sink: &mut dyn Sink) -> Result<Option<u64>, VideoError> {
        if self.pending {
            return Ok(None);
        }
        sink.push(&vec![17; self.frames * 2]);
        Ok(Some(self.frames as u64))
    }
}
impl Decoder for Decode {
    fn request(&self, start_us: u64, length_us: u64) -> Result<Box<dyn ChunkJob>, VideoError> {
        lock(&self.requests).push(start_us);
        let start = start_us * 48_000 / 1_000_000;
        let frames = self.frames.saturating_sub(start).min(length_us * 48_000 / 1_000_000) as usize;
        Ok(Box::new(Job { frames, pending: self.pending, cancelled: Arc::clone(&self.cancelled) }))
    }
}

fn until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "audio worker made no bounded progress");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn feeder() -> (Feeder, Arc<Device>) {
    let device = Arc::new(Device::default());
    let output = Arc::clone(&device);
    let feeder = Feeder::spawn(Box::new(move || {
        output.opens.fetch_add(1, Ordering::Relaxed);
        lock(&output.ring).reset();
        Ok(Box::new(FakeSink(Arc::clone(&output))))
    }))
    .unwrap();
    (feeder, device)
}

#[test]
fn the_worker_feeds_past_eight_seconds_without_ui_frames_and_uses_decoded_eof() {
    let (feeder, device) = feeder();
    let source = Arc::new(Decode::new(600_000, false)); // 12.5 s, despite a one-second header.
    feeder.request(source.clone(), 1_000_000, 0).unwrap();
    // This simulates only the device callback. There is no egui context or Audios::poll.
    until(|| {
        device.callbacks.fetch_add(1, Ordering::Relaxed);
        let mut ring = lock(&device.ring);
        let buffered = ring.buffered();
        for _ in 0..buffered.min(96_000) {
            ring.next();
        }
        assert!(buffered <= super::super::output::MAX_FRAMES);
        drop(ring);
        feeder.status().is_some_and(|status| !status.playing)
    });
    let status = feeder.status().unwrap();
    assert_eq!((status.position_us, status.duration_us, status.notice), (12_500_000, 12_500_000, None));
    assert!(lock(&source.requests).iter().any(|start| *start >= 10_000_000));
    assert_eq!(device.opens.load(Ordering::Relaxed), 1);
}

#[test]
fn dragging_seeks_cancels_old_work_and_decodes_only_the_latest_settled_position() {
    let (feeder, device) = feeder();
    let source = Arc::new(Decode::new(480_000, true));
    feeder.request(source.clone(), 10_000_000, 0).unwrap();
    until(|| lock(&source.requests).len() == 1);
    for target in 0..400 {
        feeder.scrub(target * 1000);
    }
    until(|| source.cancelled.load(Ordering::Relaxed) == 1);
    assert_eq!(*lock(&source.requests), vec![0], "a held drag never decodes intermediate targets");
    feeder.finish_seek();
    until(|| lock(&source.requests).len() == 2);
    assert_eq!(*lock(&source.requests), vec![0, 399_000]);
    assert_eq!(source.cancelled.load(Ordering::Relaxed), 1);
    assert_eq!(feeder.status().unwrap().position_us, 399_000);
    assert_eq!(device.opens.load(Ordering::Relaxed), 1, "seeking reuses its device");
    feeder.stop();
    until(|| source.cancelled.load(Ordering::Relaxed) == 2);
    assert!(feeder.status().is_none());
}

#[test]
fn a_blocked_device_open_does_not_hold_the_request_mailbox_or_start_a_cancelled_decode() {
    let (entered, opening) = std::sync::mpsc::sync_channel(1);
    let (release, gate) = std::sync::mpsc::sync_channel(1);
    let device = Arc::new(Device::default());
    let mut gate = Some(gate);
    let feeder = Feeder::spawn(Box::new(move || {
        if let Some(gate) = gate.take() {
            entered.send(()).unwrap();
            gate.recv().unwrap();
        }
        Ok(Box::new(FakeSink(Arc::clone(&device))))
    }))
    .unwrap();
    let source = Arc::new(Decode::new(480_000, false));
    feeder.request(source.clone(), 10_000_000, 0).unwrap();
    opening.recv_timeout(Duration::from_secs(5)).unwrap();
    feeder.stop(); // Returns before the test releases the stuck device call.
    assert!(feeder.status().is_none());
    let shared = Arc::clone(&feeder.shared);
    drop(feeder);
    release.send(()).unwrap();
    until(|| shared.dead.load(Ordering::Acquire));
    assert!(lock(&source.requests).is_empty());
}

#[test]
fn losing_app_frames_during_a_drag_eventually_commits_the_latest_target() {
    let (feeder, _) = feeder();
    let source = Arc::new(Decode::new(480_000, true));
    feeder.request(source.clone(), 10_000_000, 0).unwrap();
    until(|| lock(&source.requests).len() == 1);
    feeder.scrub(1_000_000);
    // Advance only the inactivity deadline; no app frame or release event is delivered.
    lock(&feeder.shared.desired).request.as_mut().unwrap().held = Some(Instant::now() - Duration::from_secs(1));
    feeder.shared.wake.notify_one();
    until(|| lock(&source.requests).len() == 2);
    assert_eq!(*lock(&source.requests), vec![0, 1_000_000]);
    assert!(lock(&feeder.shared.desired).request.as_ref().unwrap().held.is_none());
}

#[test]
fn seeking_keeps_a_length_already_observed_beyond_the_container_hint() {
    let (feeder, _) = feeder();
    let source = Arc::new(Decode::new(600_000, false));
    feeder.request(source, 1_000_000, 0).unwrap();
    until(|| feeder.status().is_some_and(|status| status.duration_us >= 4_000_000));
    feeder.scrub(500_000);
    assert!(feeder.status().unwrap().duration_us >= 4_000_000);
    feeder.finish_seek();
    assert!(feeder.status().unwrap().duration_us >= 4_000_000);
}

#[test]
fn a_device_without_callbacks_has_a_terminal_failure_without_an_error_callback() {
    let device = Arc::new(Device::default());
    let source = Arc::new(Decode::new(480_000, true));
    let started = Instant::now();
    let request = Request { decoder: source, duration_us: 10_000_000, from_us: 0, play: 1, settle_until: None, held: None };
    let mut active = Active {
        play: 1,
        epoch: 1,
        request,
        output: Box::new(FakeSink(device)),
        job: None,
        next_us: 0,
        length_us: CHUNK_US,
        ended: false,
        retry: None,
        callbacks: 0,
        callback_at: started,
    };
    let status = active.poll(started + STALL, STALL.as_secs_f64());
    assert!(!status.playing);
    assert_eq!(status.notice, Some(DEVICE_STOPPED));
}

#[test]
fn a_failed_device_open_records_a_terminal_outcome_and_can_be_retried() {
    let feeder = Feeder::spawn(Box::new(|| Err("No sound output device found"))).unwrap();
    let source = Arc::new(Decode::new(48_000, false));
    feeder.request(source.clone(), 1_000_000, 0).unwrap();
    until(|| feeder.status().is_some_and(|status| !status.playing));
    assert_eq!(feeder.status().unwrap().notice, Some("No sound output device found"));
    feeder.request(source, 1_000_000, 200_000).unwrap();
    until(|| feeder.status().is_some_and(|status| !status.playing));
    assert_eq!(feeder.status().unwrap().position_us, 200_000);
}

#[test]
fn dropping_the_host_cancels_its_job_and_releases_the_decoder_without_an_app_frame() {
    let (feeder, _) = feeder();
    let source = Arc::new(Decode::new(480_000, true));
    let weak = Arc::downgrade(&source);
    let cancelled = Arc::clone(&source.cancelled);
    feeder.request(source.clone(), 10_000_000, 0).unwrap();
    until(|| lock(&source.requests).len() == 1);
    let shared = Arc::clone(&feeder.shared);
    drop(source);
    drop(feeder);
    until(|| shared.dead.load(Ordering::Acquire));
    assert_eq!(cancelled.load(Ordering::Relaxed), 1);
    assert!(weak.upgrade().is_none());
}
