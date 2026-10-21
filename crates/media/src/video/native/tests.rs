use super::*;
fn wait<T>(job: &mut VideoJob<T>) -> Result<T> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(r) = job.try_result()? {
            return Ok(r);
        }
        if Instant::now() >= deadline {
            job.cancel();
            return Err(VideoError::Timeout);
        }
        thread::sleep(Duration::from_millis(5));
    }
}
fn service() -> VideoService {
    service_limits(VideoLimits::default())
}
fn service_limits(limits: VideoLimits) -> VideoService {
    let paths = ToolPaths::new(
        PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").expect("explicit ffmpeg test path")),
        PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").expect("explicit ffprobe test path")),
    )
    .unwrap();
    let mut opening = VideoService::begin_open(ServiceConfig::new(paths, std::env::temp_dir(), limits).unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(s) = opening.try_result().unwrap() {
            return s;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn compositing_is_linear_and_preserves_opaque_bytes() {
    let mut out = [0; 9];
    composite(&[17, 31, 255, 255, 255, 0, 0, 0, 255, 255, 255, 128], &mut out, [0, 0, 0]).unwrap();
    assert_eq!(out.get(..6), Some([17, 31, 255, 0, 0, 0].as_slice()));
    assert_eq!(out.get(6), Some(&188));
}
#[test]
fn quota_exact_and_one_short_release() {
    let b = Arc::new(Budget { used: Mutex::new(Usage::default()), limits: VideoLimits::default().lower_retained(8).unwrap() });
    let a = b.reserve(Usage { bytes: 8, ..Usage::default() }).unwrap();
    assert!(b.reserve(Usage { bytes: 1, ..Usage::default() }).is_err());
    drop(a);
    assert_eq!(b.used.lock().unwrap().bytes, 0);
}
#[test]
#[ignore = "requires explicitly configured user-installed tools; no download"]
fn actual_render_decode_scrub_and_cleanup() {
    let service = service();
    let spec = RenderSpec::new(16, 16, (1, 1), 3, [0, 0, 0]).unwrap();
    let (mut sender, mut job) = service.render(spec).unwrap();
    let send_deadline = Instant::now() + Duration::from_secs(10);
    for (i, rgb) in [[255, 0, 0], [0, 255, 0], [0, 0, 255]].into_iter().enumerate() {
        let rgba: Vec<u8> = std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], 256).flatten().collect();
        let mut packet = FramePacket { index: u32::try_from(i).unwrap(), rgba: Arc::from(rgba) };
        loop {
            assert!(Instant::now() < send_deadline, "bounded send deadline");
            match sender.try_send(packet) {
                Ok(()) => break,
                Err(e) if e.error == VideoError::Busy => {
                    packet = e.packet;
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("frame send failed: {}", e.error),
            }
        }
    }
    loop {
        assert!(Instant::now() < send_deadline, "bounded finish deadline");
        match sender.finish() {
            Ok(()) => break,
            Err(s) => {
                sender = s;
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    let artifact = wait(&mut job).unwrap();
    assert_eq!(artifact.frames, 3);
    assert_eq!(artifact.asset.metadata.color.assumed_fields, 0);
    for (i, channel) in [0, 1, 2].into_iter().enumerate() {
        let mut j = service.frame(artifact.asset(), FrameRequest::new(u64::try_from(i).unwrap() * 1_000_000, 16, 16).unwrap()).unwrap();
        let f = wait(&mut j).unwrap();
        assert_eq!((f.width, f.height), (16, 16));
        let pixel = f.pixels().get(..4).unwrap();
        assert!(pixel[channel] > 240, "actual decoded primary {pixel:?}");
        assert_eq!(pixel[3], 255);
        let mut hit = service.frame(artifact.asset(), FrameRequest::new(u64::try_from(i).unwrap() * 1_000_000, 16, 16).unwrap()).unwrap();
        assert!(f.shares_pixels(&wait(&mut hit).unwrap()));
    }
    let mut eos = service.frame(artifact.asset(), FrameRequest::new(3_000_000, 16, 16).unwrap()).unwrap();
    assert_eq!(wait(&mut eos).err(), Some(VideoError::NoFrame));
    drop(eos);
    drop(job);
    let ticket = service.begin_shutdown();
    let report = ticket.try_result().unwrap();
    assert!(report.cleanup_remaining);
    drop(artifact);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = ticket.try_result().unwrap();
        if !r.cleanup_remaining {
            assert!(r.children_stopped);
            break;
        }
        assert!(Instant::now() < deadline, "cleanup {r:?}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn authored_input(untagged: bool, geometry: bool) -> Vec<u8> {
    authored_rotation(untagged, geometry, 90)
}
fn authored_rotation(untagged: bool, geometry: bool, angle: u16) -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-video-fixture-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("input.mp4");
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let mut args = process::args(&[
        "-v",
        "error",
        "-nostdin",
        "-y",
        "-filter_threads",
        "1",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        "16x8",
        "-r",
        "4",
        "-i",
        "pipe:0",
        "-vf",
        if geometry { "setpts=if(eq(N\\,2)\\,7.5\\,5+N)/TB,setsar=2/1" } else { "setsar=1/1" },
        "-c:v",
        "libx264",
        "-pix_fmt",
        "yuv420p",
        "-fps_mode",
        "passthrough",
    ]);
    if geometry {
        let input = args.iter().position(|s| s == "-vf").unwrap();
        args.splice(input..input, process::args(&["-f", "lavfi", "-i", "anullsrc=r=8000:cl=mono"]));
        args.extend(process::args(&["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac", "-t", "9", "-copyts", "-avoid_negative_ts", "disabled"]));
    }
    if !untagged {
        args.extend(process::args(&["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]));
    }
    args.push(path.as_os_str().to_owned());
    let input: Vec<u8> = (0..3)
        .flat_map(|frame| {
            (0..8).flat_map(move |y| {
                (0..16).flat_map(move |x| {
                    let quadrant = if y < 4 && x < 8 {
                        0
                    } else if y < 4 {
                        1
                    } else {
                        2
                    };
                    match (quadrant + frame) % 3 {
                        0 => [255, 0, 0],
                        1 => [0, 255, 0],
                        _ => [0, 0, 255],
                    }
                })
            })
        })
        .collect();
    let stop = Arc::new(AtomicBool::new(false));
    let input = process::Input { stop, write: Box::new(move |w| w.write_all(&input).map_err(|_| VideoError::Io)) };
    let cancel = Arc::new(AtomicBool::new(false));
    process::run(
        &tool,
        &args,
        &dir,
        Some(input),
        1024,
        process::Watchdog { cancel: &cancel, closed: &cancel, deadline: Instant::now() + Duration::from_secs(10), counters: None },
        &mut || {},
    )
    .unwrap();
    let read = if geometry {
        let rotated = dir.join("rotated.mp4");
        let mut args = process::args(&["-v", "error", "-nostdin", "-y", "-copyts", "-display_rotation", &angle.to_string(), "-i"]);
        args.push(path.into_os_string());
        args.extend(process::args(&["-c", "copy", "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]));
        args.push(rotated.as_os_str().to_owned());
        process::run(
            &tool,
            &args,
            &dir,
            None,
            1024,
            process::Watchdog { cancel: &cancel, closed: &cancel, deadline: Instant::now() + Duration::from_secs(10), counters: None },
            &mut || {},
        )
        .unwrap();
        rotated
    } else {
        path
    };
    let bytes = fs::read(read).unwrap();
    assert!(bytes.len() < 65536);
    fs::remove_dir_all(dir).unwrap();
    bytes
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_untagged_policy_sar_rotation_vfr_and_nonzero_start() {
    let service = service();
    let bytes: Arc<[u8]> = Arc::from(authored_input(true, false));
    let mut strict = service.import(Arc::clone(&bytes), ColorPolicy::RequireDeclared).map_err(|e| e.0).unwrap();
    assert_eq!(wait(&mut strict).err(), Some(VideoError::Unsupported));
    drop(strict);
    let mut default = service.import(bytes, ColorPolicy::default()).map_err(|e| e.0).unwrap();
    let asset = wait(&mut default).unwrap();
    assert_ne!(asset.metadata.color.assumed_fields, 0);
    let mut frame = service.frame(&asset, FrameRequest::new(0, 16, 8).unwrap()).unwrap();
    assert!(wait(&mut frame).unwrap().pixels().get(..4).unwrap()[0] > 230);
    drop(frame);
    drop(asset);
    drop(default);
    let bytes: Arc<[u8]> = Arc::from(authored_input(false, true));
    let mut import = service.import(bytes, ColorPolicy::RequireDeclared).map_err(|e| e.0).unwrap();
    let asset = wait(&mut import).unwrap();
    assert_eq!(asset.metadata.rotation, 270);
    assert_eq!(asset.metadata.sar, (2, 1));
    assert_eq!(asset.metadata.format_start, 0);
    assert_eq!(asset.metadata.stream_start, 5_000_000);
    for (time, channel) in [(0, 1), (1_250_000, 0)] {
        let mut job = service.frame(&asset, FrameRequest::new(time, 64, 64).unwrap()).unwrap();
        let frame = wait(&mut job).unwrap();
        assert_eq!((frame.width, frame.height), (8, 32));
        let first = frame.pixels().get(..4).unwrap();
        assert!(first[channel] > 230, "stream-relative time {time}, 90 CCW first pixel {first:?}");
    }
    drop(asset);
    drop(import);
    let ticket = service.begin_shutdown();
    assert!(ticket.wait_blocking(Duration::from_secs(10)).is_ok());
}

#[test]
#[ignore = "explicit installed tools only"]
fn actual_cancel_unfinished_render_and_latest_scrub() {
    let service = service();
    let (mut sender, mut render) = service.render(RenderSpec::new(16, 16, (1, 1), 3, [0, 0, 0]).unwrap()).unwrap();
    sender.try_send(FramePacket { index: 0, rgba: Arc::from(vec![255; 1024]) }).map_err(|e| e.error).unwrap();
    render.cancel();
    assert_eq!(wait(&mut render).err(), Some(VideoError::Cancelled));
    drop(sender);
    drop(render);
    let bytes: Arc<[u8]> = Arc::from(authored_input(true, false));
    let mut import = service.import(bytes, ColorPolicy::default()).map_err(|e| e.0).unwrap();
    let asset = wait(&mut import).unwrap();
    let mut scrub = ScrubSession::default();
    let before = service.statistics().0;
    scrub.request(&asset, FrameRequest::new(0, 16, 8).unwrap());
    assert!(scrub.poll(&service).unwrap().is_none());
    let until = Instant::now() + Duration::from_secs(5);
    while service.statistics().0 == before {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    scrub.request(&asset, FrameRequest::new(250_000, 8, 4).unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(f) = scrub.poll(&service).unwrap() {
            assert_eq!(f.requested_time_us, 250_000);
            assert_eq!((f.width, f.height), (8, 4));
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    drop(scrub);
    // Controlled operation-worker barrier: old completion remains in flight until
    // AFTER the newer scrub result has been published, without sleeping to guess.
    let mut old_job = service.frame(&asset, FrameRequest::new(0, 16, 8).unwrap()).unwrap();
    let old_frame = wait(&mut old_job).unwrap();
    drop(old_job);
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let completed = Arc::new(AtomicBool::new(false));
    let (entry, gate, done) = (Arc::clone(&entered), Arc::clone(&release), Arc::clone(&completed));
    let delayed = service
        .submit(move |_, _| {
            entry.store(true, Ordering::Release);
            let until = Instant::now() + Duration::from_secs(10);
            while !gate.load(Ordering::Acquire) {
                if Instant::now() >= until {
                    return Err(VideoError::Timeout);
                }
                thread::sleep(Duration::from_millis(1));
            }
            done.store(true, Ordering::Release);
            Ok(old_frame)
        })
        .unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    while !entered.load(Ordering::Acquire) {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let mut scrub = ScrubSession { active: Some(delayed), pending: None };
    scrub.request(&asset, FrameRequest::new(500_000, 8, 4).unwrap());
    loop {
        if let Some(frame) = scrub.poll(&service).unwrap() {
            assert_eq!(frame.requested_time_us, 500_000);
            assert!(!completed.load(Ordering::Acquire), "old completion remains behind controlled barrier");
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    release.store(true, Ordering::Release);
    while !completed.load(Ordering::Acquire) {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(scrub.poll(&service).unwrap().is_none());
    // Exhaust only transient job admission, then retry the SAME latest request.
    let until = Instant::now() + Duration::from_secs(5);
    while service.usage().unwrap().2 != 1 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let jobs = service.control.budget.limits.jobs - 1;
    let busy = service.control.budget.reserve(Usage { jobs, ..Usage::default() }).unwrap();
    scrub.request(&asset, FrameRequest::new(0, 8, 4).unwrap());
    assert!(scrub.poll(&service).unwrap().is_none());
    assert!(scrub.pending.is_some(), "Busy preserves latest pending request");
    drop(busy);
    loop {
        if let Some(frame) = scrub.poll(&service).unwrap() {
            assert_eq!(frame.requested_time_us, 0);
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    drop(scrub);
    drop(asset);
    drop(import);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}

#[test]
fn shutdown_wait_duration_overflow_returns_owning_ticket() {
    let budget = Arc::new(Budget { used: Mutex::new(Usage::default()), limits: VideoLimits::default() });
    let control = Arc::new(Control {
        counters: Arc::new(process::Counters::default()),
        closed: Arc::new(AtomicBool::new(false)),
        budget,
        id: 0,
        report: Mutex::new(ShutdownReport { cleanup_remaining: true, ..ShutdownReport::default() }),
        render_active: AtomicBool::new(false),
    });
    let failure = ShutdownTicket { control: Arc::clone(&control) }.wait_blocking(Duration::MAX).err().unwrap();
    assert_eq!(failure.error, VideoError::Invalid);
    assert!(Arc::ptr_eq(&failure.ticket.control, &control));
}
#[test]
fn real_frame_aliases_retain_quota_until_last_owner() {
    let budget = Arc::new(Budget { used: Mutex::new(Usage::default()), limits: VideoLimits::default().lower_retained(8).unwrap() });
    let frame = VideoFrame {
        data: Arc::new(FrameData { pixels: vec![1; 8], _lease: budget.reserve(Usage { bytes: 8, ..Usage::default() }).unwrap() }),
        width: 2,
        height: 1,
        requested_time_us: 0,
    };
    let cached = frame.clone();
    let caller = frame.clone();
    drop(frame);
    drop(cached);
    assert_eq!(budget.used.lock().unwrap().bytes, 8);
    assert!(budget.reserve(Usage { bytes: 1, ..Usage::default() }).is_err());
    assert_eq!(caller.pixels(), &[1; 8]);
    drop(caller);
    assert_eq!(budget.used.lock().unwrap().bytes, 0);
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_decode_supplies_active_render_and_retained_cancelled_sender_is_busy() {
    let service = service();
    let bytes: Arc<[u8]> = Arc::from(authored_input(true, false));
    let mut import = service.import(bytes, ColorPolicy::default()).map_err(|e| e.0).unwrap();
    let asset = wait(&mut import).unwrap();
    let spec = RenderSpec::new(16, 8, (4, 1), 3, [0, 0, 0]).unwrap();
    let (mut sender, mut render) = service.render(spec).unwrap();
    let ready_deadline = Instant::now() + Duration::from_secs(5);
    while !sender.admission.writer_ready.load(Ordering::Acquire) {
        assert!(Instant::now() < ready_deadline, "render input worker must be live before decode");
        thread::sleep(Duration::from_millis(1));
    }
    let before = service.statistics();
    assert_eq!(service.render(spec).err().unwrap(), VideoError::Busy);
    assert_eq!(service.statistics(), before);
    let deadline = Instant::now() + Duration::from_secs(15);
    for index in 0..3 {
        let mut decode = service.frame(&asset, FrameRequest::new(u64::from(index) * 250_000, 16, 8).unwrap()).unwrap();
        let frame = wait(&mut decode).unwrap();
        let mut packet = FramePacket { index, rgba: Arc::from(frame.pixels()) };
        loop {
            assert!(Instant::now() < deadline);
            match sender.try_send(packet) {
                Ok(()) => break,
                Err(error) if error.error == VideoError::Busy => {
                    packet = error.packet;
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{}", error.error),
            }
        }
    }
    assert!(sender.finish().is_ok());
    let rendered = wait(&mut render).unwrap();
    assert_eq!(rendered.frames(), 3);
    assert!(service.usage().unwrap().1 < 1024 * 1024, "verified output shrinks maximum disk reservation");
    drop(rendered);
    drop(render);
    let (sender, mut cancelled) = service.render(spec).unwrap();
    cancelled.cancel();
    assert_eq!(wait(&mut cancelled).err(), Some(VideoError::Cancelled));
    assert_eq!(service.render(spec).err().unwrap(), VideoError::Busy);
    drop(sender);
    drop(cancelled);
    let (sender, mut next) = service.render(spec).unwrap();
    next.cancel();
    drop(sender);
    assert_eq!(wait(&mut next).err(), Some(VideoError::Cancelled));
    drop(next);
    drop(asset);
    drop(import);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}

fn isolated_context() -> Context {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-video-state-test-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let config = ServiceConfig::new(
        ToolPaths::new(std::env::current_exe().unwrap(), std::env::current_exe().unwrap()).unwrap(),
        std::env::temp_dir(),
        VideoLimits::default(),
    )
    .unwrap();
    let budget = Arc::new(Budget { used: Mutex::new(Usage::default()), limits: config.limits });
    let base = Arc::new(budget.reserve(Usage::default()).unwrap());
    Context {
        _base: base,
        config,
        dir,
        control: Arc::new(Control {
            counters: Arc::new(process::Counters::default()),
            closed: Arc::new(AtomicBool::new(false)),
            budget,
            id,
            report: Mutex::new(ShutdownReport::default()),
            render_active: AtomicBool::new(false),
        }),
        registry: Arc::new(Mutex::new(RecordRegistry { records: Vec::with_capacity(1024), next: 0 })),
        cache: Arc::new(Mutex::new(FrameCache::default())),
    }
}
#[test]
fn cleanup_backoff_retains_file_quota_and_creation_unwind_is_cleanup_eligible() {
    let mut context = isolated_context();
    let (pin, path) = context.record(17).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    drop(pin);
    context.cleanup();
    let retry = context.registry.lock().unwrap().records.first().unwrap().retry;
    assert_eq!(context.registry.lock().unwrap().records.first().unwrap().delay, Duration::from_millis(200));
    context.cleanup();
    assert_eq!(context.registry.lock().unwrap().records.first().unwrap().retry, retry, "no hot retry before backoff");
    assert_eq!(context.control.budget.used.lock().unwrap().disk, 17);
    fs::remove_dir(&path).unwrap();
    context.registry.lock().unwrap().records.first_mut().unwrap().retry = Instant::now();
    context.cleanup();
    assert_eq!(context.control.budget.used.lock().unwrap().disk, 0);
    let (pin, _) = context.record(9).unwrap();
    context.registry.lock().unwrap().records.first_mut().unwrap().creating = true;
    let registry = Arc::clone(&context.registry);
    let id = pin.id;
    assert!(
        std::panic::catch_unwind(move || {
            let _guard = CreationGuard { registry, id };
            panic!("authored worker unwind");
        })
        .is_err()
    );
    assert!(!context.registry.lock().unwrap().records.first().unwrap().creating);
    drop(pin);
    context.cleanup();
    assert_eq!(context.control.budget.used.lock().unwrap().records, 0);
    fs::remove_dir(&context.dir).unwrap();
}
#[test]
fn expired_stage_refuses_before_child_or_capture_and_directory_guard_cleans_errors() {
    let mut context = isolated_context();
    let counters = Arc::clone(&context.control.counters);
    let cancel = Arc::new(AtomicBool::new(false));
    assert_eq!(context.run(&std::env::current_exe().unwrap(), &[], None, 1024, &cancel, Instant::now()).err(), Some(VideoError::Timeout));
    assert_eq!(counters.spawned.load(Ordering::Relaxed), 0);
    assert_eq!(counters.captured.load(Ordering::Relaxed), 0);
    assert_eq!(context.control.budget.used.lock().unwrap().bytes, 0);
    let path = context.dir.join("owned-startup");
    fs::create_dir(&path).unwrap();
    {
        let _guard = DirectoryGuard(Some(path.clone()));
    }
    assert!(!path.exists());
    fs::remove_dir(&context.dir).unwrap();
}

#[test]
fn poisoned_registry_failed_unlink_keeps_lease_and_worker_fault_drains_queued_pins() {
    let mut context = isolated_context();
    let (pin, path) = context.record(23).unwrap();
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    drop(pin);
    let registry = Arc::clone(&context.registry);
    assert!(
        thread::spawn(move || {
            let _lock = registry.lock().unwrap();
            panic!("authored poisoned registry");
        })
        .join()
        .is_err()
    );
    context.cleanup();
    assert_eq!(context.control.budget.used.lock().unwrap().disk, 23);
    assert!(context.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).records.first().unwrap().payload.is_some());
    fs::remove_dir(&path).unwrap();
    context.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner).records.first_mut().unwrap().retry = Instant::now();
    context.cleanup();
    assert_eq!(context.control.budget.used.lock().unwrap().disk, 0);
    fs::remove_dir(&context.dir).unwrap();
    // Admission is fail-closed after poison; queue-drain proof uses a fresh service.
    let mut context = isolated_context();
    let (pin, _) = context.record(7).unwrap();
    let (tx, rx) = mpsc::sync_channel::<Work>(2);
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    tx.send(Box::new(move |context| {
        assert!(context.control.closed.load(Ordering::Acquire));
        drop(pin);
        let _ = result_tx.send(Err::<(), _>(VideoError::Cancelled));
    }))
    .unwrap();
    let queue = Arc::new(Mutex::new(rx));
    let poison = Arc::clone(&queue);
    let worker = thread::spawn(move || {
        let _lock = poison.lock().unwrap();
        panic!("authored worker failure with queued pin");
    });
    let until = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let budget = Arc::clone(&context.control.budget);
    let directory = context.dir.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let supervisor = thread::spawn(move || {
        context.serve(vec![worker], queue);
        let _ = done_tx.send(());
    });
    done_rx.recv_timeout(Duration::from_secs(3)).expect("bounded worker-fault shutdown acknowledgement");
    supervisor.join().unwrap();
    assert_eq!(result_rx.try_recv().unwrap(), Err(VideoError::Cancelled));
    assert_eq!(budget.used.lock().unwrap().disk, 0);
    assert!(!directory.exists());
}
#[test]
fn tiny_cache_capacity_is_retained_charged_and_eviction_preserves_external_aliases() {
    let mut context = isolated_context();
    let cache_bytes = MAX_CACHE_ENTRIES * std::mem::size_of::<Cached>();
    context._base = Arc::new(context.control.budget.reserve(Usage { bytes: cache_bytes, ..Usage::default() }).unwrap());
    *context.cache.lock().unwrap() = FrameCache::preallocated().unwrap();
    let mut alias = None;
    for index in 0..MAX_CACHE_ENTRIES + 1 {
        let frame = VideoFrame {
            data: Arc::new(FrameData { pixels: vec![1; 4], _lease: context.control.budget.reserve(Usage { bytes: 4, ..Usage::default() }).unwrap() }),
            width: 1,
            height: 1,
            requested_time_us: u64::try_from(index).unwrap(),
        };
        if index == 0 {
            alias = Some(frame.clone());
        }
        context.cache_frame(1, FrameRequest::new(u64::try_from(index).unwrap(), 1, 1).unwrap(), &frame).unwrap();
    }
    let cache = context.cache.lock().unwrap();
    assert_eq!(cache.entries.len(), MAX_CACHE_ENTRIES);
    assert_eq!(cache.entries.capacity(), MAX_CACHE_ENTRIES);
    assert_eq!(cache.bytes, MAX_CACHE_ENTRIES * 132);
    drop(cache);
    assert_eq!(context.control.budget.used.lock().unwrap().bytes, cache_bytes + MAX_CACHE_ENTRIES * 132 + 4);
    let removed = {
        let mut cache = context.cache.lock().unwrap();
        cache.bytes = 0;
        cache.entries.drain(..).collect::<Vec<_>>()
    };
    drop(removed);
    assert_eq!(context.cache.lock().unwrap().entries.capacity(), MAX_CACHE_ENTRIES);
    assert_eq!(context.control.budget.used.lock().unwrap().bytes, cache_bytes + 4);
    drop(alias);
    assert_eq!(context.control.budget.used.lock().unwrap().bytes, cache_bytes);
    fs::remove_dir(&context.dir).unwrap();
}

#[test]
#[ignore = "explicit installed tools only"]
fn actual_all_quarter_rotations_preserve_pixel_orientation() {
    let service = service();
    for (angle, clockwise, dimensions, channel) in [(0, 0, (32, 8), 0), (90, 270, (8, 32), 1), (180, 180, (32, 8), 2), (270, 90, (8, 32), 2)] {
        let bytes: Arc<[u8]> = Arc::from(authored_rotation(false, true, angle));
        let mut import = service.import(bytes, ColorPolicy::RequireDeclared).map_err(|error| error.0).unwrap();
        let asset = wait(&mut import).unwrap();
        assert_eq!(asset.metadata.rotation, clockwise);
        let mut decode = service.frame(&asset, FrameRequest::new(0, 64, 64).unwrap()).unwrap();
        let frame = wait(&mut decode).unwrap();
        assert_eq!((frame.width, frame.height), dimensions);
        assert!(frame.pixels().get(channel).copied().unwrap() > 230, "angle={angle}, pixel={:?}", frame.pixels().get(..4));
        drop(frame);
        drop(decode);
        drop(asset);
        drop(import);
    }
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}

#[test]
fn bounded_public_admission_returns_original_inputs_and_packet_without_side_effects() {
    let mut context = isolated_context();
    let (tx, rx) = mpsc::sync_channel::<Work>(0);
    let service = VideoService { tx, control: Arc::clone(&context.control) };
    let input: Arc<[u8]> = Arc::from([1, 2, 3, 4]);
    let failure = service.import(Arc::clone(&input), ColorPolicy::default()).err().unwrap();
    assert_eq!(failure.0, VideoError::Busy);
    assert!(Arc::ptr_eq(&input, &failure.1));
    assert_eq!(service.usage().unwrap(), (0, 0, 0, 0));
    assert_eq!(service.statistics(), (0, 0));
    drop(rx);
    drop(service);
    fs::remove_dir(&context.dir).unwrap();
    // A queued render owns its envelope before any frame is accepted; invalid
    // packets are returned with the identical immutable payload owner.
    context = isolated_context();
    let (tx, rx) = mpsc::sync_channel::<Work>(1);
    let service = VideoService { tx, control: Arc::clone(&context.control) };
    let spec = RenderSpec::new(2, 2, (1, 1), 1, [0, 0, 0]).unwrap();
    let (mut sender, mut job) = service.render(spec).unwrap();
    let reserved = service.usage().unwrap();
    let rgba: Arc<[u8]> = Arc::from([255; 16]);
    let refusal = sender.try_send(FramePacket { index: 1, rgba: Arc::clone(&rgba) }).err().unwrap();
    assert_eq!(refusal.error, VideoError::Invalid);
    assert!(Arc::ptr_eq(&rgba, &refusal.packet.rgba));
    assert_eq!(service.usage().unwrap(), reserved);
    sender.try_send(FramePacket { index: 0, rgba }).map_err(|error| error.error).unwrap();
    assert_eq!(service.usage().unwrap(), reserved, "packet uses pre-admitted envelope, no double charge");
    assert!(sender.finish().is_ok());
    assert_eq!(service.render(spec).err().unwrap(), VideoError::Busy, "queued operation still owns slot");
    context.control.closed.store(true, Ordering::Release);
    rx.try_recv().unwrap()(&mut context);
    assert_eq!(wait(&mut job).err(), Some(VideoError::Cancelled));
    drop(job);
    assert_eq!(service.usage().unwrap(), (0, 0, 0, 0));
    fs::remove_dir(&context.dir).unwrap();
}

#[test]
fn failed_actual_startup_removes_its_directory() {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let scratch = std::env::temp_dir().join(format!("nodecraft-video-startup-test-{}-{id}", std::process::id()));
    fs::create_dir(&scratch).unwrap();
    let tool = std::env::current_exe().unwrap();
    let config = ServiceConfig::new(ToolPaths::new(tool.clone(), tool).unwrap(), scratch.clone(), VideoLimits::default()).unwrap();
    let mut opening = VideoService::begin_open(config).unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match opening.try_result() {
            Err(_) => break,
            Ok(None) => {}
            Ok(Some(_)) => panic!("test harness is not ffmpeg"),
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    fs::remove_dir(scratch).unwrap();
}

#[test]
#[ignore = "explicit installed tools only"]
fn actual_output_quota_refuses_partial_file_without_becoming_user_cancel() {
    let limits = VideoLimits { output: 64, ..VideoLimits::default() };
    let service = service_limits(limits);
    let spec = RenderSpec::new(16, 16, (1, 1), 3, [0, 0, 0]).unwrap();
    let (mut sender, mut job) = service.render(spec).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    for index in 0..3 {
        let mut packet = FramePacket { index, rgba: Arc::from([255; 1024]) };
        loop {
            assert!(Instant::now() < until);
            match sender.try_send(packet) {
                Ok(()) => break,
                Err(error) if error.error == VideoError::Busy => {
                    packet = error.packet;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) if error.error == VideoError::Closed => break,
                Err(error) => panic!("{}", error.error),
            }
        }
    }
    let _ = sender.finish();
    assert_eq!(wait(&mut job).err(), Some(VideoError::Limit));
    assert!(!job.cancel.load(Ordering::Acquire), "quota enforcement must not impersonate caller cancellation");
    drop(job);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}

/// A clip FFmpeg draws itself: a test card, optionally with a tone, in the given container.
fn generated(name: &str, size: &str, seconds: &str, tone: bool, webm: bool) -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-compose-fixture-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join(if webm { format!("{name}.webm") } else { format!("{name}.mp4") });
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let card = format!("testsrc2=size={size}:rate=10:duration={seconds}");
    let mut args = process::args(&["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", &card]);
    if tone {
        args.extend(process::args(&["-f", "lavfi", "-i", &format!("sine=frequency=440:duration={seconds}")]));
    }
    if webm {
        args.extend(process::args(&["-c:v", "libvpx-vp9", "-deadline", "realtime"]));
    } else {
        args.extend(process::args(&["-c:v", "libx264"]));
    }
    args.extend(process::args(&["-pix_fmt", "yuv420p", "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]));
    if tone {
        args.extend(process::args(&["-c:a", if webm { "libopus" } else { "aac" }]));
    }
    args.push(path.as_os_str().to_owned());
    let status = std::process::Command::new(tool).args(&args).status().unwrap();
    assert!(status.success());
    let bytes = fs::read(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    bytes
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_compose_cuts_clips_that_are_not_whole_frames() {
    let service = service();
    let mut job = service.import(Arc::from(generated("a", "64x48", "2", true, false)), ColorPolicy::default()).ok().unwrap();
    let asset = wait(&mut job).unwrap();
    // 1.001 s at 30 fps would keep 31 frames each if trimmed by time; four cuts, four stray frames.
    let clips = (0..4).map(|_| ComposeClip::new(asset.clone(), 0, 1_001_000).unwrap()).collect();
    let spec = ComposeSpec::new(64, 48, (30, 1), [0, 0, 0], clips).unwrap();
    assert_eq!(spec.duration_us(), 4_000_000);
    let mut job = service.compose(spec).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let video = loop {
        if let Some(video) = job.try_result().unwrap() {
            break video;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    };
    assert!(video.frames().abs_diff(120) <= 1, "{} frames", video.frames());
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_compose_stitches_four_clips_with_fades_crosses_and_sound() {
    let service = service();
    let mut assets = Vec::new();
    for (name, size, tone, webm) in [("a", "64x48", true, false), ("b", "64x48", true, false), ("c", "64x48", false, true), ("d", "48x64", true, false)] {
        let mut job = service.import(Arc::from(generated(name, size, "2", tone, webm)), ColorPolicy::default()).ok().unwrap();
        let asset = wait(&mut job).unwrap();
        assert_eq!(asset.metadata().has_audio(), tone, "{name}");
        assets.push(asset);
    }
    let [a, b, c, d] = <[VideoAsset; 4]>::try_from(assets).ok().unwrap();
    let clips = vec![
        ComposeClip::new(a, 0, 2_000_000).unwrap().with_fades(500_000, 0).unwrap().with_transition(500_000).unwrap(),
        ComposeClip::new(b, 500_000, 2_000_000).unwrap().with_volume(500, false).unwrap(),
        ComposeClip::new(c, 0, 1_500_000).unwrap().with_transition(300_000).unwrap(),
        ComposeClip::new(d, 0, 2_000_000).unwrap().with_fades(0, 500_000).unwrap(),
    ];
    let spec = ComposeSpec::new(64, 48, (10, 1), [0, 0, 0], clips).unwrap();
    assert_eq!(spec.duration_us(), 6_200_000);
    let mut job = service.compose(spec.clone()).unwrap();
    assert_eq!(service.compose(spec).err(), Some(VideoError::Busy), "one render at a time");
    let deadline = Instant::now() + Duration::from_secs(120);
    let video = loop {
        if let Some(video) = job.try_result().unwrap() {
            break video;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    };
    assert!(video.frames().abs_diff(62) <= 2, "{} frames", video.frames());
    let m = video.asset().metadata();
    assert_eq!((m.width, m.height), (64, 48));
    assert!(m.has_audio());
    assert!(m.duration_us.abs_diff(6_200_000) <= 300_000, "{}", m.duration_us);
    let luma = |time_us: u64| {
        let mut job = service.frame(video.asset(), FrameRequest::new(time_us, 64, 48).unwrap()).unwrap();
        let frame = wait(&mut job).unwrap();
        frame.pixels().chunks(4).map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum::<u64>() / (64 * 48)
    };
    assert!(luma(0) < 20, "the fade in starts on black");
    assert!(luma(1_000_000) > 100, "the card shows once faded in");
    // d is portrait, so its frames are letterboxed with black bars at the sides.
    let mut job = service.frame(video.asset(), FrameRequest::new(5_000_000, 64, 48).unwrap()).unwrap();
    let frame = wait(&mut job).unwrap();
    assert!(frame.pixels().get(..4).unwrap().iter().take(3).all(|v| *v < 20), "left bar is background");
    drop(job);
    drop(video);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}
/// A half-second beep FFmpeg writes as 16-bit WAV.
fn beep() -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-beep-fixture-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("beep.wav");
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let args = process::args(&["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "sine=frequency=1000:duration=0.5", "-c:a", "pcm_s16le", "-ar", "44100"]);
    let status = std::process::Command::new(tool).args(&args).arg(&path).status().unwrap();
    assert!(status.success());
    let bytes = fs::read(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    bytes
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_layers_align_sound_to_picture() {
    let service = service();
    let mut job = service.import(Arc::from(generated("main", "64x48", "2", false, false)), ColorPolicy::default()).ok().unwrap();
    let card = wait(&mut job).unwrap();
    let mut job = service.import_audio(Arc::from(beep())).ok().unwrap();
    let sound = wait(&mut job).unwrap();
    assert!(sound.metadata().duration_us.abs_diff(500_000) < 30_000, "{}", sound.metadata().duration_us);
    let mut job = service.stage_still(Arc::from(vec![255u8; 16 * 16 * 4]), 16, 16).ok().unwrap();
    let white = wait(&mut job).unwrap();
    let mut job = service.import_audio(Arc::from(vec![0u8; 64])).ok().unwrap();
    assert_eq!(wait(&mut job).err(), Some(VideoError::Unsupported), "not a sound file");
    // A black main track of 1 s, then the beep and a white square both at 1.5 s; the output runs to 2 s.
    let black = ComposeClip::new(card, 0, 1_000_000).unwrap().with_fades(1_000_000, 0).unwrap();
    let spec = ComposeSpec::new(64, 48, (10, 1), [0, 0, 0], vec![black])
        .unwrap()
        .with_layers(vec![(ComposeClip::audio(sound, 0, 500_000).unwrap(), 1_500_000), (ComposeClip::still(white, 500_000).unwrap(), 1_500_000)])
        .unwrap();
    assert_eq!(spec.duration_us(), 2_000_000);
    let mut job = service.compose(spec).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let video = loop {
        if let Some(video) = job.try_result().unwrap() {
            break video;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    };
    assert!(video.frames().abs_diff(20) <= 1, "{} frames", video.frames());
    let luma = |time_us: u64| {
        let mut job = service.frame(video.asset(), FrameRequest::new(time_us, 64, 48).unwrap()).unwrap();
        let frame = wait(&mut job).unwrap();
        frame.pixels().chunks(4).map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum::<u64>() / (64 * 48)
    };
    assert!(luma(1_400_000) < 20, "nothing shows before 1.5 s");
    assert!(luma(1_600_000) > 100, "the white square shows at 1.5 s");
    // Decode the render's sound and find where the beep starts.
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < video.bytes() {
        let length = usize::try_from((video.bytes() - bytes.len() as u64).min(1024 * 1024)).unwrap();
        let mut job = service.read_chunk(video.asset(), bytes.len() as u64, length).unwrap();
        bytes.extend_from_slice(wait(&mut job).unwrap().bytes());
    }
    let dir = std::env::temp_dir().join(format!("nodecraft-align-{}-{}", std::process::id(), SERVICE_ID.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("out.webm");
    fs::write(&path, &bytes).unwrap();
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let out = std::process::Command::new(tool).args(["-v", "error", "-i"]).arg(&path).args(["-f", "s16le", "-ac", "1", "-ar", "48000", "-"]).output().unwrap();
    fs::remove_dir_all(&dir).unwrap();
    assert!(out.status.success());
    let samples: Vec<i16> = out.stdout.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
    let onset = samples.iter().position(|s| s.unsigned_abs() > 2000).unwrap();
    let onset_us = onset as u64 * 1_000_000 / 48_000;
    assert!(onset_us.abs_diff(1_500_000) < 30_000, "the beep starts with the picture: {onset_us}");
    let end_us = samples.iter().rposition(|s| s.unsigned_abs() > 2000).unwrap() as u64 * 1_000_000 / 48_000;
    assert!(end_us.abs_diff(2_000_000) < 40_000, "{end_us}");
    drop(video);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_compose_places_turns_and_speeds_up_clips() {
    let service = service();
    let mut job = service.import(Arc::from(generated("fast", "64x48", "2", true, false)), ColorPolicy::default()).ok().unwrap();
    let card = wait(&mut job).unwrap();
    let mut job = service.stage_still(Arc::from(vec![255u8; 16 * 16 * 4]), 16, 16).ok().unwrap();
    let white = wait(&mut job).unwrap();
    // Two seconds of the card at twice the speed, shrunk into the left half; a white square,
    // turned and half see-through, in the right half.
    let small = Placement { pos_x: 250, scale: 500, ..Placement::IDENTITY };
    let main = ComposeClip::new(card, 0, 2_000_000).unwrap().with_speed(2000).unwrap().with_placement(small).unwrap();
    assert_eq!(main.length_us(), 1_000_000);
    let square = Placement { pos_x: 750, pos_y: 500, scale: 250, rotation: 45, opacity: 500, ..Placement::IDENTITY };
    let layer = ComposeClip::still(white, 1_000_000).unwrap().with_placement(square).unwrap();
    let spec = ComposeSpec::new(64, 48, (10, 1), [0, 0, 0], vec![main]).unwrap().with_layers(vec![(layer, 0)]).unwrap().with_quality(Quality::Draft);
    assert_eq!(spec.duration_us(), 1_000_000);
    let mut job = service.compose(spec).unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let video = loop {
        if let Some(video) = job.try_result().unwrap() {
            break video;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(20));
    };
    assert!(video.frames().abs_diff(10) <= 1, "{} frames", video.frames());
    assert!(video.asset().metadata().has_audio());
    let mut job = service.frame(video.asset(), FrameRequest::new(500_000, 64, 48).unwrap()).unwrap();
    let frame = wait(&mut job).unwrap();
    let at = |x: usize, y: usize| frame.pixels().get((y * 64 + x) * 4..(y * 64 + x) * 4 + 3).unwrap().iter().map(|v| u32::from(*v)).sum::<u32>();
    assert!(at(2, 4) < 40 && at(61, 2) < 40 && at(16, 44) < 40, "around the card and the square is background");
    assert!(at(16, 24) > 60, "the card shows in the left half: {}", at(16, 24));
    assert!(at(48, 24) > 250 && at(48, 24) < 600, "the square is half see-through: {}", at(48, 24));
    drop(job);
    drop(video);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}
/// Ten minutes at 30 a second, red until 590 s and green after, keyframes every 10 s.
fn long_red_then_green() -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-long-fixture-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join("long.mp4");
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let graph = "color=c=red:s=320x240:r=30:d=590[a];color=c=lime:s=320x240:r=30:d=10[b];[a][b]concat=n=2:v=1:a=0";
    let mut args = process::args(&["-v", "error", "-nostdin", "-y", "-filter_complex", graph]);
    args.extend(process::args(&["-c:v", "libx264", "-preset", "ultrafast", "-g", "300", "-pix_fmt", "yuv420p"]));
    args.extend(process::args(&["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]));
    args.push(path.as_os_str().to_owned());
    let status = std::process::Command::new(tool).args(&args).status().unwrap();
    assert!(status.success());
    let bytes = fs::read(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    bytes
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_a_late_frame_is_exact_and_costs_no_more_than_an_early_one() {
    let service = service();
    let mut job = service.import(Arc::from(long_red_then_green()), ColorPolicy::default()).ok().unwrap();
    let asset = wait(&mut job).unwrap();
    let grab = |time: u64| {
        let started = Instant::now();
        let mut job = service.frame(&asset, FrameRequest::new(time, 32, 24).unwrap()).unwrap();
        let pixel: [u8; 4] = wait(&mut job).unwrap().pixels().get(..4).unwrap().try_into().unwrap();
        (pixel, started.elapsed())
    };
    let (early, early_took) = grab(1_000_000);
    // The last red frame starts at 589.9666 s; the first green one at 590 s, between keyframes.
    let (red, _) = grab(589_950_000);
    let (green, late_took) = grab(590_000_000);
    assert!(early[0] > 200 && early[1] < 60, "{early:?}");
    assert!(red[0] > 200 && red[1] < 60, "the frame before the switch is red: {red:?}");
    assert!(green[1] > 200 && green[0] < 60, "the frame at the switch is green: {green:?}");
    // Decoding from the start would read 17,700 frames for the late grab.
    assert!(late_took < early_took * 4 + Duration::from_secs(1), "early {early_took:?}, late {late_took:?}");
}
/// Three seconds at 10 a second, red then blue from 1.5 s, with a tone; MP4 with AAC or WebM with Opus.
fn hard_cut(webm: bool) -> Vec<u8> {
    let id = SERVICE_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nodecraft-cut-fixture-{}-{id}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    let path = dir.join(if webm { "cut.webm" } else { "cut.mp4" });
    let tool = PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap());
    let graph = "color=c=red:s=64x48:r=10:d=1.5[a];color=c=blue:s=64x48:r=10:d=1.5[b];[a][b]concat=n=2:v=1:a=0[v];sine=frequency=440:duration=3[s]";
    let mut args = process::args(&["-v", "error", "-nostdin", "-y", "-filter_complex", graph, "-map", "[v]", "-map", "[s]"]);
    if webm {
        args.extend(process::args(&["-c:v", "libvpx-vp9", "-deadline", "realtime", "-c:a", "libopus"]));
    } else {
        args.extend(process::args(&["-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"]));
    }
    args.extend(process::args(&["-pix_fmt", "yuv420p", "-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"]));
    args.push(path.as_os_str().to_owned());
    let status = std::process::Command::new(tool).args(&args).status().unwrap();
    assert!(status.success());
    let bytes = fs::read(&path).unwrap();
    fs::remove_dir_all(&dir).unwrap();
    bytes
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_scenes_find_a_hard_cut_and_nothing_else() {
    let service = service();
    for webm in [false, true] {
        let mut job = service.import(Arc::from(hard_cut(webm)), ColorPolicy::default()).ok().unwrap();
        let asset = wait(&mut job).unwrap();
        let mut job = service.scenes(&asset, 300).unwrap();
        assert_eq!(wait(&mut job).unwrap(), vec![1_500_000], "webm {webm}");
        assert_eq!(service.scenes(&asset, 49).err(), Some(VideoError::Invalid));
        assert_eq!(service.scenes(&asset, 1001).err(), Some(VideoError::Invalid));
    }
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}
#[test]
#[ignore = "explicit installed tools only"]
fn actual_extract_audio_writes_a_probed_ogg_copied_or_encoded() {
    let service = service();
    for webm in [false, true] {
        let mut job = service.import(Arc::from(hard_cut(webm)), ColorPolicy::default()).ok().unwrap();
        let video = wait(&mut job).unwrap();
        let mut job = service.extract_audio(&video).unwrap();
        let sound = wait(&mut job).unwrap();
        let m = sound.asset().metadata();
        assert!(m.duration_us.abs_diff(3_000_000) < 60_000, "webm {webm}: {} µs", m.duration_us);
        assert_eq!(m.codec, "opus");
        // AAC is encoded to stereo; Opus is copied as it was, one channel.
        assert_eq!((m.sample_rate, m.channels), (48_000, if webm { 1 } else { 2 }), "webm {webm}");
        let mut job = service.read_audio_chunk(sound.asset(), 0, 4).unwrap();
        assert_eq!(wait(&mut job).unwrap().bytes(), b"OggS");
        assert!(sound.bytes() > 0);
    }
    let mut job = service.import(Arc::from(generated("silent", "64x48", "1", false, false)), ColorPolicy::default()).ok().unwrap();
    let silent = wait(&mut job).unwrap();
    assert_eq!(service.extract_audio(&silent).err(), Some(VideoError::Invalid));
    drop(silent);
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}
