//! Startup registers a lazy worker: native file locks never block the app thread.
use super::*;
use nodecraft_engine::commands::Services;
use nodecraft_plugin_store::{FolderBackend, StoreBackend};

struct Directory(std::path::PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn result(session: &mut nodecraft_engine::Session, pending: &str) -> Result<serde_json::Value, nodecraft_types::Error> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match session.dispatch("plugins.result", &json!({"pending":pending})) {
            Ok(reply) if matches!(reply["status"].as_str(), Some("completed" | "failed")) => return Ok(reply),
            Ok(_) => (),
            Err(error) if error.kind == nodecraft_types::ErrorKind::Busy => (),
            _ => return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, "unexpected worker reply")),
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::yield_now();
    }
}

#[test]
fn plugin_startup_is_lazy_and_native_contention_is_observable_then_recoverable() {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    let name: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let dir = Directory(std::env::temp_dir().join(format!("nodecraft-plugin431-{name}")));
    let mut services = Services::default();
    NodeCraftApp::register_plugins(&mut services, dir.0.clone()).unwrap();
    assert!(!dir.0.exists(), "registration must not open or create the store");
    let backend = FolderBackend::new(dir.0.join("store")).unwrap();
    let lock = backend.transaction().unwrap();
    let mut session = NodeCraftApp::headless().unwrap().session.with_services(services);
    let before = (session.document().clone(), session.revision(), session.events_since(0).unwrap());
    let first = session.dispatch("plugins.list", &json!({})).unwrap();
    let id = first["pending"].as_str().unwrap();
    let failed = result(&mut session, id).unwrap();
    assert_eq!(failed["status"], "failed");
    assert_eq!(failed["error"]["kind"], serde_json::to_value(nodecraft_types::ErrorKind::Busy).unwrap());
    drop(lock);
    let next = session.dispatch("plugins.list", &json!({})).unwrap();
    let done = result(&mut session, next["pending"].as_str().unwrap()).unwrap();
    assert_eq!(done["status"], "completed");
    assert_eq!(done["result"], json!({"packages":[]}));
    assert_eq!(session.dispatch("plugins.result", &json!({"pending":id})).unwrap(), failed, "retry never overwrites the previous failure");
    assert_eq!((session.document().clone(), session.revision(), session.events_since(0).unwrap()), before);
    assert!(!session.can_undo());
}
