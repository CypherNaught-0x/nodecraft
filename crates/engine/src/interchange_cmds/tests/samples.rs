//! Original representative media, independent of cloud calls and future template packaging.
use super::*;
use nodecraft_nodes::timeline::{Clip, Input, Timeline, Track, TrackKind};
const VIDEO: &[u8] = include_bytes!("fixtures/moving-marker.mp4");
const STILL: NodeId = NodeId::from_bytes([7; 16]);

fn wav() -> Vec<u8> {
    let frames = 8_000u32;
    let data_bytes = frames * 4;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&8_000u32.to_le_bytes());
    out.extend_from_slice(&32_000u32.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for frame in 0..frames {
        let sample = ((f64::from(frame) * 440.0 * std::f64::consts::TAU / 8_000.0).sin() * 6_000.0) as i16;
        out.extend_from_slice(&sample.to_le_bytes());
        out.extend_from_slice(&sample.saturating_neg().to_le_bytes());
    }
    out
}
fn png() -> Vec<u8> {
    let mut pixels = Vec::new();
    for y in 0..8 {
        for x in 0..8 {
            let pixel = if x < 2 || y > 5 { [240, 150, 40, 255] } else { [30, 160, 70, 255] };
            pixels.extend_from_slice(&pixel);
        }
    }
    nodecraft_media::encode_png(8, 8, &pixels, &Default::default()).unwrap()
}
fn put(s: &mut Session, id: NodeId, kind: &str, title: &str, bytes: &[u8], mime: &str, meta: MediaMeta) {
    let hash = s.import_asset(bytes).unwrap();
    s.dispatch("node.create", &json!({"canvas":CANVAS,"id":id,"kind":kind,"pos":{"x":0,"y":0}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas":CANVAS,"id":id,"name":"asset","value":Value::Media(AssetRef { hash, mime:mime.parse().unwrap(), meta })}))
        .unwrap();
    s.dispatch("node.title", &json!({"canvas":CANVAS,"id":id,"title":title})).unwrap();
}
fn representative() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([42; 16]), "Original interchange sample").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "Practice") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    put(&mut s, FOX, "source.video", "Moving marker", VIDEO, "video/mp4", MediaMeta { duration_ms: Some(1000), channels: Some(1), ..Default::default() });
    put(
        &mut s,
        SONG,
        "source.audio",
        "Stereo tone",
        &wav(),
        "audio/wav",
        MediaMeta { duration_ms: Some(1000), channels: Some(2), sample_rate: Some(8000), ..Default::default() },
    );
    put(&mut s, STILL, "source.image", "Colored marker", &png(), "image/png", MediaMeta { width: Some(8), height: Some(8), ..Default::default() });
    s.dispatch("node.create", &json!({"canvas":CANVAS,"id":EDITOR,"kind":ID,"pos":{"x":400,"y":0}})).unwrap();
    s.dispatch("node.title", &json!({"canvas":CANVAS,"id":EDITOR,"title":"Practice cut"})).unwrap();
    wire(&mut s, 1, FOX, nodecraft_nodes::timeline::INPUT);
    wire(&mut s, 2, SONG, nodecraft_nodes::timeline::AUDIO_INPUT);
    wire(&mut s, 3, STILL, nodecraft_nodes::timeline::STILLS_INPUT);
    let mut first = Clip::wired("first".into(), Input::Clips, FOX.to_string(), 0, 0);
    first.out_us = Some(1_000_000);
    first.fade_in_us = 200_000;
    first.fade_out_us = 200_000;
    first.transition_us = 200_000;
    let mut second = first.clone();
    second.id = "trimmed".into();
    second.in_us = 200_000;
    second.transition_us = 0;
    second.opacity = 700;
    let mut sound = Clip::wired("sound".into(), Input::Audio, SONG.to_string(), 0, 1);
    sound.at_us = 100_000;
    sound.out_us = Some(900_000);
    sound.fade_in_us = 300_000;
    sound.fade_out_us = 300_000;
    sound.volume = 700;
    let mut still = Clip::wired("marker".into(), Input::Stills, STILL.to_string(), 0, 2);
    still.at_us = 300_000;
    still.out_us = Some(700_000);
    still.fade_in_us = 200_000;
    still.fade_out_us = 200_000;
    still.opacity = 600;
    let timeline = Timeline {
        clips: vec![first, second, sound, still],
        tracks: vec![Track { kind: TrackKind::Audio }, Track { kind: TrackKind::Video }],
        captions: vec![],
    };
    s.dispatch("node.timeline_set", &json!({"canvas":CANVAS,"id":EDITOR,"clips":timeline.clips,"tracks":timeline.tracks})).unwrap();
    s.dispatch(
        "timeline.caption_add",
        &json!({"canvas":CANVAS,"id":EDITOR,"caption":"intro","text":"Original practice cut","start_us":300_000,"end_us":900_000}),
    )
    .unwrap();
    s
}
fn verify_archive(path: &std::path::Path) {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    assert_eq!(zip.len(), 6, "XML, report, SRT and three media files");
    for (name, expected) in [("media/01-Moving-marker.mp4", VIDEO.to_vec()), ("media/02-Stereo-tone.wav", wav()), ("media/03-Colored-marker.png", png())] {
        let mut bytes = Vec::new();
        zip.by_name(name).unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }
    let mut xml = String::new();
    zip.by_name("Practice-cut.fcpxml").unwrap().read_to_string(&mut xml).unwrap();
    assert!(xml.contains("duration=\"48/30s\""), "{xml}");
    assert!(xml.contains("lane=\"-1\"") && xml.contains("lane=\"1\""));
    assert!(xml.contains("Cross Dissolve") && xml.contains("<title "));
    assert!(xml.contains("<adjust-volume") && xml.contains("<adjust-blend"));
    assert!(xml.contains("time=\"6/30s\""), "trimmed/source clock");
    let mut report = String::new();
    zip.by_name("report.txt").unwrap().read_to_string(&mut report).unwrap();
    assert!(!report.lines().any(|line| line.contains(": fade:") || line.contains(": opacity:")), "{report}");
    assert!(report.contains("hasn't been verified"));
}

#[test]
fn original_representative_archive_roundtrips_every_media_type_and_fade() {
    let mut s = representative();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("practice.zip");
    s.dispatch("timeline.export_interchange", &json!({"canvas":CANVAS,"id":EDITOR,"path":path})).unwrap();
    verify_archive(&path);
}

#[test]
#[ignore = "manual original sample author; requires NODECRAFT_INTERCHANGE643_SAMPLES (no editor import is performed)"]
fn write_representative_interchange643_samples() {
    let root = PathBuf::from(std::env::var_os("NODECRAFT_INTERCHANGE643_SAMPLES").unwrap());
    std::fs::create_dir_all(&root).unwrap();
    let mut s = representative();
    let path = root.join("practice-cut.zip");
    s.dispatch("timeline.export_interchange", &json!({"canvas":CANVAS,"id":EDITOR,"path":path,"overwrite":true})).unwrap();
    verify_archive(&path);
    let first = s.document().clone();
    let snapshot = nodecraft_format::Snapshot { project: first.clone(), assets: s.assets().clone(), epoch: 0 };
    let reopened = nodecraft_format::decode(&nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    assert_eq!(reopened.project, first);
    std::fs::write(root.join("practice-source.ncraft"), nodecraft_format::encode(&snapshot).unwrap()).unwrap();
    std::fs::write(root.join("IMPORT-CHECKLIST.txt"),"Original NodeCraft sample. Not imported in an editor by this test.\nRecord exact Final Cut Pro and Resolve versions and verify relative media, trims/dissolve handles, video/still/audio lanes, opacity/audio fades, captions and durations.\nBoth real imports remain required before #643 can merge.\n").unwrap();
}
