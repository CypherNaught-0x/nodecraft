use super::*;
use nodecraft_automation::control::{Connection, Step};
use serde_json::{Value, json};

fn connection() -> Connection {
    let token = Token::from_bytes([7; 32]);
    let mut connection = Connection::new(token.clone());
    assert!(matches!(connection.step(&json!({"id":1,"method":"hello","params":{"token":token.to_hex()}}).to_string()), Step::Reply(_)));
    assert!(connection.is_authed());
    connection
}
fn request(connection: &mut Connection, method: &str, params: Value) -> Request {
    match connection.step(&json!({"id":2,"method":method,"params":params}).to_string()) {
        Step::Ask(_, request) => request,
        other => panic!("request wasn't admitted: {other:?}"),
    }
}
fn draw(app: &mut NodeCraftApp, ctx: &egui::Context) {
    let mut output = ctx
        .run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), ..Default::default() }, |ui| {
            app.show(ui)
        });
    output.textures_delta.clear();
}

fn run(app: &mut NodeCraftApp, ctx: &egui::Context, connection: &mut Connection, id: &str, params: Value) -> Result<Value, nodecraft_types::Error> {
    let request = request(connection, "command.run", json!({"id":id,"params":params}));
    answer(app, ctx, &request).unwrap()
}
fn list(app: &mut NodeCraftApp, ctx: &egui::Context, connection: &mut Connection) -> Value {
    let request = request(connection, "commands.list", json!({"query":"canvas."}));
    answer(app, ctx, &request).unwrap().unwrap()
}

#[test]
fn authenticated_live_control_navigation_changes_view_without_document_edits() {
    use nodecraft_engine::navigation_cmds::NavigationContext;
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    draw(&mut app, &ctx);
    let document = app.session.document().clone();
    let revision = app.session.revision();
    let events = app.session.events_since(0).unwrap();
    let history = (app.session.can_undo(), app.session.can_redo());
    let mut connection = connection();
    let entries = list(&mut app, &ctx, &mut connection);
    for id in ["canvas.fit_all", "canvas.minimap_toggle"] {
        assert_eq!(entries["commands"].as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["ready"], true);
    }
    let sentinel = NavigationContext { canvas: app.canvas, viewport_available: false, blocked: true, fit_all: false, fit_selection: false };
    app.session.swap_resource(Some(sentinel));
    let before = app.view;
    let response = run(&mut app, &ctx, &mut connection, "canvas.pan_to", json!({"x":1234,"y":-567})).unwrap();
    assert_ne!(app.view, before);
    let center = app.view.to_world(app.viewport.min, app.viewport.center());
    assert!((center.x - 1234.0).abs() < 0.01 && (center.y + 567.0).abs() < 0.01);
    assert_eq!(response["result"]["zoom"].as_f64().unwrap(), f64::from(app.view.zoom));
    assert_eq!(response["revision"], revision);
    let restored = app.session.swap_resource::<NavigationContext>(None).unwrap();
    assert!(restored.blocked && !restored.viewport_available && !restored.fit_all);
    let panned = app.view;
    run(&mut app, &ctx, &mut connection, "canvas.fit_all", json!({})).unwrap();
    assert_ne!(app.view, panned);
    let id = *app.session.document().canvases().get(&app.canvas).unwrap().nodes().keys().next().unwrap();
    app.selection.only(id);
    run(&mut app, &ctx, &mut connection, "canvas.fit_selection", json!({})).unwrap();
    let visible = app.navigation.visible;
    let response = run(&mut app, &ctx, &mut connection, "canvas.minimap_toggle", json!({})).unwrap();
    assert_eq!(app.navigation.visible, !visible);
    assert_eq!(response["result"]["minimap_visible"], !visible);
    assert_eq!(app.session.document(), &document);
    assert_eq!(app.session.revision(), revision);
    assert_eq!(app.session.events_since(0).unwrap(), events);
    assert_eq!((app.session.can_undo(), app.session.can_redo()), history);
    assert!(app.session.swap_resource::<NavigationContext>(None).is_none());
}

#[test]
fn busy_live_ownership_invalid_params_and_missing_viewport_preserve_state_and_resources() {
    use egui_kittest::{Harness, kittest::Queryable};
    use nodecraft_engine::navigation_cmds::NavigationContext;
    use nodecraft_types::ErrorKind;
    let ctx = egui::Context::default();
    let mut app = NodeCraftApp::headless().unwrap();
    let mut connection = connection();
    assert_eq!(run(&mut app, &ctx, &mut connection, "canvas.pan_to", json!({"x":0,"y":0})).unwrap_err().kind, ErrorKind::Conflict);
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Text").click();
    h.run();
    let ctx = h.ctx.clone();
    assert!(nodecraft_ui_egui::canvas::text_input_focused(&ctx));
    let document = h.state().session.document().clone();
    let view = h.state().view;
    let revision = h.state().session.revision();
    let sentinel = NavigationContext { canvas: h.state().canvas, viewport_available: false, blocked: true, fit_all: false, fit_selection: false };
    h.state_mut().session.swap_resource(Some(sentinel));
    assert_eq!(run(h.state_mut(), &ctx, &mut connection, "canvas.pan_to", json!({"x":0,"y":0})).unwrap_err().kind, ErrorKind::Conflict);
    let entries = list(h.state_mut(), &ctx, &mut connection);
    assert_eq!(entries["commands"].as_array().unwrap().iter().find(|e| e["id"] == "canvas.fit_all").unwrap()["ready"], false);
    let focused = ctx.memory(|memory| memory.focused()).unwrap();
    ctx.memory_mut(|memory| memory.surrender_focus(focused));
    h.run();
    for popup in [false, true] {
        if popup {
            egui::Popup::open_id(&ctx, egui::Id::new("control-owner"));
        } else {
            ctx.set_dragged_id(egui::Id::new("control-gesture"));
        }
        assert_eq!(run(h.state_mut(), &ctx, &mut connection, "canvas.minimap_toggle", json!({})).unwrap_err().kind, ErrorKind::Conflict);
        if popup {
            egui::Popup::close_all(&ctx);
        } else {
            ctx.stop_dragging();
        }
    }
    assert_eq!(run(h.state_mut(), &ctx, &mut connection, "canvas.pan_to", json!({"x":1e13,"y":0})).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(
        run(h.state_mut(), &ctx, &mut connection, "canvas.pan_to", json!({"x":0,"y":0,"payload":"x".repeat(1024 * 1024)})).unwrap_err().kind,
        ErrorKind::InvalidInput
    );
    let restored = h.state_mut().session.swap_resource::<NavigationContext>(None).unwrap();
    assert!(restored.blocked && !restored.viewport_available);
    assert_eq!(h.state().view, view);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert!(!h.state().session.can_undo() && !h.state().session.can_redo());
}

#[test]
fn graph_commands_headless_refusal_and_screenshot_pending_keep_their_paths() {
    use nodecraft_types::ErrorKind;
    let mut app = NodeCraftApp::headless().unwrap();
    let ctx = egui::Context::default();
    draw(&mut app, &ctx);
    let mut connection = connection();
    let before = app.session.document().clone();
    let id = *app.session.document().canvases().get(&app.canvas).unwrap().nodes().keys().next().unwrap();
    let canvas = app.canvas;
    let response = run(&mut app, &ctx, &mut connection, "node.title", json!({"canvas":canvas,"id":id,"title":"Control edit"})).unwrap();
    assert_eq!(response["revision"], 1);
    assert!(app.session.can_undo());
    run(&mut app, &ctx, &mut connection, "edit.undo", json!({})).unwrap();
    assert_eq!(app.session.document(), &before);
    assert!(app.session.can_redo());
    for (id, params) in
        [("canvas.fit_all", json!({})), ("canvas.fit_selection", json!({})), ("canvas.minimap_toggle", json!({})), ("canvas.pan_to", json!({"x":0,"y":0}))]
    {
        let headless = request(&mut connection, "command.run", json!({"id":id,"params":params}));
        assert_eq!(control::answer(&mut app.session, &headless).unwrap().unwrap_err().kind, ErrorKind::Unsupported);
    }
    assert!(answer(&mut app, &ctx, &Request::Screenshot).is_none(), "the pump still handles screenshots asynchronously");
    for id in ["keys.set", "keys.remove"] {
        assert_eq!(run(&mut app, &ctx, &mut connection, id, json!({"account":"fixture"})).unwrap_err().kind, ErrorKind::Unsupported);
    }
    assert_eq!(app.session.document(), &before);
    assert_eq!(app.session.revision(), 2);
}

#[test]
fn active_minimap_drag_refuses_external_navigation_until_release() {
    use egui_kittest::Harness;
    use nodecraft_engine::navigation_cmds::NavigationContext;
    use nodecraft_types::ErrorKind;
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let node = *app.session.document().canvases().get(&canvas).unwrap().nodes().keys().next().unwrap();
    app.session.dispatch("node.title", &json!({"canvas":canvas,"id":node,"title":"First"})).unwrap();
    app.session.dispatch("node.title", &json!({"canvas":canvas,"id":node,"title":"Second"})).unwrap();
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert!(app.session.can_undo() && app.session.can_redo());
    let mut h = Harness::builder().with_size(egui::vec2(1000.0, 700.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    let map = nodecraft_ui_egui::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
    let from = map.center() + egui::vec2(20.0, 0.0);
    h.hover_at(from);
    h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.state().navigation.drag.is_some() && h.state().navigation.pointer_owned);
    let ctx = h.ctx.clone();
    let mut connection = connection();
    let sentinel = NavigationContext { canvas, viewport_available: false, blocked: true, fit_all: false, fit_selection: false };
    h.state_mut().session.swap_resource(Some(sentinel));
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    let events = h.state().session.events_since(0).unwrap();
    let history = (h.state().session.can_undo(), h.state().session.can_redo());
    let selection = h.state().selection.clone();
    let view = h.state().view;
    let entries = list(h.state_mut(), &ctx, &mut connection);
    for id in ["canvas.fit_all", "canvas.minimap_toggle"] {
        assert_eq!(entries["commands"].as_array().unwrap().iter().find(|entry| entry["id"] == id).unwrap()["ready"], false);
    }
    // Both external availability and dispatch must reject the internal minimap-pan exemption.
    let mut live = LiveCommands { app: h.state_mut(), ctx: &ctx };
    assert_eq!(
        nodecraft_automation::commands::CommandHost::enabled(&mut live, "canvas.pan_to", &json!({"x":12345,"y":6789})).unwrap_err().kind,
        ErrorKind::Conflict
    );
    assert_eq!(run(h.state_mut(), &ctx, &mut connection, "canvas.pan_to", json!({"x":12345,"y":6789})).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(h.state().view, view);
    assert!(h.state().navigation.drag.is_some() && h.state().navigation.pointer_owned);
    // Held pointer movement still pans internally through the original scoped bridge.
    h.hover_at(from + egui::vec2(35.0, 25.0));
    h.run();
    assert_ne!(h.state().view, view);
    let to = from + egui::vec2(35.0, 25.0);
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    assert!(h.state().navigation.drag.is_none() && !h.state().navigation.pointer_owned);
    let released = h.state().view;
    run(h.state_mut(), &ctx, &mut connection, "canvas.pan_to", json!({"x":12345,"y":6789})).unwrap();
    assert_ne!(h.state().view, released);
    assert_eq!(h.state().selection, selection);
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert_eq!(h.state().session.events_since(0).unwrap(), events);
    assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), history);
    let restored = h.state_mut().session.swap_resource::<NavigationContext>(None).unwrap();
    assert!(restored.blocked && !restored.viewport_available && !restored.fit_all && !restored.fit_selection);
}
