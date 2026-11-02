//! An Export to folder card (#746) writes only through these commands, only into a folder the host
//! granted, and never changes the document.
use crate::Session;
use crate::local_export::{ExportHost, dispatch_with_host};
use nodecraft_format::export::reasons;
use nodecraft_graph::{Canvas, Change, Edge, Endpoint, Generation, Node, Port, Project};
use nodecraft_nodes::export;
use nodecraft_types::{AssetRef, Blake3, CanvasId, EdgeId, ErrorKind, GenerationId, Modality, NodeId, PortType, ProjectId, Value};
use serde_json::{Value as Json, json};
use std::sync::{Arc, Mutex};

const C: CanvasId = CanvasId::from_bytes([0x46; 16]);
const HERO: &[u8] = b"\x89PNG the hero, not really a PNG";
const LEFT: &[u8] = b"\x89PNG the left take";
const RIGHT: &[u8] = b"\x89PNG the right take";
const SOUND: &[u8] = b"RIFF\0\0\0\0WAVEfmt ";
const CARD: u8 = 9;

fn id(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}

fn media(bytes: &[u8], mime: &str) -> Value {
    Value::Media(AssetRef { hash: Blake3::from_bytes(*blake3::hash(bytes).as_bytes()), mime: mime.parse().unwrap(), meta: Default::default() })
}

fn ran(n: u8, title: &str, modality: Modality, many: bool, value: Value) -> Node {
    let mut node = Node::new(id(n), "test.gen");
    node.title = title.into();
    node.outputs = vec![Port { name: "out".into(), port_type: PortType { modality, many } }];
    let result = Generation::new(GenerationId::from_bytes([n; 16]), vec![value]);
    node.active = Some(result.id);
    node.history.push(result);
    node
}

fn card() -> Node {
    let kinds = nodecraft_nodes::Registry::builtins().unwrap();
    kinds.get(export::ID).unwrap().instantiate_instance(id(CARD), Default::default(), &Default::default()).unwrap()
}

fn wire(e: u8, from: u8, port: &str, order: u32) -> Change {
    Change::Connect {
        canvas: C,
        edge: Edge {
            id: EdgeId::from_bytes([e; 16]),
            from: Endpoint { node: id(from), port: "out".into() },
            to: Endpoint { node: id(CARD), port: port.into() },
            order,
        },
    }
}

/// hero (image) and a two-item list (image) into the image port, a sound into the audio port.
fn session() -> Session {
    let mut project = Project::new(ProjectId::from_bytes([0x46; 16]), "Export").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    for node in [
        ran(1, "hero", Modality::Image, false, media(HERO, "image/png")),
        ran(2, "takes", Modality::Image, true, Value::List(vec![media(LEFT, "image/png"), media(RIGHT, "image/png")])),
        ran(3, "sound", Modality::Audio, false, media(SOUND, "audio/wav")),
        card(),
    ] {
        Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    }
    // The sound is wired first, but the export lists ports in their declared order.
    for change in [wire(0x31, 3, "audio", 0), wire(0x32, 1, "image", 0), wire(0x33, 2, "image", 1)] {
        change.apply(&mut project).unwrap();
    }
    let mut session = Session::new(project).unwrap();
    for bytes in [HERO, LEFT, RIGHT, SOUND] {
        session.import_asset(bytes).unwrap();
    }
    session
}

fn args() -> Json {
    json!({"canvas": C, "node": id(CARD)})
}

fn set(session: &mut Session, name: &str, value: &str) {
    session.dispatch("node.param_set", &json!({"canvas": C, "id": id(CARD), "name": name, "value": {"type":"text","value":value}})).unwrap();
}

fn files_in(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn without_a_granted_folder_nothing_is_planned_or_written() {
    let mut s = session();
    for command in ["export.node_plan", "export.node_run"] {
        let error = s.dispatch(command, &args()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Unsupported, "{command}");
        assert_eq!(error.message, reasons::NO_HOST);
    }
    // A caller cannot name a folder: the card's commands take no destination at all.
    let dir = tempfile::tempdir().unwrap();
    let error = s.dispatch("export.node_run", &json!({"canvas": C, "node": id(CARD), "destination": dir.path()})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(files_in(dir.path()).is_empty());
}

#[test]
fn a_preview_lists_wired_results_in_port_order_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let revision = s.revision();
    let preview = dispatch_with_host(&mut s, &host, "export.node_plan", &args()).unwrap();
    let files: Vec<_> = preview["entries"].as_array().unwrap().iter().map(|e| e["file"].as_str().unwrap().to_owned()).collect();
    assert_eq!(files, ["hero-out-1.png", "takes-out-2.png", "takes-out-3.png", "sound-out-4.wav"]);
    assert_eq!(preview["entries"][1]["item"], 0);
    assert_eq!(preview["entries"][2]["item"], 1);
    assert_eq!(preview["on_collision"], "skip");
    assert!(!preview.to_string().contains(&dir.path().display().to_string()), "a result never repeats the folder");
    assert!(files_in(dir.path()).is_empty());
    assert_eq!(s.revision(), revision);
    // The grant was scoped to that one command.
    assert_eq!(s.dispatch("export.node_plan", &args()).unwrap_err().kind, ErrorKind::Unsupported);
}

#[test]
fn only_an_export_card_with_wired_results_exports() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session();
    let host = ExportHost::authorize(dir.path()).unwrap();
    let error = dispatch_with_host(&mut s, &host, "export.node_plan", &json!({"canvas": C, "node": id(1)})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    let error = dispatch_with_host(&mut s, &host, "export.node_plan", &json!({"canvas": C, "node": id(77)})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
    for edge in [0x31, 0x32, 0x33] {
        s.dispatch("edge.disconnect", &json!({"canvas": C, "id": EdgeId::from_bytes([edge; 16])})).unwrap();
    }
    let error = dispatch_with_host(&mut s, &host, "export.node_plan", &args()).unwrap_err();
    assert!(error.message.contains("wire image"), "{}", error.message);
}

#[test]
fn a_subfolder_must_stay_inside_the_grant_and_exist() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session();
    let host = ExportHost::authorize(dir.path()).unwrap();
    set(&mut s, export::SUBFOLDER, "renders");
    let error = dispatch_with_host(&mut s, &host, "export.node_plan", &args()).unwrap_err();
    assert_eq!(error.message, reasons::DESTINATION_MISSING, "export never creates a folder");
    std::fs::create_dir(dir.path().join("renders")).unwrap();
    dispatch_with_host(&mut s, &host, "export.node_plan", &args()).unwrap();
    // A hostile stored value (hand-edited project) is refused, never resolved outside the grant.
    let mut project = s.document().clone();
    let mut node = project.canvases().get(&C).unwrap().nodes().get(&id(CARD)).unwrap().clone();
    node.params.insert(export::SUBFOLDER.into(), Value::Text("../outside".into()));
    Change::SetNode { canvas: C, node }.apply(&mut project).unwrap();
    let mut hostile = Session::new(project).unwrap();
    let error = dispatch_with_host(&mut hostile, &host, "export.node_run", &args()).unwrap_err();
    assert!(error.message.contains(".."), "{}", error.message);
}

#[cfg(unix)]
#[test]
fn running_writes_into_the_granted_folder_and_changes_no_document() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("final")).unwrap();
    let mut s = session();
    set(&mut s, export::SUBFOLDER, "final");
    let host = ExportHost::authorize(dir.path()).unwrap();
    let (revision, undo) = (s.revision(), s.can_undo());
    let document = serde_json::to_value(s.document()).unwrap();
    let outcome = dispatch_with_host(&mut s, &host, "export.node_run", &args()).unwrap();
    assert_eq!(outcome["counts"]["written"], 4, "{outcome}");
    assert_eq!(files_in(&dir.path().join("final")), ["hero-out-1.png", "sound-out-4.wav", "takes-out-2.png", "takes-out-3.png"]);
    assert_eq!(std::fs::read(dir.path().join("final/hero-out-1.png")).unwrap(), HERO);
    assert_eq!(files_in(dir.path()), ["final"], "nothing lands beside the subfolder");
    assert_eq!((s.revision(), s.can_undo()), (revision, undo), "an export is no edit and no undo step");
    assert_eq!(serde_json::to_value(s.document()).unwrap(), document);

    // Running again keeps what is there by default, and says so.
    let again = dispatch_with_host(&mut s, &host, "export.node_run", &args()).unwrap();
    assert_eq!(again["counts"]["skipped"], 4);
    set(&mut s, export::ON_COLLISION, "numbered");
    let numbered = dispatch_with_host(&mut s, &host, "export.node_run", &args()).unwrap();
    assert_eq!(numbered["counts"]["written"], 4);
    assert!(files_in(&dir.path().join("final")).contains(&"hero-out-1 (2).png".to_owned()));
    set(&mut s, export::ON_COLLISION, "overwrite");
    std::fs::write(dir.path().join("final/hero-out-1.png"), b"stale").unwrap();
    let replaced = dispatch_with_host(&mut s, &host, "export.node_run", &args()).unwrap();
    assert_eq!(replaced["counts"]["written"], 4);
    assert_eq!(std::fs::read(dir.path().join("final/hero-out-1.png")).unwrap(), HERO);
}

#[cfg(unix)]
#[test]
fn a_permission_failure_is_reported_per_file_and_never_panics() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    // Root ignores directory permissions; there is nothing to observe then.
    if std::fs::write(locked.join("probe"), b"x").is_ok() {
        return;
    }
    let mut s = session();
    set(&mut s, export::SUBFOLDER, "locked");
    let host = ExportHost::authorize(dir.path()).unwrap();
    let outcome = dispatch_with_host(&mut s, &host, "export.node_run", &args()).unwrap();
    assert_eq!(outcome["counts"]["failed"], 4);
    assert!(outcome["entries"][0]["reason"].as_str().unwrap().contains("choose another folder"), "{outcome}");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[derive(Default)]
struct Held(Mutex<Option<crate::save_runner::SaveJob>>);
impl crate::save_runner::SaveRunner for Held {
    fn spawn(&self, job: crate::save_runner::SaveJob) -> Result<(), crate::save_runner::SaveJob> {
        *self.0.lock().unwrap() = Some(job);
        Ok(())
    }
}
impl Held {
    fn run(&self) {
        let job = self.0.lock().unwrap().take();
        job.unwrap().run();
    }
}

#[cfg(unix)]
#[test]
fn a_background_export_reports_progress_and_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = session();
    let runner = Arc::new(Held::default());
    s.set_export_runner(runner.clone());
    let host = ExportHost::authorize(dir.path()).unwrap();
    let background = json!({"canvas": C, "node": id(CARD), "background": true});
    let pending = dispatch_with_host(&mut s, &host, "export.node_run", &background).unwrap();
    let job = pending["pending"].as_u64().unwrap();
    assert!(files_in(dir.path()).is_empty(), "nothing is written before the worker runs");
    runner.run();
    let status = s.export_status().unwrap();
    let Some(crate::export_job::ExportResult::Batch { outcome }) = status.result else { panic!("batch receipt: {status:?}") };
    assert_eq!(outcome["counts"]["written"], 4);
    assert_eq!(status.progress.map(|p| (p.completed, p.total, p.written)), Some((4, 4, 4)));
    assert_eq!(files_in(dir.path()).len(), 4);

    // Cancelled before the worker starts: every entry is reported, none is written.
    let other = tempfile::tempdir().unwrap();
    let host = ExportHost::authorize(other.path()).unwrap();
    let pending = dispatch_with_host(&mut s, &host, "export.node_run", &background).unwrap();
    assert_ne!(pending["pending"].as_u64().unwrap(), job);
    s.dispatch("export.cancel", &json!({"id": pending["pending"]})).unwrap();
    runner.run();
    let status = s.export_status().unwrap();
    assert!(matches!(status.result, Some(crate::export_job::ExportResult::Cancelled | crate::export_job::ExportResult::Batch { .. })), "{status:?}");
    if let Some(crate::export_job::ExportResult::Batch { outcome }) = status.result {
        assert_eq!(outcome["counts"]["not_started"], 4);
    }
    assert!(files_in(other.path()).is_empty());
}
