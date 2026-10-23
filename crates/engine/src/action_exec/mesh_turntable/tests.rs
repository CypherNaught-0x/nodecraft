use super::*;
use nodecraft_exec::run::ActionExecutor;
use nodecraft_types::{NodeId, Number, Unsigned};
use std::cell::Cell;

#[allow(clippy::unwrap_used)] // Test fixture; no product path calls it.
fn params() -> ParamValues {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    let kind = kinds.get(nodecraft_nodes::mesh::TURNTABLE).unwrap();
    let node = kind.instantiate_instance(NodeId::from_bytes([15; 16]), Default::default(), &Default::default()).unwrap();
    kind.validate_instance(&node, true).unwrap().into_visible_values()
}
#[test]
fn turntable_cancellation_between_frames_does_not_render_or_queue_the_next_one() {
    let sequence = Sequence::new([16, 16], 4, 1.0, 0.0, 90.0, false, false).unwrap();
    let cancelled = Cell::new(false);
    let mut seen = Vec::new();
    let result = frames(sequence, &|| cancelled.get(), |index, yaw| {
        seen.push((index, yaw));
        cancelled.set(true);
        Ok(())
    });
    assert_eq!(seen, vec![(0, 0.0)]);
    assert!(result.unwrap_err().contains("cancelled"));
}
#[test]
fn turntable_invalid_work_is_refused_before_reading_a_missing_model_or_opening_tools() {
    let actions = LocalActions::new(nodecraft_format::AssetStore::default());
    let mut p = params();
    p.insert("width".into(), Value::Unsigned(Unsigned(2048)));
    p.insert("height".into(), Value::Unsigned(Unsigned(2048)));
    p.insert("duration".into(), Value::Number(Number::new(60.0).unwrap()));
    assert!(actions.execute(nodecraft_nodes::mesh::TURNTABLE, &p, &[]).unwrap_err().contains("256 million"));
    let p = params();
    assert!(actions.execute(nodecraft_nodes::mesh::TURNTABLE, &p, &[]).unwrap_err().contains("ffmpeg"));
    assert!(actions.long_running(nodecraft_nodes::mesh::TURNTABLE));
    assert!(actions.execute_cancellable(nodecraft_nodes::mesh::TURNTABLE, &p, &[], &|| true).unwrap_err().contains("cancelled"));
}
#[test]
fn turntable_direction_and_mode_are_strict_at_the_executor_boundary() {
    for (name, value) in [("direction", "sideways"), ("orbit", "spiral")] {
        let mut p = params();
        p.insert(name.into(), Value::Text(value.into()));
        assert!(sequence(&p).is_err());
    }
}

#[test]
fn turntable_default_full_orbit_does_not_require_its_hidden_second_angle() {
    let p = params();
    assert!(!p.contains_key("end"));
    assert_eq!(sequence(&p).unwrap().frames(), 180);
}

#[test]
#[ignore = "actual GPU and ffmpeg; explicit NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE"]
fn turntable_actual_frames_repeat_and_form_an_ordinary_video() {
    use crate::action_exec::timeline::native::{detach_test_service, service, wait};
    use nodecraft_media::video::FrameRequest;
    use nodecraft_types::{AssetRef, MediaMeta};
    use std::{path::PathBuf, sync::Arc, time::Duration};
    let tools =
        (PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap()), PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap()));
    let open = service(&tools, &|| false).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let obj = b"v 0 0 0\nv 2 0 0\nv 2 1 0\nv 0 1 0\nv 0 0 0.5\nv 2 0 0.5\nv 2 1 0.5\nv 0 1 0.5\nf 1 2 3 4\nf 5 8 7 6\nf 1 5 6 2\nf 2 6 7 3\nf 3 7 8 4\nf 5 1 4 8\n";
        let mut assets = nodecraft_format::AssetStore::default();
        let model = Value::Media(AssetRef { hash: assets.insert(obj).unwrap(), mime: "model/obj".parse().unwrap(), meta: MediaMeta::default() });
        let actions = LocalActions::new(assets).with_video_tools(Some(tools.clone()));
        let mut p = params();
        for (key, value) in [("width", 64), ("height", 48), ("fps", 12)] {
            p.insert(key.into(), Value::Unsigned(Unsigned(value)));
        }
        p.insert("duration".into(), Value::Number(Number::new(1.0).unwrap()));
        let mut sequences = Vec::new();
        for _ in 0..2 {
            let Value::Media(output) = actions.execute(nodecraft_nodes::mesh::TURNTABLE, &p, std::slice::from_ref(&model)).unwrap() else { panic!("a video") };
            assert_eq!(output.mime.essence(), "video/webm");
            assert_eq!((output.meta.width, output.meta.height, output.meta.duration_ms), (Some(64), Some(48), Some(1000)));
            let video = actions.import_video(&open, Some(&Value::Media(output)), "a video", &|| false).unwrap();
            let metadata = video.metadata();
            assert_eq!((metadata.width, metadata.height, metadata.frame_rate), (64, 48, Some((12, 1))));
            assert!(!metadata.has_audio());
            assert!((990_000..=1_100_000).contains(&metadata.duration_us));
            let mut hashes = Vec::new();
            for index in 0..12u64 {
                let frame = wait(open.frame(&video, FrameRequest::new(index * 1_000_000 / 12, 64, 48).unwrap()).unwrap(), &|| false).unwrap();
                hashes.push(nodecraft_types::Blake3::from_bytes(*blake3::hash(frame.pixels()).as_bytes()));
            }
            assert_ne!(hashes[0], hashes[3], "the camera actually turns");
            sequences.push(hashes);
        }
        assert_eq!(sequences[0], sequences[1], "the saved frame sequence repeats after decoding");
        eprintln!("NODECRAFT_TURNTABLE_EVIDENCE: two actual GPU/FFmpeg renders, 12 repeatable 64x48 frames at 12fps, ordinary soundless WebM");
    }));
    detach_test_service(&open);
    let cleanup = match Arc::try_unwrap(open) {
        Ok(own) => own.begin_shutdown().wait_blocking(Duration::from_secs(30)).map(|r| !r.cleanup_remaining).unwrap_or(false),
        Err(shared) => {
            drop(shared);
            true
        }
    };
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup, "the owned test video service cleaned up");
}
