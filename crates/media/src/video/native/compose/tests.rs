use super::super::tests::{isolated_context, service, wait};
use super::*;

#[test]
fn script_syntax_is_observed_and_only_parser_failures_try_the_legacy_option() {
    let mut tried = Vec::new();
    assert_eq!(
        script_option(|option| {
            tried.push(option);
            Ok(())
        }),
        Ok(SCRIPT_OPTIONS[0])
    );
    assert_eq!(tried, vec![SCRIPT_OPTIONS[0]]);
    tried.clear();
    assert_eq!(
        script_option(|option| {
            tried.push(option);
            if option == SCRIPT_OPTIONS[0] { Err(VideoError::Process) } else { Ok(()) }
        }),
        Ok(SCRIPT_OPTIONS[1])
    );
    assert_eq!(tried, SCRIPT_OPTIONS);
    assert_eq!(script_option(|_| Err(VideoError::Process)), Err(VideoError::Unsupported));
    for error in [VideoError::Cancelled, VideoError::Timeout, VideoError::Io, VideoError::Limit, VideoError::Unavailable, VideoError::Closed] {
        let mut calls = 0;
        assert_eq!(
            script_option(|_| {
                calls += 1;
                Err(error)
            }),
            Err(error)
        );
        assert_eq!(calls, 1);
    }
}

#[test]
fn script_records_are_charged_pinned_and_cleaned_on_success_and_probe_errors() {
    let mut ctx = isolated_context();
    let graph = "color=s=16x16[vout];anullsrc[aout]";
    for error in [None, Some(VideoError::Cancelled), Some(VideoError::Process)] {
        let result = (|| {
            let (pin, path) = ctx.compose_script(graph)?;
            assert_eq!(fs::read(&path).unwrap(), graph.as_bytes());
            assert_eq!(ctx.control.budget.used.lock().unwrap().disk, graph.len() as u64);
            ctx.cleanup();
            assert!(path.exists(), "the live pin keeps the script readable by the child");
            if let Some(error) = error {
                if error == VideoError::Cancelled {
                    let cancelled = Arc::new(AtomicBool::new(true));
                    ctx.compose_script_option(&ctx.compose_name(&pin)?, &cancelled, Instant::now() + Duration::from_secs(10))?;
                } else {
                    // The test binary is an available process, but neither ffmpeg syntax works.
                    ctx.compose_script_option(&ctx.compose_name(&pin)?, &Arc::new(AtomicBool::new(false)), Instant::now() + Duration::from_secs(10))?;
                }
            }
            Ok(())
        })();
        assert_eq!(result.err(), error.map(|e| if e == VideoError::Process { VideoError::Unsupported } else { e }));
        ctx.cleanup();
        assert_eq!(ctx.control.budget.used.lock().unwrap().disk, 0);
        assert_eq!(ctx.control.budget.used.lock().unwrap().records, 0);
        assert_eq!(fs::read_dir(&ctx.dir).unwrap().count(), 0);
    }
    assert_eq!(ctx.compose_script(&"x".repeat(MAX_SCRIPT_BYTES + 1)).err(), Some(VideoError::Limit));
    ctx.control.budget.used.lock().unwrap().disk = ctx.config.limits.disk;
    assert_eq!(ctx.compose_script(graph).err(), Some(VideoError::Limit));
    ctx.control.budget.used.lock().unwrap().disk = 0;
    assert_eq!(fs::read_dir(&ctx.dir).unwrap().count(), 0);
    fs::remove_dir(&ctx.dir).unwrap();
}

fn sources(ctx: &mut Context) -> (VideoAsset, AudioAsset, StillAsset) {
    let (pin, _) = ctx.record(1).unwrap();
    let video = VideoAsset {
        pin,
        metadata: Arc::new(VideoMetadata {
            width: 4096,
            height: 4096,
            duration_us: timeline::MAX_DURATION_US,
            stream: 0,
            rotation: 0,
            sar: (1, 1),
            frame_rate: Some((120000, 1001)),
            color: VideoColorInfo { original: [None, None, None, None], profile: ColorProfile::Bt709, full_range: false, assumed_fields: 15 },
            // Even the longest formatted seek offset does not change the argument bound.
            format_start: i128::MIN,
            stream_start: i128::MAX,
            timebase: (1, 1000),
            frames: None,
            audio: Some((1, "aac")),
        }),
        hash: ContentHash([0; 32]),
        mov: true,
    };
    let (pin, _) = ctx.record(1).unwrap();
    let audio = AudioAsset {
        pin,
        metadata: Arc::new(AudioMetadata {
            duration_us: timeline::MAX_DURATION_US,
            stream: 0,
            codec: "aac",
            format_start: i128::MIN,
            stream_start: i128::MAX,
            sample_rate: 384000,
            channels: 8,
        }),
        hash: ContentHash([0; 32]),
        demuxer: "mov",
    };
    let (pin, _) = ctx.record(1).unwrap();
    let still = StillAsset { pin, width: 4096, height: 4096, hash: ContentHash([0; 32]) };
    (video, audio, still)
}

#[test]
fn maximum_timeline_command_stays_below_windows_limit_including_executable_and_quoting() {
    let mut ctx = isolated_context();
    // Longest generated basename: even after a service has staged billions of files.
    ctx.registry.lock().unwrap().next = u64::MAX - 10;
    let (video, audio, still) = sources(&mut ctx);
    let video = ComposeClip::new(video, 0, 100_000_000).unwrap();
    let audio = ComposeClip::audio(audio, 0, 100_000_000).unwrap();
    let picture = ComposeClip::still(still.clone(), 100_000_000).unwrap();
    let (script, _) = ctx.compose_script("generated graph stays in this file").unwrap();
    let (output, _) = ctx.record(1).unwrap();
    let mut largest = 0;
    for main in [&video, &picture] {
        for layer in [&video, &audio, &picture] {
            for layer_count in [0, 8, timeline::MAX_CLIPS - 1] {
                let spec = ComposeSpec::new(4096, 4096, (120000, 1001), [0; 3], vec![main.clone(); timeline::MAX_CLIPS - layer_count])
                    .unwrap()
                    .with_layers(vec![(layer.clone(), 0); layer_count])
                    .unwrap()
                    .with_captions(vec![ComposeCaption::new(still.clone(), 0, 0, 0, 100_000_000).unwrap(); timeline::MAX_CAPTIONS])
                    .unwrap();
                for option in SCRIPT_OPTIONS {
                    let args = ctx.compose_args(&spec, option, &script, &output).unwrap();
                    assert_eq!(args.iter().filter(|arg| *arg == "-i").count(), timeline::MAX_CLIPS + timeline::MAX_CAPTIONS);
                    assert!(!args.iter().any(|arg| arg == "-filter_complex" || arg == "generated graph stays in this file"));
                    // Compose arguments are generated ASCII, without embedded quotes/backslashes.
                    // Count every one as quoted (including separators), plus NUL, and allow twice
                    // the full configured executable limit for Windows escaping and its quotes.
                    let mut units = 2 * 4096 + 3 + 1;
                    for arg in &args {
                        let arg = arg.to_str().unwrap();
                        assert!(arg.is_ascii() && !arg.contains(['"', '\\']));
                        assert!(!arg.contains(ctx.dir.to_str().unwrap()));
                        units += arg.encode_utf16().count() + 3;
                    }
                    largest = largest.max(units);
                    assert!(units < 32767, "{units} UTF-16 units including maximum executable path");
                }
            }
        }
    }
    eprintln!("maximum conservative Windows command: {largest} UTF-16 units");
    drop((video, audio, picture, still, script, output));
    ctx.cleanup();
    fs::remove_dir(&ctx.dir).unwrap();
}

#[test]
fn repeated_layer_additions_cannot_bypass_the_command_input_limit() {
    let mut ctx = isolated_context();
    let (video, audio, still) = sources(&mut ctx);
    let clip = ComposeClip::still(still.clone(), 1_000_000).unwrap();
    let spec =
        ComposeSpec::new(4096, 4096, (30, 1), [0; 3], vec![clip.clone()]).unwrap().with_layers(vec![(clip.clone(), 0); timeline::MAX_CLIPS - 1]).unwrap();
    assert_eq!(spec.with_layers(vec![(clip, 0)]).err(), Some(VideoError::Invalid));
    drop((video, audio, still));
    ctx.cleanup();
    fs::remove_dir(&ctx.dir).unwrap();
}

#[test]
#[ignore = "explicit installed ffmpeg/ffprobe only; runnable on Windows as well as macOS/Linux"]
fn actual_maximum_clips_layers_and_captions_render_from_a_script() {
    let service = service();
    let mut job = service.stage_still(Arc::from(vec![255; 16 * 16 * 4]), 16, 16).ok().unwrap();
    let still = wait(&mut job).unwrap();
    drop(job);
    let clip = ComposeClip::still(still.clone(), 100_000).unwrap();
    let captions = vec![ComposeCaption::new(still.clone(), 0, 0, 0, 100_000).unwrap(); timeline::MAX_CAPTIONS];
    let spec = ComposeSpec::new(16, 16, (10, 1), [0; 3], vec![clip.clone(); timeline::MAX_CLIPS - 8])
        .unwrap()
        .with_layers(vec![(clip, 0); 8])
        .unwrap()
        .with_captions(captions)
        .unwrap();
    let mut job = service.compose(spec).unwrap();
    let rendered = wait(&mut job).unwrap();
    assert_eq!(rendered.frames(), 24);
    assert!(rendered.asset().metadata().has_audio());
    drop((job, rendered, still));
    assert!(service.begin_shutdown().wait_blocking(Duration::from_secs(10)).is_ok());
}
