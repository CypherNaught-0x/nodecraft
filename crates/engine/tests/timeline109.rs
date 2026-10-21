//! The Timeline (#109) end to end: two imported videos wired into a Timeline, one trimmed through
//! the clip commands, and the node run through the run host into one WebM in the project. Runs a
//! real ffmpeg: opt in with NODECRAFT_VIDEO_TEST_FFMPEG and NODECRAFT_VIDEO_TEST_FFPROBE.
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

const C: CanvasId = CanvasId::from_bytes([81; 16]);
const ONE: NodeId = NodeId::from_bytes([1; 16]);
const TWO: NodeId = NodeId::from_bytes([2; 16]);
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

/// A small test-pattern MP4 with a tone.
fn video(ffmpeg: &Path, seconds: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("in.mp4");
    let status = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10", "-f", "lavfi", "-i", "sine=frequency=440"])
        .args(["-t", seconds, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::read(path).unwrap()
}

#[test]
#[ignore = "runs a real ffmpeg"]
fn wired_videos_render_through_the_run_host_into_one_webm() {
    let (ffmpeg, ffprobe) = (tool("NODECRAFT_VIDEO_TEST_FFMPEG"), tool("NODECRAFT_VIDEO_TEST_FFPROBE"));
    let snapshot = GenerationCatalog::new(RunCatalog::new(Catalog::from_toml("").unwrap()).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([82; 16]), "Timeline").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "timeline".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let mut host = RunHost::new(Arc::new(Threads(Instant::now())), budget).with_retry(RetryPolicy { max_attempts: 1, ..Default::default() });
    host.set_video_tools(Some((&ffmpeg, &ffprobe)));
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    host.install(&mut session);
    let (one, two) = (video(&ffmpeg, "2"), video(&ffmpeg, "1"));
    let files = [Incoming { name: "one.mp4", bytes: &one }, Incoming { name: "two.mp4", bytes: &two }];
    import_files(&mut session, C, Position { x: 0.0, y: 0.0 }, 300.0, &files, &[ONE, TWO]).unwrap();
    session.dispatch("node.create", &json!({"canvas": C, "id": TIMELINE, "kind": "video.timeline", "pos": {"x": 400, "y": 0}})).unwrap();
    for (name, value) in [("width", 64), ("height", 48)] {
        session.dispatch("node.param_set", &json!({"canvas": C, "id": TIMELINE, "name": name, "value": Value::Unsigned(Unsigned(value))})).unwrap();
    }
    for (n, from) in [(1u8, ONE), (2, TWO)] {
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": TIMELINE, "port": "clips"}, "order": n});
        session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    // Drop the first half second of the first video and cross into the second.
    let listed = session.dispatch("timeline.list", &json!({"canvas": C, "id": TIMELINE})).unwrap();
    // An imported video's length isn't known until the render probes it.
    assert_eq!(listed["clips"][0]["wired"], json!(true), "{listed}");
    let first = listed["clips"][0]["id"].clone();
    session.dispatch("timeline.update", &json!({"canvas": C, "id": TIMELINE, "clip": first, "set": {"in_us": 500_000, "transition_us": 300_000}})).unwrap();

    let request = json!({"canvas": C, "mode": "selected", "nodes": [ONE, TWO, TIMELINE]});
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
    assert_eq!((out.mime.essence(), out.meta.duration_ms), ("video/webm", Some(2200)));
    let bytes = session.assets().get(&out.hash).unwrap().to_vec();
    assert_eq!(bytes.get(..4), Some([0x1a, 0x45, 0xdf, 0xa3].as_slice()), "the project holds the WebM");
}
