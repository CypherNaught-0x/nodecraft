//! File commands remain outside MCP until read/write root authorization exists (#116).
use nodecraft_automation::mcp::McpServer;
use nodecraft_engine::Session;
use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_types::{CanvasId, Error, NodeId, ProjectId};
use serde_json::{Value, json};
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
#[test]
fn storage_enabled_file_commands_are_hidden_and_refused_without_io_or_state_changes() {
    let canvas = CanvasId::from_bytes([1; 16]);
    let node = NodeId::from_bytes([2; 16]);
    let mut project = Project::new(ProjectId::from_bytes([3; 16]), "MCP storage").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }, Change::AddNode { canvas, node: Node::new(node, "source.text") }])
        .apply(&mut project)
        .unwrap();
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
    for (id, params) in
        [("file.open", json!({"location":location,"discard_dirty":true})), ("file.save", json!({})), ("file.save_as", json!({"location":location}))]
    {
        assert!(!commands.iter().any(|command| command["id"] == id));
        let reply = call(&mut server, "command_run", json!({"id":id,"params":params}));
        assert_eq!(reply["isError"], true);
        assert_eq!(reply["content"][0]["text"], format!("unsupported: {id} can only be run by a person in the app"));
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
    assert_eq!(
        server.session().expect("this test uses the local MCP backend").document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos.x,
        2.0
    );
    assert_eq!(call(&mut server, "command_run", json!({"id":"edit.undo"}))["isError"], false);
    assert_eq!(server.session().expect("this test uses the local MCP backend").document(), &before.0);
    assert_eq!(call(&mut server, "command_run", json!({"id":"node.move","params":movement(3)}))["isError"], false);
    assert_eq!(
        server.session().expect("this test uses the local MCP backend").document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos.x,
        3.0
    );
    assert_eq!(storage.0.borrow().calls, [0, 0, 0, 3, 0, 0]);
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
    for (id, params) in
        [("file.open", json!({"location":location,"discard_dirty":true})), ("file.save", json!({})), ("file.save_as", json!({"location":location}))]
    {
        assert!(!listed["commands"].as_array().unwrap().iter().any(|command| command["id"] == id));
        let request = Request::RunCommand(json!({"id":id,"params":params}).as_object().unwrap().clone());
        let error = answer(&mut session, &request).unwrap().unwrap_err();
        assert_eq!(error.kind, nodecraft_types::ErrorKind::Unsupported);
        assert_eq!(error.to_string(), format!("{id} can only be run by a person in the app"));
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
