//! Process lock control isolated from the parallel library lock fixtures.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_plugin_store::{FolderBackend, Store, StoreBackend};
use nodecraft_types::ErrorKind;
use std::time::{Duration, Instant};

// A single explicitly selected fixture in the child; ordinary cargo test performs no work here.
#[test]
fn process_try_open() {
    let Some(root) = std::env::var_os("NODECRAFT_PLUGIN431_TEST_ROOT") else { return };
    let result = Store::open(FolderBackend::new(root.into()).unwrap());
    if std::env::var_os("NODECRAFT_PLUGIN431_TEST_BUSY").is_some() {
        assert_eq!(result.err().unwrap().kind, ErrorKind::Busy);
    } else {
        assert!(result.is_ok());
    }
}

fn child_try_open(root: &std::path::Path, busy: bool) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "process_try_open", "--nocapture"]).env("NODECRAFT_PLUGIN431_TEST_ROOT", root);
    if busy {
        command.env("NODECRAFT_PLUGIN431_TEST_BUSY", "1");
    } else {
        command.env_remove("NODECRAFT_PLUGIN431_TEST_BUSY");
    }
    command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "native lock child exited {status}");
            break;
        }
        assert!(Instant::now() < deadline, "native lock child did not terminate");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_separately_opened_process_is_excluded_until_the_guard_drops() {
    let dir = tempfile::tempdir().unwrap();
    let backend = FolderBackend::new(dir.path().to_owned()).unwrap();
    let held = backend.transaction().unwrap();
    child_try_open(dir.path(), true);
    drop(held);
    child_try_open(dir.path(), false);
}
