//! The window saves and opens projects for a control client, inside the folders it was started
//! with (#423): no dialog, whatever has focus.
use super::*;

#[test]
fn a_control_client_saves_and_opens_projects_inside_the_allowed_folders() {
    let base = tempfile::tempdir().unwrap();
    let (work, elsewhere) = (base.path().join("work"), base.path().join("elsewhere"));
    for dir in [&work, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let ctx = egui::Context::default();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
    let storage = nodecraft_ui_egui::app::storage::StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: None };
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(storage)).unwrap();
    draw(&mut app, &ctx);
    let roots = FileRoots::new(&[], std::slice::from_ref(&work)).unwrap();
    let mut connection = connection();
    let mut run = |app: &mut NodeCraftApp, id: &str, params: Value| {
        let request = request(&mut connection, "command.run", json!({"id":id,"params":params}));
        answer(app, &ctx, &roots, &request).unwrap()
    };

    let outside = elsewhere.join("a.ncraft");
    let error = run(&mut app, "file.save_as", json!({"location":outside.to_str().unwrap()})).unwrap_err();
    assert_eq!(error.kind, nodecraft_types::ErrorKind::Forbidden);
    assert!(!outside.exists(), "nothing is written outside");

    let saved = work.join("a.ncraft");
    run(&mut app, "file.save_as", json!({"location":saved.to_str().unwrap()})).unwrap();
    assert!(saved.is_file(), "the project file is on disk");
    let title = app.session.document().title().to_owned();
    run(&mut app, "file.save", json!({})).unwrap();

    // Another project replaces this one: open the saved file again over it.
    run(&mut app, "file.open", json!({"location":saved.to_str().unwrap()})).unwrap();
    assert_eq!(app.session.document().title(), title);
    let resolved = std::fs::canonicalize(&saved).unwrap();
    assert_eq!(format!("{:?}", app.session.file_status().location), format!("Some(Path({resolved:?}))"), "opened from the resolved path");

    // Without folders the same window refuses, saying how to allow them.
    let request = request(&mut connection, "command.run", json!({"id":"file.save","params":{}}));
    let error = answer(&mut app, &ctx, &FileRoots::default(), &request).unwrap().unwrap_err();
    assert!(error.message.contains("--root"), "{error}");
}

#[test]
fn an_mcp_bridge_saves_inside_the_allowed_folders_and_is_refused_outside() {
    const CHILD: &str = "NODECRAFT_TEST_FILES_BRIDGE_CHILD";
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
        use std::io::Read;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        struct Reaped(std::process::Child);
        impl Drop for Reaped {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let path = std::env::temp_dir().join(format!("nodecraft-files423-{}.log", std::process::id()));
        let log = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        let mut child = Reaped(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "control::tests::files423::an_mcp_bridge_saves_inside_the_allowed_folders_and_is_refused_outside",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap(),
        );
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "MCP subprocess exceeded deadline; childguard kills and reaps it");
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut output = String::new();
        std::fs::File::open(&path).unwrap().take(64 * 1024).read_to_string(&mut output).unwrap();
        println!("{output}");
        assert!(status.success(), "MCP subprocess failed; retained log {}", path.display());
        std::fs::remove_file(path).unwrap();
        return;
    }
    use std::time::{Duration, Instant};
    let base = tempfile::tempdir().unwrap();
    let (work, elsewhere) = (base.path().join("work"), base.path().join("elsewhere"));
    for dir in [&work, &elsewhere] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let (inside, outside) = (work.join("b.ncraft"), elsewhere.join("b.ncraft"));
    let token = Token::from_bytes([42; 32]);
    let listener = control::listen_with(
        token.clone(),
        Arc::new(|| {}),
        control::Timeouts { handshake: Duration::from_secs(1), idle: Duration::from_secs(1), write: Duration::from_secs(1), answer: Duration::from_secs(1) },
    )
    .unwrap();
    let addr = listener.addr;
    let calls = [
        ("file.save_as", json!({"location":outside.to_str().unwrap()})),
        ("file.save_as", json!({"location":inside.to_str().unwrap()})),
        ("file.save", json!({})),
        ("file.open", json!({"location":inside.to_str().unwrap()})),
    ];
    let worker = std::thread::spawn(move || -> Result<Vec<Value>, String> {
        let client = control::Client::connect(addr, &token.to_hex()).map_err(|e| e.to_string())?;
        let mut mcp = nodecraft_automation::mcp::McpServer::bridge(client, Box::new(|| [7; 16]));
        let mut replies = Vec::new();
        for (id, params) in calls {
            let reply = mcp
                .handle(
                    &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"command_run","arguments":{"id":id,"params":params}}}).to_string(),
                )
                .ok_or("missing MCP reply")?;
            replies.push(serde_json::from_str(&reply).map_err(|e| e.to_string())?);
        }
        Ok(replies)
    });
    let ctx = egui::Context::default();
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(base.path().join("settings")));
    let storage = nodecraft_ui_egui::app::storage::StorageSetup { recovery: base.path().join("settings/recovery/session.journal"), open: None };
    let mut app = NodeCraftApp::with_settings_and_storage(&ctx, store, Some(storage)).unwrap();
    draw(&mut app, &ctx);
    let roots = FileRoots::new(&[], std::slice::from_ref(&work)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !worker.is_finished() && Instant::now() < deadline {
        match listener.calls.recv_timeout(Duration::from_millis(50)) {
            Ok(call) => {
                let response = answer(&mut app, &ctx, &roots, &call.request).unwrap();
                call.reply(response);
                draw(&mut app, &ctx);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("listener disconnected: {error}"),
        }
    }
    let finished_in_time = worker.is_finished();
    drop(listener);
    let replies = worker.join().unwrap().unwrap();
    assert!(finished_in_time, "MCP worker did not finish within its bounded host pump");
    assert_eq!(replies[0]["result"]["isError"], true, "{}", replies[0]);
    assert!(replies[0].to_string().contains("forbidden"), "{}", replies[0]);
    assert!(!outside.exists(), "nothing is written outside");
    assert_eq!(replies[1]["result"]["isError"], false, "{}", replies[1]);
    assert!(inside.is_file(), "the project file is on disk");
    assert_eq!(replies[2]["result"]["isError"], false, "{}", replies[2]);
    assert_eq!(replies[3]["result"]["isError"], false, "{}", replies[3]);
    let resolved = std::fs::canonicalize(&inside).unwrap();
    assert_eq!(format!("{:?}", app.session.file_status().location), format!("Some(Path({resolved:?}))"));
}
