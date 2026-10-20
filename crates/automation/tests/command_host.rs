#![cfg(not(target_arch = "wasm32"))]
//! Shared admission happens before any trusted frontend dispatch callback.
use nodecraft_automation::{
    blank_session,
    commands::CommandHost,
    control::{Request, answer_with},
};
use nodecraft_engine::{Session, commands::Registry};
use nodecraft_types::{CanvasId, Error, ErrorKind, ProjectId};
use serde_json::{Value, json};
struct Observed {
    session: Session,
    calls: usize,
}
impl CommandHost for Observed {
    fn session(&self) -> &Session {
        &self.session
    }
    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        self.calls += 1;
        self.session.dispatch(id, params)
    }
}
#[test]
fn human_only_and_unclassified_commands_never_reach_the_host() {
    let session = blank_session(ProjectId::from_bytes([1; 16]), CanvasId::from_bytes([2; 16])).unwrap();
    let mut registry = Registry::builtins().unwrap();
    let spec = *registry.get("node.title").unwrap();
    for id in ["run.selected", "test.unclassified"] {
        registry.register(nodecraft_engine::commands::CommandSpec { id, ..spec }).unwrap();
    }
    let mut host = Observed { session: Session::with_registry(session.document().clone(), registry).unwrap(), calls: 0 };
    for id in ["keys.set", "keys.remove", "file.open", "file.save", "usage.reconcile_ownership", "run.selected", "test.unclassified"] {
        let request = Request::RunCommand(json!({"id":id,"params":{}}).as_object().unwrap().clone());
        assert_eq!(answer_with(&mut host, &request).unwrap().unwrap_err().kind, ErrorKind::Unsupported);
        assert_eq!(host.calls, 0);
    }
    let list = answer_with(&mut host, &Request::ListCommands(Default::default())).unwrap().unwrap();
    assert!(list["commands"].as_array().unwrap().iter().all(|entry| !matches!(
        entry["id"].as_str(),
        Some("keys.set" | "keys.remove" | "file.open" | "file.save" | "usage.reconcile_ownership" | "run.selected" | "test.unclassified")
    )));
    // Paid is not a separate exclusion: classified run.start reaches engine admission, which
    // still refuses absent/invalid quote and host resources. No provider is installed here.
    assert!(nodecraft_automation::mcp::visible("run.start"));
    let request = Request::RunCommand(json!({"id":"run.start","params":{}}).as_object().unwrap().clone());
    assert!(answer_with(&mut host, &request).unwrap().is_err());
    assert_eq!(host.calls, 1);
    assert!(answer_with(&mut host, &Request::Screenshot).is_none());
}
