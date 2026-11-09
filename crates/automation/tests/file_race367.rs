//! An agent racing the person's background open or its own background save (#367): while a file
//! operation runs, MCP commands that would change or replace the project are refused as busy
//! and name `file.cancel`, nothing queues behind it, and once it settles the agent works on the
//! project that is really open.
use nodecraft_automation::commands::CommandHost;
use nodecraft_automation::mcp::McpServer;
use nodecraft_automation::roots::FileRoots;
use nodecraft_engine::Session;
use nodecraft_engine::save_runner::{SaveJob, SaveRunner};
use nodecraft_format::{AssetStore, MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{CanvasId, Error, NodeId, ProjectId};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<MemoryStorage>>);
impl ProjectStorage for Shared {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.lock().unwrap().load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.0.lock().unwrap().append(b)
    }
}

#[derive(Default)]
struct Gate(Mutex<Vec<SaveJob>>);
impl SaveRunner for Gate {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        self.0.lock().unwrap().push(job);
        Ok(())
    }
}
impl Gate {
    fn release(&self) {
        let jobs = std::mem::take(&mut *self.0.lock().unwrap());
        jobs.into_iter().for_each(SaveJob::run);
    }
}

/// The app's window as the MCP host: like the app's frame, it picks up a finished background
/// file operation before answering a request.
struct Window {
    session: Session,
    roots: FileRoots,
}
impl CommandHost for Window {
    fn session(&self) -> &Session {
        &self.session
    }
    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        // The window shows the outcome; this test reads its effect on the session instead.
        let _ = self.session.poll_file();
        self.session.dispatch(id, params)
    }
    fn file_roots(&self) -> Option<&FileRoots> {
        Some(&self.roots)
    }
}

const CANVAS: CanvasId = CanvasId::from_bytes([41; 16]);
const NODE: NodeId = NodeId::from_bytes([42; 16]);
fn project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([43; 16]), "Race").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }, Change::AddNode { canvas: CANVAS, node: Node::new(NODE, "source.text") }])
        .apply(&mut project)
        .unwrap();
    project
}
fn movement(x: i32) -> Value {
    json!({"canvas":CANVAS,"id":NODE,"pos":{"x":x,"y":0}})
}
fn x(server: &McpServer) -> f32 {
    server.session().unwrap().document().canvases()[&CANVAS].nodes()[&NODE].pos.x
}
fn run(server: &mut McpServer, id: &str, params: Value) -> Value {
    let message = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"command_run","arguments":{"id":id,"params":params}}});
    serde_json::from_str::<Value>(&server.handle(&message.to_string()).unwrap()).unwrap()["result"].clone()
}
fn busy(reply: &Value) -> bool {
    reply["isError"] == true && reply["content"][0]["text"].as_str().is_some_and(|text| text.starts_with("busy") && text.contains("file.cancel"))
}

#[test]
fn agent_commands_during_the_persons_background_open_are_refused_and_then_act_on_the_opened_project() {
    let base = tempfile::tempdir().unwrap();
    let work = base.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let file = std::fs::canonicalize(&work).unwrap().join("race.ncraft");
    let path = file.to_str().unwrap().to_owned();
    let gate = Arc::new(Gate::default());
    let mut session =
        Session::with_sendable_storage(Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 }, Box::new(Shared::default())).unwrap();
    session.set_save_runner(gate.clone());
    session.dispatch("node.move", &movement(10)).unwrap();
    session.dispatch("file.save_as", &json!({"location": path})).unwrap();
    // The folder check resolves the real file; the bytes live in memory storage.
    std::fs::write(&file, b"").unwrap();
    session.dispatch("node.move", &movement(20)).unwrap();
    // The person opens the saved file in the background, discarding the unsaved move.
    assert_eq!(session.dispatch("file.open", &json!({"location": path, "discard_dirty": true, "background": true})).unwrap()["opening"], true);
    let roots = FileRoots::new(std::slice::from_ref(&work), std::slice::from_ref(&work)).unwrap();
    let mut server = McpServer::with_host(Window { session, roots }, Box::new(|| [7; 16]));

    for (id, params) in [
        ("node.move", movement(30)),
        ("edit.undo", json!({})),
        ("file.save", json!({})),
        ("file.save_as", json!({"location": path})),
        ("file.open", json!({"location": path})),
    ] {
        let reply = run(&mut server, id, params);
        // Opening over unsaved work is refused before it could even wait: automation never
        // discards it. Everything else is busy and says how to stop the open.
        let refused = if id == "file.open" { reply["isError"] == true } else { busy(&reply) };
        assert!(refused, "{id}: {reply}");
    }
    assert_eq!(x(&server), 20.0, "nothing reached the project being replaced");
    assert!(server.session().unwrap().file_status().operation.is_some());

    gate.release();
    // The next request sees the open's outcome first, then moves the opened project's node.
    let reply = run(&mut server, "node.move", movement(40));
    assert_eq!(reply["isError"], false, "{reply}");
    assert_eq!(x(&server), 40.0);
    let status = server.session().unwrap().file_status();
    assert!(status.operation.is_none() && status.dirty);
    assert_eq!(status.location, Some(StorageLocation::Path(file.clone())));
}

#[test]
fn an_agents_cancelled_background_save_leaves_the_file_unchanged_and_a_racing_open_cant_discard_the_work() {
    let base = tempfile::tempdir().unwrap();
    let work = base.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let file = std::fs::canonicalize(&work).unwrap().join("save-race.ncraft");
    let path = file.to_str().unwrap().to_owned();
    let gate = Arc::new(Gate::default());
    let storage = Shared::default();
    let mut session =
        Session::with_sendable_storage(Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    session.set_save_runner(gate.clone());
    session.dispatch("file.save_as", &json!({"location": path})).unwrap();
    std::fs::write(&file, b"").unwrap();
    let saved = || storage.0.lock().unwrap().files.get(&StorageLocation::Path(file.clone())).cloned().unwrap();
    let before = saved();
    let roots = FileRoots::new(std::slice::from_ref(&work), std::slice::from_ref(&work)).unwrap();
    let mut server = McpServer::with_host(Window { session, roots }, Box::new(|| [7; 16]));
    assert_eq!(run(&mut server, "node.move", movement(5))["isError"], false);

    let started = run(&mut server, "file.save", json!({"background": true}));
    assert_eq!(started["isError"], false, "{started}");
    assert!(server.session().unwrap().file_status().saving);
    // Cancelling is a read of the operation, not a change that waits for it.
    assert_eq!(run(&mut server, "file.cancel", json!({}))["isError"], false);
    assert!(server.session().unwrap().file_status().operation.is_some_and(|operation| operation.cancel_requested));
    gate.release();
    // The next request settles the cancelled save first: the work is still unsaved, so an open
    // (which automation may never use to discard work) is refused rather than losing it.
    let open = run(&mut server, "file.open", json!({"location": path}));
    assert_eq!(open["isError"], true, "{open}");
    assert!(open["content"][0]["text"].as_str().unwrap().starts_with("conflict"), "{open}");
    assert_eq!(saved(), before, "the cancelled save left the file as it was");
    let status = server.session().unwrap().file_status();
    assert!(status.dirty && status.operation.is_none());
    assert_eq!(x(&server), 5.0);
}

/// Signals that the write was reached, then waits for the go-ahead.
type Hold = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
/// Memory storage whose next save is held inside the write, on the job's thread.
#[derive(Clone, Default)]
struct Slow {
    memory: Shared,
    hold: Arc<Mutex<Option<Hold>>>,
}
impl ProjectStorage for Slow {
    fn load(&mut self, l: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.memory.load(l)
    }
    fn save(&mut self, l: &StorageLocation, b: &[u8]) -> Result<(), Error> {
        let hold = self.hold.lock().unwrap().take();
        if let Some((entered, go)) = hold {
            entered.send(()).unwrap();
            go.recv().unwrap();
        }
        self.memory.save(l, b)
    }
    fn checkpoint(&mut self, b: &[u8]) -> Result<(), Error> {
        self.memory.checkpoint(b)
    }
    fn append(&mut self, b: &[u8]) -> Result<(), Error> {
        self.memory.append(b)
    }
}
struct Threads;
impl SaveRunner for Threads {
    fn spawn(&self, job: SaveJob) -> Result<(), SaveJob> {
        std::thread::spawn(move || job.run());
        Ok(())
    }
}

#[test]
fn an_agents_cancel_during_a_save_already_writing_reports_the_save_truthfully() {
    let base = tempfile::tempdir().unwrap();
    let work = base.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let file = std::fs::canonicalize(&work).unwrap().join("writing.ncraft");
    let path = file.to_str().unwrap().to_owned();
    let storage = Slow::default();
    let mut session =
        Session::with_sendable_storage(Snapshot { project: project(), assets: AssetStore::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    session.set_save_runner(Arc::new(Threads));
    session.dispatch("file.save_as", &json!({"location": path})).unwrap();
    std::fs::write(&file, b"").unwrap();
    let roots = FileRoots::new(std::slice::from_ref(&work), std::slice::from_ref(&work)).unwrap();
    let mut server = McpServer::with_host(Window { session, roots }, Box::new(|| [7; 16]));
    assert_eq!(run(&mut server, "node.move", movement(7))["isError"], false);
    let (entered, held) = std::sync::mpsc::channel();
    let (go, wait) = std::sync::mpsc::channel();
    *storage.hold.lock().unwrap() = Some((entered, wait));
    assert_eq!(run(&mut server, "file.save", json!({"background": true}))["isError"], false);
    // The job's thread is inside the write: past the point a cancel can stop.
    held.recv().unwrap();
    assert_eq!(run(&mut server, "file.cancel", json!({}))["isError"], false);
    go.send(()).unwrap();
    // The next request waits for the save, then runs: the file holds the moved node.
    assert_eq!(run(&mut server, "node.move", movement(8))["isError"], false);
    let written = storage.memory.0.lock().unwrap().files.get(&StorageLocation::Path(file.clone())).cloned().unwrap();
    let saved = nodecraft_format::decode(&written).unwrap().project;
    assert_eq!(saved.canvases()[&CANVAS].nodes()[&NODE].pos.x, 7.0, "the save went through despite the late cancel");
    assert_eq!(x(&server), 8.0);
    assert!(server.session().unwrap().file_status().dirty, "only the later move is unsaved");
}
