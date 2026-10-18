//! Comments use the same admitted Session transaction through actual JSON-RPC MCP calls.
use nodecraft_automation::mcp::{AGENT_COMMANDS, HUMAN_ONLY, McpServer};
use nodecraft_engine::Session;
use nodecraft_format::{MemoryStorage, ProjectStorage, Snapshot, StorageLocation, journal};
use nodecraft_graph::{Canvas, Change, Project};
use nodecraft_types::{CanvasId, CommentId, Error, ProjectId};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

const CANVAS: CanvasId = CanvasId::from_bytes([2; 16]);
const ROOT: CommentId = CommentId::from_bytes([3; 16]);
const REPLY: CommentId = CommentId::from_bytes([4; 16]);
#[derive(Clone, Default)]
struct Storage(Rc<RefCell<MemoryStorage>>);
impl ProjectStorage for Storage {
    fn load(&mut self, location: &StorageLocation) -> Result<Vec<u8>, Error> {
        self.0.borrow_mut().load(location)
    }
    fn save(&mut self, location: &StorageLocation, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().save(location, bytes)
    }
    fn checkpoint(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().checkpoint(bytes)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().append(bytes)
    }
}
fn setup() -> (McpServer, Storage) {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Public discussion fixture").unwrap();
    Change::AddCanvas { canvas: Canvas::new(CANVAS, "Canvas") }.apply(&mut project).unwrap();
    let storage = Storage::default();
    let session = Session::with_storage(Snapshot { project, assets: Default::default(), epoch: 0 }, Box::new(storage.clone())).unwrap();
    (McpServer::new(session, Box::new(|| [9; 16])), storage)
}
fn tool(server: &mut McpServer, name: &str, arguments: Value) -> Value {
    let reply = server.handle(&json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":arguments}}).to_string()).unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    reply["result"].clone()
}
fn command(server: &mut McpServer, id: &str, params: Value) -> Value {
    tool(server, "command_run", json!({"id":id,"params":params}))
}
fn recovered(server: &McpServer, storage: &Storage) {
    let recovered = journal::recover(&storage.0.borrow().recovery).unwrap();
    assert!(!recovered.legacy && recovered.damage.is_none());
    assert_eq!(&recovered.snapshot.project, server.session().unwrap().document());
}
fn success(server: &mut McpServer, storage: &Storage, id: &str, params: Value) {
    let reply = command(server, id, params);
    assert_eq!(reply["isError"], false, "{id}: {reply}");
    recovered(server, storage);
}
#[test]
fn discussion_mutations_are_discoverable_undoable_and_recover_through_real_mcp() {
    let (mut server, storage) = setup();
    let listed = tool(&mut server, "command_list", json!({}));
    let commands = listed["structuredContent"]["commands"].as_array().unwrap();
    for id in ["comment.create", "comment.edit", "comment.reply", "comment.reply_remove", "comment.move", "comment.resolve", "comment.remove"] {
        assert!(AGENT_COMMANDS.contains(&id));
        let spec = commands.iter().find(|spec| spec["id"] == id).unwrap();
        assert_eq!(spec["params_schema"]["type"], "object");
    }
    for id in HUMAN_ONLY {
        assert!(!commands.iter().any(|spec| spec["id"] == *id));
    }
    success(&mut server, &storage, "comment.create", json!({"canvas":CANVAS,"id":ROOT,"text":"Root","pos":{"x":5,"y":7}}));
    success(&mut server, &storage, "comment.reply", json!({"canvas":CANVAS,"id":ROOT,"reply":REPLY,"text":"Reply"}));
    success(&mut server, &storage, "comment.edit", json!({"canvas":CANVAS,"id":ROOT,"text":"Edited root"}));
    success(&mut server, &storage, "comment.move", json!({"canvas":CANVAS,"id":ROOT,"pos":{"x":8,"y":9}}));
    success(&mut server, &storage, "comment.resolve", json!({"canvas":CANVAS,"id":ROOT,"resolved":true}));
    let resolved = server.session().unwrap().document().clone();
    success(&mut server, &storage, "edit.undo", json!({}));
    assert!(!server.session().unwrap().document().canvases().get(&CANVAS).unwrap().comments().get(&ROOT).unwrap().resolved);
    success(&mut server, &storage, "edit.redo", json!({}));
    assert_eq!(server.session().unwrap().document(), &resolved);
    let before = server.session().unwrap().document().clone();
    let revision = server.session().unwrap().revision();
    let history = (server.session().unwrap().can_undo(), server.session().unwrap().can_redo());
    let events = server.session().unwrap().events_since(0).unwrap();
    let recovery = storage.0.borrow().recovery.clone();
    for (id, params) in [
        ("comment.edit", json!({"canvas":CANVAS,"id":ROOT,"text":"x".repeat(1024*1024+1)})),
        ("comment.edit", json!({"canvas":CANVAS,"id":ROOT,"text":"safe","actor":"untrusted"})),
        ("comment.reply", json!({"canvas":CANVAS,"id":ROOT,"reply":CommentId::from_bytes([5;16]),"text":"Resolved replies require reopen"})),
        ("comment.open", json!({"id":ROOT})),
        ("comment.compose", json!({"pos":{"x":0,"y":0}})),
        ("canvas.comments_toggle", json!({})),
    ] {
        let reply = command(&mut server, id, params);
        assert_eq!(reply["isError"], true, "{id}: {reply}");
        if matches!(id, "comment.open" | "comment.compose" | "canvas.comments_toggle") {
            assert!(HUMAN_ONLY.contains(&id));
        }
        let session = server.session().unwrap();
        assert_eq!(session.document(), &before);
        assert_eq!(session.revision(), revision);
        assert_eq!((session.can_undo(), session.can_redo()), history);
        assert_eq!(session.events_since(0).unwrap(), events);
        assert_eq!(storage.0.borrow().recovery, recovery);
        assert!(!session.file_status().recovery_required);
    }
    success(&mut server, &storage, "comment.resolve", json!({"canvas":CANVAS,"id":ROOT,"resolved":false}));
    let with_reply = server.session().unwrap().document().clone();
    success(&mut server, &storage, "comment.reply_remove", json!({"canvas":CANVAS,"id":ROOT,"reply":REPLY}));
    success(&mut server, &storage, "comment.remove", json!({"canvas":CANVAS,"id":ROOT}));
    assert!(server.session().unwrap().document().canvases().get(&CANVAS).unwrap().comments().is_empty());
    success(&mut server, &storage, "edit.undo", json!({}));
    success(&mut server, &storage, "edit.undo", json!({}));
    assert_eq!(server.session().unwrap().document(), &with_reply);
}
