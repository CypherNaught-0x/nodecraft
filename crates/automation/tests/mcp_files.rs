//! File commands reach agents only inside the folders the person allowed (#423).
use nodecraft_automation::commands::CommandHost;
use nodecraft_automation::mcp::McpServer;
use nodecraft_automation::roots::FileRoots;
use nodecraft_engine::Session;
use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{CanvasId, Error, NodeId, ProjectId};
use serde_json::{Value, json};
use std::path::Path;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct State {
    memory: MemoryStorage,
    // load, save, checkpoint, append, flush, discard
    calls: [usize; 6],
}
#[derive(Clone, Default)]
struct CountingStorage(Rc<RefCell<State>>);
impl ProjectStorage for CountingStorage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        let mut s = self.0.borrow_mut();
        s.calls[0] += 1;
        s.memory.load(location)
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.calls[1] += 1;
        s.memory.save(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.calls[2] += 1;
        s.memory.checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.calls[3] += 1;
        s.memory.append(bytes)
    }
    fn flush(&mut self) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.calls[4] += 1;
        s.memory.flush()
    }
    fn discard(&mut self) -> Result<(), Error> {
        let mut s = self.0.borrow_mut();
        s.calls[5] += 1;
        s.memory.discard()
    }
}
fn call(server: &mut McpServer, tool: &str, arguments: Value) -> Value {
    let reply = server.handle(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":arguments}}).to_string()).unwrap();
    serde_json::from_str::<Value>(&reply).unwrap()["result"].clone()
}

/// The app's window as a host: a session plus the folders the person allowed.
struct Window {
    session: Session,
    roots: FileRoots,
}
impl CommandHost for Window {
    fn session(&self) -> &Session {
        &self.session
    }
    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        self.session.dispatch(id, params)
    }
    fn file_roots(&self) -> Option<&FileRoots> {
        Some(&self.roots)
    }
}

fn project(seed: u8) -> (Project, CanvasId, NodeId) {
    let canvas = CanvasId::from_bytes([seed; 16]);
    let node = NodeId::from_bytes([seed.wrapping_add(1); 16]);
    let mut project = Project::new(ProjectId::from_bytes([seed.wrapping_add(2); 16]), "MCP storage").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }, Change::AddNode { canvas, node: Node::new(node, "source.text") }])
        .apply(&mut project)
        .unwrap();
    (project, canvas, node)
}

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn without_allowed_folders_file_commands_are_refused_without_io_or_state_changes() {
    let (project, canvas, node) = project(1);
    let storage = CountingStorage::default();
    let mut session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    let movement = |x| json!({"canvas":canvas,"id":node,"pos":{"x":x,"y":0}});
    session.dispatch("node.move", &movement(1)).unwrap();
    session.dispatch("node.move", &movement(2)).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    // Establish a usable save destination through the authorized engine surface.
    let location = "PRIVATE_mcp_storage_destination.ncraft";
    session.dispatch("file.save_as", &json!({"location":location})).unwrap();
    assert!(session.file_status().available);
    assert!(session.can_undo() && session.can_redo());
    let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status(), session.status_revision());
    storage.0.borrow_mut().calls = [0; 6];
    let mut server = McpServer::new(session, Box::new(|| [7; 16]));
    let listed = call(&mut server, "command_list", json!({}));
    assert_eq!(listed["isError"], false);
    let commands = listed["structuredContent"]["commands"].as_array().unwrap();
    // Listed, so an agent learns what to ask for; the parameterless ones say why they can't run.
    let save = commands.iter().find(|command| command["id"] == "file.save").unwrap();
    assert_eq!(save["ready"], false);
    assert!(save["not_ready_because"].as_str().unwrap().contains("--root"));
    for (id, params) in
        [("file.open", json!({"location":location})), ("file.save", json!({})), ("file.save_as", json!({"location":location})), ("file.cancel", json!({}))]
    {
        assert!(commands.iter().any(|command| command["id"] == id), "{id}");
        let reply = call(&mut server, "command_run", json!({"id":id,"params":params}));
        assert_eq!(reply["isError"], true);
        let text = reply["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with(&format!("unsupported: {id} needs folders automation may use")), "{text}");
        assert!(!reply.to_string().contains(location));
        assert_eq!(storage.0.borrow().calls, [0; 6]);
        let session = server.session().expect("this test uses the local MCP backend");
        assert_eq!(
            (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status(), session.status_revision()),
            before
        );
        assert!(session.can_undo() && session.can_redo());
    }
    // Existing history still replays, and a normal allowed edit reaches persistence.
    assert_eq!(call(&mut server, "command_run", json!({"id":"edit.redo"}))["isError"], false);
    assert_eq!(call(&mut server, "command_run", json!({"id":"node.move","params":movement(3)}))["isError"], false);
    assert_eq!(storage.0.borrow().calls, [0, 0, 0, 2, 0, 0]);
}

#[test]
fn an_agent_saves_and_opens_inside_allowed_folders_and_nowhere_else() {
    let base = tempfile::tempdir().unwrap();
    let (work, reference, elsewhere) = (base.path().join("work"), base.path().join("reference"), base.path().join("elsewhere"));
    for dir in [&work, &reference, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let roots = FileRoots::new(std::slice::from_ref(&reference), std::slice::from_ref(&work)).unwrap();
    let (project, canvas, node) = project(21);
    let storage = CountingStorage::default();
    let session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    let mut server = McpServer::with_host(Window { session, roots }, Box::new(|| [7; 16]));
    let run = |server: &mut McpServer, id: &str, params: Value| call(server, "command_run", json!({"id":id,"params":params}));
    let refused = |reply: &Value, kind: &str| {
        assert_eq!(reply["isError"], true, "{reply}");
        assert!(reply["content"][0]["text"].as_str().unwrap().starts_with(kind), "{kind}: {reply}");
    };

    // Outside the write folders, in a read-only one, or by a relative path: refused before any I/O.
    for location in [s(&elsewhere.join("a.ncraft")).to_owned(), s(&reference.join("a.ncraft")).to_owned(), s(&work.join("../elsewhere/a.ncraft")).to_owned()] {
        refused(&run(&mut server, "file.save_as", json!({"location":location})), "forbidden");
    }
    refused(&run(&mut server, "file.save_as", json!({"location":"a.ncraft"})), "invalid_input");
    assert_eq!(storage.0.borrow().calls[1], 0);

    // Inside one: saved to the resolved path, with no dialog.
    let saved = run(&mut server, "file.save_as", json!({"location":s(&work.join("a.ncraft"))}));
    assert_eq!(saved["isError"], false, "{saved}");
    let resolved = std::fs::canonicalize(&work).unwrap().join("a.ncraft");
    let status = server.session().unwrap().file_status();
    assert_eq!(status.location, Some(StorageLocation::Path(resolved.clone())));
    assert_eq!(storage.0.borrow().calls[1], 1);
    let movement = json!({"canvas":canvas,"id":node,"pos":{"x":5,"y":0}});
    assert_eq!(run(&mut server, "node.move", movement)["isError"], false);
    assert_eq!(run(&mut server, "file.save", json!({}))["isError"], false);
    assert_eq!(storage.0.borrow().calls[1], 2);

    // Opening: never discarding unsaved changes, only from an allowed folder.
    assert_eq!(run(&mut server, "node.move", json!({"canvas":canvas,"id":node,"pos":{"x":6,"y":0}}))["isError"], false);
    refused(&run(&mut server, "file.open", json!({"location":s(&work.join("a.ncraft")),"discard_dirty":true})), "conflict");
    assert_eq!(run(&mut server, "file.save", json!({}))["isError"], false);
    std::fs::write(elsewhere.join("b.ncraft"), b"").unwrap();
    refused(&run(&mut server, "file.open", json!({"location":s(&elsewhere.join("b.ncraft"))})), "forbidden");
    let loads = storage.0.borrow().calls[0];
    // MemoryStorage holds the bytes; the file on disk is what the folder check resolves.
    std::fs::write(work.join("a.ncraft"), b"").unwrap();
    let opened = run(&mut server, "file.open", json!({"location":s(&work.join("a.ncraft"))}));
    assert_eq!(opened["isError"], false, "{opened}");
    assert_eq!(storage.0.borrow().calls[0], loads + 1);
    assert_eq!(server.session().unwrap().document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos.x, 6.0);
    assert_eq!(run(&mut server, "file.cancel", json!({}))["isError"], false);
}

#[test]
fn saving_in_place_is_refused_when_the_project_lives_outside_the_write_folders() {
    let base = tempfile::tempdir().unwrap();
    let (work, elsewhere) = (base.path().join("work"), base.path().join("elsewhere"));
    for dir in [&work, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let (project, ..) = project(31);
    let storage = CountingStorage::default();
    let mut session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    // The person saved it there themselves.
    session.dispatch("file.save_as", &json!({"location":s(&elsewhere.join("theirs.ncraft"))})).unwrap();
    let saves = storage.0.borrow().calls[1];
    let mut server = McpServer::with_host(Window { session, roots: FileRoots::new(&[], &[work]).unwrap() }, Box::new(|| [7; 16]));
    let reply = call(&mut server, "command_run", json!({"id":"file.save"}));
    assert_eq!(reply["isError"], true);
    assert!(reply["content"][0]["text"].as_str().unwrap().starts_with("forbidden"), "{reply}");
    assert_eq!(storage.0.borrow().calls[1], saves);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn control_shares_file_refusal_before_storage_and_preserves_history() {
    use nodecraft_automation::control::{Request, answer};
    let canvas = CanvasId::from_bytes([11; 16]);
    let node = NodeId::from_bytes([12; 16]);
    let mut project = Project::new(ProjectId::from_bytes([13; 16]), "Control storage").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }, Change::AddNode { canvas, node: Node::new(node, "source.text") }])
        .apply(&mut project)
        .unwrap();
    let storage = CountingStorage::default();
    let mut session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    let movement = |x| json!({"canvas":canvas,"id":node,"pos":{"x":x,"y":0}});
    session.dispatch("node.move", &movement(1)).unwrap();
    session.dispatch("node.move", &movement(2)).unwrap();
    session.dispatch("edit.undo", &json!({})).unwrap();
    let location = "PRIVATE_control_storage_destination.ncraft";
    session.dispatch("file.save_as", &json!({"location":location})).unwrap();
    let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status(), session.status_revision());
    storage.0.borrow_mut().calls = [0; 6];
    let listed = answer(&mut session, &Request::ListCommands(Default::default())).unwrap().unwrap();
    for (id, params) in [("file.open", json!({"location":location})), ("file.save", json!({})), ("file.save_as", json!({"location":location}))] {
        assert!(listed["commands"].as_array().unwrap().iter().any(|command| command["id"] == id));
        let request = Request::RunCommand(json!({"id":id,"params":params}).as_object().unwrap().clone());
        let error = answer(&mut session, &request).unwrap().unwrap_err();
        assert_eq!(error.kind, nodecraft_types::ErrorKind::Unsupported);
        assert!(error.to_string().starts_with(&format!("{id} needs folders automation may use")), "{error}");
        assert!(!error.to_string().contains(location));
        assert_eq!(storage.0.borrow().calls, [0; 6]);
        assert_eq!(
            (session.document().clone(), session.revision(), session.events_since(0).unwrap(), session.file_status(), session.status_revision()),
            before
        );
        assert!(session.can_undo() && session.can_redo());
    }
    for arguments in [json!({"id":"edit.redo"}), json!({"id":"edit.undo"}), json!({"id":"node.move","params":movement(3)})] {
        answer(&mut session, &Request::RunCommand(arguments.as_object().unwrap().clone())).unwrap().unwrap();
    }
    assert_eq!(session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos.x, 3.0);
    assert_eq!(storage.0.borrow().calls, [0, 0, 0, 3, 0, 0]);
    session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(session.document(), &before.0);
}
