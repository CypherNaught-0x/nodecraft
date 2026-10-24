//! Every agent command can run somewhere (#360): the view commands through the live window,
//! everything else headless as well.
use super::*;
use nodecraft_automation::commands::CommandHost;
use nodecraft_automation::mcp::{AGENT_COMMANDS, HUMAN_ONLY, LIVE_ONLY};
use nodecraft_types::{ErrorKind, NodeId};

#[test]
fn every_live_only_agent_command_is_routed_to_the_window_and_only_those() {
    let app = NodeCraftApp::headless().unwrap();
    for id in AGENT_COMMANDS {
        assert_eq!(LIVE_ONLY.contains(id), NodeCraftApp::is_host_command(id), "{id}: LIVE_ONLY and the live host's routing disagree");
    }
    for id in LIVE_ONLY {
        assert!(AGENT_COMMANDS.contains(id), "{id} is live-only but agents can't run it");
    }
    for spec in app.session.registry().specs().filter(|spec| NodeCraftApp::is_host_command(spec.id)) {
        assert!(LIVE_ONLY.contains(&spec.id) || HUMAN_ONLY.contains(&spec.id), "{} is a host command nobody classified", spec.id);
    }
}

/// A canvas with three cards in a row, none of them selected.
fn three_cards() -> (NodeCraftApp, egui::Context, Vec<NodeId>) {
    let mut app = NodeCraftApp::headless().unwrap();
    let canvas = app.canvas;
    let ids: Vec<_> = (0u8..3).map(|n| NodeId::from_bytes([200 + n; 16])).collect();
    for (n, id) in ids.iter().enumerate() {
        let x = 900.0 * n as f64;
        app.session.dispatch("node.create", &json!({"canvas":canvas,"id":id,"kind":"source.text","pos":{"x":x,"y":40.0 * n as f64}})).unwrap();
    }
    app.selection.clear();
    let ctx = egui::Context::default();
    draw(&mut app, &ctx);
    (app, ctx, ids)
}

#[test]
fn an_agent_formats_named_cards_it_never_selected_in_one_undo_step() {
    let (mut app, ctx, ids) = three_cards();
    let canvas = app.canvas;
    let positions =
        |app: &NodeCraftApp| ids.iter().map(|id| app.session.document().canvases().get(&canvas).unwrap().nodes().get(id).unwrap().pos).collect::<Vec<_>>();
    let before = positions(&app);
    let revision = app.session.revision();
    let params = json!({"mode":"grid","columns":1,"nodes":ids});
    {
        let mut live = LiveCommands { app: &mut app, ctx: &ctx, roots: None };
        live.enabled("canvas.auto_format", &params).unwrap();
        assert_eq!(live.impact("canvas.auto_format", &params).unwrap(), nodecraft_engine::impact::Impact::Edit);
    }
    assert_eq!(positions(&app), before, "judging a format moves nothing");
    let mut connection = connection();
    let response = run(&mut app, &ctx, &mut connection, "canvas.auto_format", params).unwrap();
    assert_eq!(response["result"]["moved"], 3, "{response}");
    assert_ne!(positions(&app), before);
    assert_eq!(app.session.revision(), revision + 1);
    assert!(app.selection.is_empty(), "the agent's list doesn't become the person's selection");
    assert!(app.session.swap_resource::<nodecraft_engine::layout_cmds::LayoutContext>(None).is_none());
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    assert_eq!(positions(&app), before);
}

#[test]
fn an_agent_frames_named_cards_without_changing_the_persons_selection() {
    let (mut app, ctx, ids) = three_cards();
    app.selection.only(ids[0]);
    // Keep a real redo entry available: framing must neither journal a step nor clear redo.
    let canvas = app.canvas;
    app.session.dispatch("node.move", &json!({"canvas":canvas,"id":ids[0],"pos":{"x":50.0,"y":0.0}})).unwrap();
    app.session.dispatch("edit.undo", &json!({})).unwrap();
    let (document, revision, selection) = (app.session.document().clone(), app.session.revision(), app.selection.clone());
    let events = app.session.events_since(0).unwrap();
    let history = (app.session.can_undo(), app.session.can_redo());
    assert_eq!(history, (true, true));
    let mut connection = connection();
    let result = run(&mut app, &ctx, &mut connection, "canvas.fit_selection", json!({"nodes":[ids[1],ids[2]]})).unwrap();
    assert!(result["result"]["zoom"].as_f64().is_some(), "{result}");
    assert_eq!(result["result"]["clamped"], false, "these two cards fit without a zoom clamp: {result}");
    assert_eq!(result["revision"], revision);
    for id in [ids[1], ids[2]] {
        let rect = *app.navigation.geometry.rects.get(&id).unwrap();
        let screen = app.view.rect_to_screen(app.viewport.min, rect);
        assert!(app.viewport.contains_rect(screen), "named card must fit entirely: {screen:?} outside {:?}", app.viewport);
    }
    let aside = app.view.rect_to_screen(app.viewport.min, *app.navigation.geometry.rects.get(&ids[0]).unwrap());
    assert!(!app.viewport.intersects(aside), "the selected, off-to-the-side card must not be framed: {aside:?}");
    assert_eq!((app.session.document(), app.session.revision()), (&document, revision));
    assert_eq!(app.selection, selection, "agent framing cannot select its named cards");
    assert_eq!(app.session.events_since(0).unwrap(), events, "framing must not emit a document event");
    assert_eq!((app.session.can_undo(), app.session.can_redo()), history, "framing must preserve undo and redo");
    let max = (nodecraft_engine::navigation_cmds::specs().into_iter().find(|spec| spec.id == "canvas.fit_selection").unwrap().input_schema)();
    let max = usize::try_from(max["properties"]["nodes"]["maxItems"].as_u64().unwrap()).unwrap();
    let bounds = format!("between 2 and {max} nodes");
    for (nodes, message) in [
        (json!([]), bounds.as_str()),
        (json!(vec![ids[1]; max + 1]), bounds.as_str()),
        (json!([ids[1], ids[1]]), "fit selection nodes are invalid"),
        (json!([ids[1], NodeId::from_bytes([9; 16])]), "fit selection nodes are invalid"),
        (json!([ids[0]]), bounds.as_str()),
    ] {
        let before = app.view;
        let error = run(&mut app, &ctx, &mut connection, "canvas.fit_selection", json!({"nodes":nodes})).unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Conflict | ErrorKind::InvalidInput), "{error:?}");
        assert!(error.message.contains(message), "invalid nodes need an actionable error: {error:?}");
        assert_eq!(app.view, before, "invalid nodes cannot move the camera");
        assert_eq!(app.selection, selection);
        assert_eq!(app.session.events_since(0).unwrap(), events);
        assert_eq!((app.session.can_undo(), app.session.can_redo()), history);
    }
    app.session.dispatch("edit.redo", &json!({})).unwrap();
    assert_ne!(app.session.document(), &document, "the original redo action remains available");
    assert!(app.session.events_since(0).unwrap().len() > events.len(), "the ordinary document event still fires on redo");
}

#[test]
fn a_named_card_that_isnt_on_the_shown_canvas_formats_nothing() {
    let (mut app, ctx, ids) = three_cards();
    let revision = app.session.revision();
    let mut connection = connection();
    for nodes in [json!([ids[0], NodeId::from_bytes([9; 16])]), json!([ids[0], ids[0]]), json!([ids[0]])] {
        let error = run(&mut app, &ctx, &mut connection, "canvas.auto_format", json!({"mode":"grid","nodes":nodes})).unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Conflict | ErrorKind::InvalidInput), "{nodes}: {error:?}");
        assert_eq!(app.session.revision(), revision);
    }
}

#[test]
fn an_agent_opens_discussions_and_the_tag_filter_in_the_window() {
    let (mut app, ctx, ids) = three_cards();
    let canvas = app.canvas;
    let root = nodecraft_types::CommentId::from_bytes([31; 16]);
    app.session.dispatch("comment.create", &json!({"canvas":canvas,"id":root,"text":"Look here","node":ids[1],"pos":{"x":0,"y":0}})).unwrap();
    draw(&mut app, &ctx);
    let (document, revision) = (app.session.document().clone(), app.session.revision());
    let mut connection = connection();
    let open = app.comments.open;
    run(&mut app, &ctx, &mut connection, "canvas.comments_toggle", json!({})).unwrap();
    assert_eq!(app.comments.open, !open);
    draw(&mut app, &ctx);
    run(&mut app, &ctx, &mut connection, "comment.open", json!({"id":root})).unwrap();
    assert!(app.comments.open);
    assert_eq!(app.comments.active, Some(root));
    draw(&mut app, &ctx);
    run(&mut app, &ctx, &mut connection, "canvas.tag_filter_open", json!({})).unwrap();
    assert!(app.tag_filter.open);
    draw(&mut app, &ctx);
    run(&mut app, &ctx, &mut connection, "canvas.tag_filter_set", json!({"tags":[],"untagged":true})).unwrap();
    draw(&mut app, &ctx);
    run(&mut app, &ctx, &mut connection, "canvas.tag_filter_close", json!({})).unwrap();
    assert!(!app.tag_filter.open);
    assert_eq!((app.session.document(), app.session.revision()), (&document, revision), "view commands leave the document alone");
    let refused = run(&mut app, &ctx, &mut connection, "comment.compose", json!({"pos":{"x":0,"y":0}})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
}

#[test]
fn no_live_only_command_reaches_the_engine_without_its_capability() {
    let (mut app, ctx, ids) = three_cards();
    let canvas = app.canvas;
    let root = nodecraft_types::CommentId::from_bytes([32; 16]);
    app.session.dispatch("comment.create", &json!({"canvas":canvas,"id":root,"text":"Pinned","pos":{"x":0,"y":0}})).unwrap();
    draw(&mut app, &ctx);
    let mut connection = connection();
    for id in LIVE_ONLY {
        let params = match *id {
            "canvas.pan_to" => json!({"x":0,"y":0}),
            "canvas.search_query" => json!({"query":"text"}),
            "canvas.auto_format" => json!({"mode":"grid","nodes":ids}),
            "comment.open" => json!({"id":root}),
            "canvas.tag_filter_set" => json!({"tags":[],"untagged":true}),
            _ => json!({}),
        };
        let result = run(&mut app, &ctx, &mut connection, id, params);
        if ["canvas.pan_to", "canvas.auto_format", "canvas.comments_toggle", "canvas.tag_filter_open"].contains(id) {
            assert!(result.is_ok(), "{id} must run in the window: {result:?}");
        }
        if let Err(error) = result {
            assert!(!error.message.contains("host capability"), "{id} reached the engine without its capability: {error:?}");
            assert_ne!(error.kind, ErrorKind::Unsupported, "{id}: {error:?}");
        }
        draw(&mut app, &ctx);
    }
}
