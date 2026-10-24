//! Timeline captions (#574) end to end: a black video wired into a Timeline with a white caption
//! for part of it, run through the run host; the caption's pixels show in the frames inside its
//! time and nowhere else. Runs a real ffmpeg: opt in with NODECRAFT_VIDEO_TEST_FFMPEG and
//! NODECRAFT_VIDEO_TEST_FFPROBE.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_catalog::Catalog;
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    import::{Incoming, import_files},
    jobs::JobBudget,
    run_cmds::RunCatalog,
    run_host::{RunHost, RunNotice},
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
    scheduler::RetryPolicy,
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_types::{CanvasId, EdgeId, NodeId, ProjectId, Unsigned, Value};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const ONE: NodeId = NodeId::from_bytes([1; 16]);
const TIMELINE: NodeId = NodeId::from_bytes([3; 16]);

struct Threads(Instant);
impl Runtime for Threads {
    fn spawn(&self, task: TaskFuture<()>) {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
    fn sleep(&self, ms: u64) -> TaskFuture<()> {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

fn tool(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap())
}

/// A black MP4.
fn black(ffmpeg: &Path, seconds: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.mp4");
    let status = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "color=c=black:size=160x96:rate=10"])
        .args(["-t", seconds, "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(path).unwrap()
}

/// The brightest pixel of the frame `at` seconds into `video`, and how many are bright.
fn brightest(ffmpeg: &Path, video: &Path, at: &str) -> (u8, usize) {
    let out = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-ss", at, "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout.len(), 160 * 96);
    (out.stdout.iter().copied().max().unwrap(), out.stdout.iter().filter(|p| **p > 200).count())
}

#[test]
#[ignore = "runs a real ffmpeg"]
fn a_caption_shows_in_the_render_only_inside_its_time() {
    let (ffmpeg, ffprobe) = (tool("NODECRAFT_VIDEO_TEST_FFMPEG"), tool("NODECRAFT_VIDEO_TEST_FFPROBE"));
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::from_toml("").unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([92; 16]), "Captions").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "captions".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    host.set_video_tools(Some((&ffmpeg, &ffprobe)));
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    host.install(&mut session);
    let one = black(&ffmpeg, "2");
    import_files(&mut session, C, Position { x: 0.0, y: 0.0 }, 300.0, &[Incoming { name: "black.mp4", bytes: &one }], &[ONE]).unwrap();
    session.dispatch("node.create", &json!({"canvas": C, "id": TIMELINE, "kind": "video.timeline", "pos": {"x": 400, "y": 0}})).unwrap();
    for (name, value) in [("width", 160), ("height", 96)] {
        session.dispatch("node.param_set", &json!({"canvas": C, "id": TIMELINE, "name": name, "value": Value::Unsigned(Unsigned(value))})).unwrap();
    }
    let edge = json!({"id": EdgeId::from_bytes([1; 16]), "from": {"node": ONE, "port": "out"}, "to": {"node": TIMELINE, "port": "clips"}, "order": 1});
    session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    let caption =
        json!({"canvas": C, "id": TIMELINE, "text": "HI", "start_us": 500_000, "end_us": 1_200_000, "size": 48, "position": "middle", "stroke_width": 2});
    session.dispatch("timeline.caption_add", &caption).unwrap();
    // One that starts after the output ends is left out, and doesn't lengthen it.
    session.dispatch("timeline.caption_add", &json!({"canvas": C, "id": TIMELINE, "text": "late", "start_us": 5_000_000, "end_us": 6_000_000})).unwrap();

    let request = json!({"canvas": C, "mode": "selected", "nodes": [ONE, TIMELINE]});
    let q = session.dispatch("run.quote", &request).unwrap();
    assert_eq!(q["allowed"], true, "{q}");
    let mut start = request;
    start["accept"] = json!({"token": q["token"], "partial": q["complete"] != true, "unpriced_jobs": 0, "amounts": {}});
    session.dispatch("run.start", &start).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut notices = Vec::new();
    loop {
        notices.extend(host.pump(&mut session));
        if !host.is_busy() {
            break;
        }
        assert!(Instant::now() < deadline, "the run never finished: {notices:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!notices.iter().any(|n| matches!(n, RunNotice::Failed { .. } | RunNotice::Refused { .. })), "{notices:?}");
    let node = session.document().canvases().get(&C).unwrap().nodes().get(&TIMELINE).unwrap().clone();
    let result = node.history.iter().find(|g| Some(g.id) == node.active).unwrap();
    let Some(Value::Media(out)) = result.outputs.first() else { panic!("no video: {result:?}") };
    assert_eq!((out.mime.essence(), out.meta.duration_ms), ("video/webm", Some(2000)));
    let dir = tempfile::tempdir().unwrap();
    let rendered = dir.path().join("out.webm");
    std::fs::write(&rendered, session.assets().get(&out.hash).unwrap()).unwrap();
    for at in ["0.2", "1.5"] {
        let (max, _) = brightest(&ffmpeg, &rendered, at);
        assert!(max < 60, "no caption at {at} s: {max}");
    }
    let (max, bright) = brightest(&ffmpeg, &rendered, "0.8");
    assert!(max > 200 && bright > 50, "the caption shows at 0.8 s: {max}, {bright} bright pixels");
}
