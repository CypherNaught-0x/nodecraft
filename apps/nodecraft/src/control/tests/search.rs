use super::*;
use egui_kittest::Harness;

fn search_app() -> (NodeCraftApp, nodecraft_types::NodeId) {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let id = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    app.session.dispatch("node.title", &json!({"canvas":canvas,"id":id,"title":"Silver comet"})).unwrap();
    app.session.dispatch("node.move", &json!({"canvas":canvas,"id":id,"pos":{"x":5000,"y":100}})).unwrap();
    (app, id)
}

#[test]
fn authenticated_live_search_opens_and_frames_the_actual_match() {
    let (app, id) = search_app();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.state_mut().view.pan = egui::Vec2::ZERO;
    h.state_mut().view.zoom = 1.0;
    h.run();
    assert!(!h.state().viewport.contains(h.state().view.to_screen(h.state().viewport.min, egui::pos2(5000.0, 100.0))));
    let ctx = h.ctx.clone();
    let mut connection = connection();
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let view = h.state().view;
    run(h.state_mut(), &ctx, &mut connection, "canvas.search_open", json!({})).unwrap();
    assert!(h.state().search.open);
    run(h.state_mut(), &ctx, &mut connection, "canvas.search_query", json!({"query":"silver comet"})).unwrap();
    for _ in 0..8 {
        if !h.state().search.scanning {
            break;
        }
        h.step();
    }
    assert!(!h.state().search.scanning);
    assert_eq!(h.state().search.current, Some(id));
    assert!(h.state().selection.contains(id));
    assert_ne!(h.state().view, view);
    assert!(h.state().viewport.contains(h.state().view.to_screen(h.state().viewport.min, egui::pos2(5000.0, 100.0))));
    for command in ["canvas.search_next", "canvas.search_previous", "canvas.search_close"] {
        run(h.state_mut(), &ctx, &mut connection, command, json!({})).unwrap();
    }
    assert!(!h.state().search.open);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
}

#[test]
fn active_minimap_refuses_every_external_search_then_release_restores_it() {
    use nodecraft_automation::commands::CommandHost;
    use nodecraft_engine::search_cmds::SearchContext;
    use nodecraft_types::ErrorKind;
    let (mut app, id) = search_app();
    app.session.dispatch("node.title", &json!({"canvas":app.canvas,"id":id,"title":"Temporary"})).unwrap();
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let ctx = h.ctx.clone();
    let mut connection = connection();
    run(h.state_mut(), &ctx, &mut connection, "canvas.search_open", json!({})).unwrap();
    run(h.state_mut(), &ctx, &mut connection, "canvas.search_query", json!({"query":"silver"})).unwrap();
    h.run();
    // End search-field editing before the real minimap press; an active editor correctly
    // prevents the map acquiring a gesture in the first place.
    if let Some(focused) = ctx.memory(|memory| memory.focused()) {
        ctx.memory_mut(|memory| memory.surrender_focus(focused));
    }
    h.run();
    let map = nodecraft_ui_egui::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    let from = map.center();
    h.hover_at(from);
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.state().navigation.pointer_owned && h.state().navigation.drag.is_some());
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    assert_eq!(history, (true, true));
    let selection = h.state().selection.clone();
    let view = h.state().view;
    let search = (h.state().search.open, h.state().search.query.clone(), h.state().search.current);
    let sentinel = SearchContext { canvas: h.state().canvas, viewport_available: false, blocked: true, open: false, has_query: false };
    h.state_mut().session.swap_resource(Some(sentinel));
    for (id, params) in [
        ("canvas.search_open", json!({})),
        ("canvas.search_query", json!({"query":"other"})),
        ("canvas.search_next", json!({})),
        ("canvas.search_previous", json!({})),
        ("canvas.search_close", json!({})),
    ] {
        let mut host = LiveCommands { app: h.state_mut(), ctx: &ctx, roots: None };
        assert_eq!(host.enabled(id, &params).unwrap_err().kind, ErrorKind::Conflict);
        assert_eq!(run(h.state_mut(), &ctx, &mut connection, id, params).unwrap_err().kind, ErrorKind::Conflict);
    }
    let entries = list(h.state_mut(), &ctx, &mut connection);
    assert_eq!(entries["commands"].as_array().unwrap().iter().find(|e| e["id"] == "canvas.search_open").unwrap()["ready"], false);
    assert_eq!(h.state().view, view);
    assert_eq!(h.state().selection, selection);
    assert_eq!((h.state().search.open, h.state().search.query.clone(), h.state().search.current), search);
    let to = map.right_bottom() + egui::vec2(50.0, 40.0);
    h.hover_at(to);
    h.run();
    assert_ne!(h.state().view, view, "internal minimap movement remains live outside the map");
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(!h.state().navigation.pointer_owned && h.state().navigation.drag.is_none());
    run(h.state_mut(), &ctx, &mut connection, "canvas.search_query", json!({"query":"silver comet"})).unwrap();
    h.run();
    assert_eq!(h.state().search.current, Some(id));
    let restored = h.state_mut().session.swap_resource::<SearchContext>(None).unwrap();
    assert!(restored.blocked && !restored.viewport_available && !restored.open);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
}

#[test]
fn live_search_restores_scoped_resources_on_bad_params_and_headless_stays_unsupported() {
    use nodecraft_engine::search_cmds::SearchContext;
    use nodecraft_types::ErrorKind;
    let (mut app, _) = search_app();
    let ctx = egui::Context::default();
    let mut connection = connection();
    assert_eq!(run(&mut app, &ctx, &mut connection, "canvas.search_open", json!({})).unwrap_err().kind, ErrorKind::Conflict);
    draw(&mut app, &ctx);
    let sentinel = SearchContext { canvas: app.canvas, viewport_available: false, blocked: true, open: false, has_query: false };
    app.session.swap_resource(Some(sentinel));
    run(&mut app, &ctx, &mut connection, "canvas.search_open", json!({})).unwrap();
    let document = app.session.document().clone();
    let revision = app.session.revision();
    let events = app.session.events_since(0).unwrap();
    let history = (app.session.can_undo(), app.session.can_redo());
    for params in [json!({"query":"é".repeat(513)}), json!({"query":true}), json!({"query":"ok","unknown":1})] {
        assert_eq!(run(&mut app, &ctx, &mut connection, "canvas.search_query", params).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    assert!(app.search.query.is_empty());
    let restored = app.session.swap_resource::<SearchContext>(None).unwrap();
    assert!(restored.blocked && !restored.viewport_available && !restored.open);
    for (id, params) in [
        ("canvas.search_open", json!({})),
        ("canvas.search_query", json!({"query":"silver"})),
        ("canvas.search_next", json!({})),
        ("canvas.search_previous", json!({})),
        ("canvas.search_close", json!({})),
    ] {
        let req = request(&mut connection, "command.run", json!({"id":id,"params":params}));
        assert_eq!(control::answer(&mut app.session, &req).unwrap().unwrap_err().kind, ErrorKind::Unsupported);
    }
    assert!(app.session.swap_resource::<SearchContext>(None).is_none());
    assert_eq!(app.session.document(), &document);
    assert_eq!(app.session.revision(), revision);
    assert_eq!(app.session.events_since(0).unwrap(), events);
    assert_eq!((app.session.can_undo(), app.session.can_redo()), history);
}

#[test]
fn mcp_bridge_search_uses_the_real_live_host_and_frames_the_match() {
    const CHILD: &str = "NODECRAFT_TEST_SEARCH_BRIDGE_CHILD";
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
        let path = std::env::temp_dir().join(format!("nodecraft-search300-{}.log", std::process::id()));
        let log = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        let mut child = Reaped(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "control::tests::search::mcp_bridge_search_uses_the_real_live_host_and_frames_the_match", "--nocapture", "--test-threads=1"])
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
            assert!(start.elapsed() < Duration::from_secs(10), "MCP subprocess exceeded deadline; childguard kills and reaps it");
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
    let token = Token::from_bytes([23; 32]);
    let listener = control::listen_with(
        token.clone(),
        Arc::new(|| {}),
        control::Timeouts { handshake: Duration::from_secs(1), idle: Duration::from_secs(1), write: Duration::from_secs(1), answer: Duration::from_secs(1) },
    )
    .unwrap();
    let addr = listener.addr;
    let worker = std::thread::spawn(move || -> Result<(), String> {
        let client = control::Client::connect(addr, &token.to_hex()).map_err(|e| e.to_string())?;
        let mut mcp = nodecraft_automation::mcp::McpServer::bridge(client, Box::new(|| [6; 16]));
        for (id, params) in [("canvas.search_open", json!({})), ("canvas.search_query", json!({"query":"silver comet"})), ("canvas.search_next", json!({}))] {
            let reply = mcp
                .handle(
                    &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"command_run","arguments":{"id":id,"params":params}}}).to_string(),
                )
                .ok_or("missing MCP reply")?;
            let reply: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
            if reply["result"]["isError"] != false {
                return Err(format!("MCP search failed: {reply}"));
            }
        }
        Ok(())
    });
    let (mut app, id) = search_app();
    let ctx = egui::Context::default();
    draw(&mut app, &ctx);
    app.view.pan = egui::Vec2::ZERO;
    app.view.zoom = 1.0;
    draw(&mut app, &ctx);
    assert!(!app.viewport.contains(app.view.to_screen(app.viewport.min, egui::pos2(5000.0, 100.0))));
    let document = app.session.document().clone();
    let revision = app.session.revision();
    let events = app.session.events_since(0).unwrap();
    let history = (app.session.can_undo(), app.session.can_redo());
    let view = app.view;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker.is_finished() && Instant::now() < deadline {
        match listener.calls.recv_timeout(Duration::from_millis(50)) {
            Ok(call) => {
                let response = answer(&mut app, &ctx, &FileRoots::default(), &call.request).unwrap();
                call.reply(response);
                draw(&mut app, &ctx);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("listener disconnected: {error}"),
        }
    }
    let finished_in_time = worker.is_finished();
    drop(listener); // Disconnect/drop queued calls before joining, including assertion-failure paths.
    let result = worker.join().unwrap();
    assert!(finished_in_time, "MCP worker did not finish within its bounded host pump");
    result.unwrap();
    assert!(app.search.open && !app.search.scanning);
    assert_eq!(app.search.current, Some(id));
    assert!(app.selection.contains(id));
    assert_ne!(app.view, view);
    assert!(app.viewport.contains(app.view.to_screen(app.viewport.min, egui::pos2(5000.0, 100.0))));
    assert_eq!(app.session.document(), &document);
    assert_eq!(app.session.revision(), revision);
    assert_eq!(app.session.events_since(0).unwrap(), events);
    assert_eq!((app.session.can_undo(), app.session.can_redo()), history);
}
