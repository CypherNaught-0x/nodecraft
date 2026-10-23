use super::*;
use crate::Session;
use nodecraft_types::{EdgeId, MediaMeta, ProjectId, Value};
use std::io::Read;

const CANVAS: CanvasId = CanvasId::from_bytes([1; 16]);
const FOX: NodeId = NodeId::from_bytes([3; 16]);
const AGAIN: NodeId = NodeId::from_bytes([4; 16]);
const SONG: NodeId = NodeId::from_bytes([5; 16]);
const EDITOR: NodeId = NodeId::from_bytes([6; 16]);

const MP4: &[u8] = b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41 fox frames";
const WAV: &[u8] = b"RIFF\x24\0\0\0WAVEfmt  owl song";

/// A file: its bytes, MIME type and length in seconds.
type File<'a> = (&'a [u8], &'a str, u64);

fn source(s: &mut Session, id: NodeId, kind: &str, title: &str, (bytes, mime, seconds): File<'_>) {
    let hash = s.import_asset(bytes).unwrap();
    let meta = MediaMeta { duration_ms: Some(seconds * 1000), channels: Some(2), ..MediaMeta::default() };
    let asset = AssetRef { hash, mime: mime.parse().unwrap(), meta };
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": id, "kind": kind, "pos": {"x": 0, "y": 0}})).unwrap();
    s.dispatch("node.param_set", &json!({"canvas": CANVAS, "id": id, "name": "asset", "value": Value::Media(asset)})).unwrap();
    s.dispatch("node.title", &json!({"canvas": CANVAS, "id": id, "title": title})).unwrap();
}

fn wire(s: &mut Session, n: u8, from: NodeId, port: &str) {
    let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": EDITOR, "port": port}, "order": n});
    s.dispatch("edge.connect", &json!({"canvas": CANVAS, "edge": edge})).unwrap();
}

/// A fox video played twice (wired from two nodes holding the same file) over a song, and an
/// unrelated asset the project also holds.
fn session() -> Session {
    let mut project = nodecraft_graph::Project::new(ProjectId::from_bytes([2; 16]), "P").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(CANVAS, "C") }.apply(&mut project).unwrap();
    let mut s = Session::new(project).unwrap();
    s.import_asset(b"an unrelated private asset").unwrap();
    source(&mut s, FOX, "source.video", "Füchse / Eulen", (MP4, "video/mp4", 4));
    source(&mut s, AGAIN, "source.video", "Fox again", (MP4, "video/mp4", 4));
    source(&mut s, SONG, "source.audio", "", (WAV, "audio/wav", 6));
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": EDITOR, "kind": ID, "pos": {"x": 400, "y": 0}})).unwrap();
    s.dispatch("node.title", &json!({"canvas": CANVAS, "id": EDITOR, "title": "Rough cut"})).unwrap();
    wire(&mut s, 1, FOX, nodecraft_nodes::timeline::INPUT);
    wire(&mut s, 2, AGAIN, nodecraft_nodes::timeline::INPUT);
    wire(&mut s, 3, SONG, nodecraft_nodes::timeline::AUDIO_INPUT);
    s
}

fn on() -> Json {
    json!({"canvas": CANVAS, "id": EDITOR})
}

#[test]
fn the_preview_names_each_file_once_and_writes_nothing() {
    let mut s = session();
    let before = s.document().clone();
    let preview = s.dispatch("timeline.interchange", &on()).unwrap();
    assert_eq!(s.document(), &before);
    assert_eq!((&preview["format"], &preview["version"], &preview["document"]), (&json!("fcpxml"), &json!("1.10"), &json!("Rough-cut.fcpxml")));
    let files: Vec<&str> = preview["media"].as_array().unwrap().iter().map(|m| m["file"].as_str().unwrap()).collect();
    assert_eq!(files, ["media/01-Füchse-Eulen.mp4", "media/02-Source-2.wav"], "the same video wired twice is one file");
    assert_eq!(preview["media"][1]["kind"], json!("audio"));
    // Two four-second clips one after the other, at the default 30 fps.
    assert_eq!(preview["frames"], json!(240));
    let xml = preview["xml"].as_str().unwrap();
    assert_eq!(xml.matches("<asset-clip").count(), 3, "{xml}");
    assert!(xml.contains("src=\"media/01-F%C3%BCchse-Eulen.mp4\""), "{xml}");
    assert!(preview["report"].as_str().unwrap().contains("No losses detected"));
}

#[test]
fn a_wired_node_without_a_result_is_named_rather_than_left_out() {
    let mut s = session();
    let painter = NodeId::from_bytes([7; 16]);
    s.dispatch("node.create", &json!({"canvas": CANVAS, "id": painter, "kind": "source.video", "pos": {"x": 0, "y": 200}})).unwrap();
    wire(&mut s, 4, painter, nodecraft_nodes::timeline::INPUT);
    let error = s.dispatch("timeline.interchange", &on()).unwrap_err();
    assert!(error.message.contains("clip 3 has no media yet"), "{}", error.message);
    let path = std::env::temp_dir().join(format!("nodecraft-interchange-{}-unrendered.zip", std::process::id()));
    assert!(s.dispatch("timeline.export_interchange", &json!({"canvas": CANVAS, "id": EDITOR, "path": path})).is_err());
    assert!(!path.exists());
}

#[test]
fn only_a_timeline_and_an_absolute_path_are_taken() {
    let mut s = session();
    assert!(s.dispatch("timeline.interchange", &json!({"canvas": CANVAS, "id": FOX})).is_err());
    for bad in ["", "cut.zip", "../cut.zip"] {
        let error = s.dispatch("timeline.export_interchange", &json!({"canvas": CANVAS, "id": EDITOR, "path": bad})).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{bad:?}");
    }
    assert!(s.dispatch("timeline.export_interchange", &json!({"canvas": CANVAS, "id": EDITOR, "path": "/x.zip", "format": "edl"})).is_err());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_archive_holds_the_document_the_report_and_only_the_media_it_names() {
    let mut s = session();
    let dir = std::env::temp_dir().join(format!("nodecraft-interchange-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("cut.zip");
    let _ = std::fs::remove_file(&path);
    let params = json!({"canvas": CANVAS, "id": EDITOR, "path": path});
    let written = s.dispatch("timeline.export_interchange", &params).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(written["bytes"], json!(bytes.len()));
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut names: Vec<String> = zip.file_names().map(|name| name.unwrap().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["Rough-cut.fcpxml", "media/01-Füchse-Eulen.mp4", "media/02-Source-2.wav", "report.txt"]);
    let mut video = Vec::new();
    zip.by_name("media/01-Füchse-Eulen.mp4").unwrap().read_to_end(&mut video).unwrap();
    assert_eq!(video, MP4);
    let mut document = String::new();
    zip.by_name("Rough-cut.fcpxml").unwrap().read_to_string(&mut document).unwrap();
    assert!(!document.contains(dir.to_str().unwrap()), "no absolute paths");

    // Never over a file unless asked; asked, the file is replaced whole.
    let again = s.dispatch("timeline.export_interchange", &params).unwrap_err();
    assert_eq!(again.kind, ErrorKind::Conflict);
    assert!(!again.message.contains("cut.zip"), "{}", again.message);
    let mut over = params.clone();
    over["overwrite"] = json!(true);
    s.dispatch("timeline.export_interchange", &over).unwrap();
    assert!(zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).is_ok());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn archived_reports_name_each_untranslated_clip_field_without_default_noise() {
    let clip = nodecraft_nodes::timeline::auto_id(nodecraft_nodes::timeline::Input::Clips, &FOX.to_string(), 0);
    // Each case is a separate saved timeline and archive. A combined case also ensures that
    // related axes/edges group together without hiding unrelated features.
    let cases = [
        ("default", json!({}), Vec::<String>::new()),
        (
            "explicit_defaults",
            json!({"speed": 1000, "pos_x": 500, "pos_y": 500, "scale": 1000, "crop_left": 0, "crop_top": 0, "crop_right": 0, "crop_bottom": 0, "rotation": 0, "opacity": 1000}),
            Vec::new(),
        ),
        (
            "speed",
            json!({"speed": 2000}),
            vec!["speed: not written: the clip exports at its unretimed media-trim length; retime it in the other editor".into()],
        ),
        ("pos_x", json!({"pos_x": 250}), vec!["position: pos_x not written: the clip exports centred in the frame; reposition it in the other editor".into()]),
        ("pos_y", json!({"pos_y": 750}), vec!["position: pos_y not written: the clip exports centred in the frame; reposition it in the other editor".into()]),
        ("scale", json!({"scale": 1500}), vec!["scale: not written: the clip exports at the editor's default fitted size; resize it there".into()]),
        ("crop_left", json!({"crop_left": 100}), vec!["crop: crop_left not written: the clip exports uncropped; crop it in the other editor".into()]),
        ("crop_top", json!({"crop_top": 100}), vec!["crop: crop_top not written: the clip exports uncropped; crop it in the other editor".into()]),
        ("crop_right", json!({"crop_right": 100}), vec!["crop: crop_right not written: the clip exports uncropped; crop it in the other editor".into()]),
        ("crop_bottom", json!({"crop_bottom": 100}), vec!["crop: crop_bottom not written: the clip exports uncropped; crop it in the other editor".into()]),
        ("rotation", json!({"rotation": 45}), vec!["rotation: not written: the clip exports upright; rotate it in the other editor".into()]),
        ("opacity", json!({"opacity": 700}), vec!["opacity: not written: the clip exports fully opaque; set its opacity in the other editor".into()]),
        (
            "all_features",
            json!({"speed": 2000, "pos_x": 250, "pos_y": 750, "scale": 1500, "crop_left": 100, "crop_top": 100, "crop_right": 100, "crop_bottom": 100, "rotation": 45, "opacity": 700}),
            vec![
                "speed: not written: the clip exports at its unretimed media-trim length; retime it in the other editor".into(),
                "position: pos_x and pos_y not written: the clip exports centred in the frame; reposition it in the other editor".into(),
                "scale: not written: the clip exports at the editor's default fitted size; resize it there".into(),
                "crop: crop_left, crop_top, crop_right, crop_bottom not written: the clip exports uncropped; crop it in the other editor".into(),
                "rotation: not written: the clip exports upright; rotate it in the other editor".into(),
                "opacity: not written: the clip exports fully opaque; set its opacity in the other editor".into(),
            ],
        ),
    ];
    let root = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let dir = tempfile::tempdir_in(root).unwrap();
    for (name, settings, expected) in cases {
        let mut s = session();
        if settings != json!({}) {
            s.dispatch("timeline.update", &json!({"canvas": CANVAS, "id": EDITOR, "clip": clip, "set": settings})).unwrap();
        }
        let path = dir.path().join(format!("{name}.zip"));
        s.dispatch("timeline.export_interchange", &json!({"canvas": CANVAS, "id": EDITOR, "path": path})).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
        let mut report = String::new();
        zip.by_name("report.txt").unwrap().read_to_string(&mut report).unwrap();
        let actual: Vec<String> = report.lines().filter(|line| line.starts_with("- ")).map(str::to_owned).collect();
        let expected: Vec<String> = expected.iter().map(|line| format!("- clip {clip}: {line}")).collect();
        assert_eq!(actual, expected, "{name}: {report}");
        assert_eq!(report.contains("No losses detected."), expected.is_empty(), "{name}: {report}");
    }
}

#[test]
fn file_names_keep_letters_and_lose_the_rest() {
    assert_eq!(stem("Füchse / Eulen", "x"), "Füchse-Eulen");
    assert_eq!(stem("../../etc/passwd", "x"), "etc-passwd");
    assert_eq!(stem("C:\\Users\\me", "x"), "C-Users-me");
    assert_eq!(stem("  ", "fallback"), "fallback");
    assert_eq!(stem(&"a".repeat(200), "x").chars().count(), MAX_STEM_CHARS);
    assert_eq!(extension("video/mp4"), "mp4");
    assert_eq!(extension("video/quicktime"), "mov");
    assert_eq!(extension("audio/x-flac"), "flac");
    assert_eq!(extension("image/svg+xml"), "svg");
    assert_eq!(extension("video/x-very-long-name"), "bin");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn captioned_archive_contains_titles_srt_and_every_caption_loss_in_the_report() {
    let mut s = session();
    s.dispatch(
        "timeline.caption_add",
        &json!({
            "canvas": CANVAS, "id": EDITOR, "caption": "intro", "text": "Füchse\nSecond line",
            "start_us": 1_000_000, "end_us": 2_500_000, "font": "Requested font", "color": "#aabbcc", "stroke_width": 2,
            "align": "right", "position": "top",
        }),
    )
    .unwrap();
    let before = s.document().clone();
    let preview = s.dispatch("timeline.interchange", &on()).unwrap();
    assert_eq!(s.document(), &before, "preview writes no graph changes");
    assert_eq!(preview["subtitles"]["file"], json!("Rough-cut.srt"));
    assert_eq!(preview["subtitles"]["format"], json!("srt"));
    let text = preview["subtitles"]["text"].as_str().unwrap();
    assert_eq!(text, "1\n00:00:01,000 --> 00:00:02,500\nFüchse\nSecond line\n\n");
    assert!(preview["xml"].as_str().unwrap().contains("<title ref=\"rCaptionTitle\""));
    let report = preview["report"].as_str().unwrap();
    assert!(report.contains("Subtitles: Rough-cut.srt"));
    assert!(report.contains("caption intro: caption style") && report.contains("Requested font") && report.contains("#aabbcc") && report.contains("2 px"));
    assert!(report.contains("caption intro: caption placement") && report.contains("SRT carries no position"));
    assert!(preview["losses"].as_array().unwrap().iter().all(|loss| loss["caption"] == "intro"));
    let path = std::env::temp_dir().join(format!("nodecraft-caption-interchange-{}.zip", std::process::id()));
    s.dispatch("timeline.export_interchange", &json!({"canvas": CANVAS, "id": EDITOR, "path": path, "overwrite": true})).unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut actual = String::new();
    zip.by_name("Rough-cut.srt").unwrap().read_to_string(&mut actual).unwrap();
    assert_eq!(actual, text);
    actual.clear();
    zip.by_name("report.txt").unwrap().read_to_string(&mut actual).unwrap();
    assert_eq!(actual, report);
    actual.clear();
    zip.by_name("Rough-cut.fcpxml").unwrap().read_to_string(&mut actual).unwrap();
    assert_eq!(actual, preview["xml"].as_str().unwrap());
    drop(zip);
    std::fs::remove_file(path).unwrap();
}
