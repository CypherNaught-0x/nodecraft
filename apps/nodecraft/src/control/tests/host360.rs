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
        let mut live = LiveCommands { app: &mut app, ctx: &ctx };
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
        if let Err(error) = run(&mut app, &ctx, &mut connection, id, params) {
            assert!(!error.message.contains("host capability"), "{id} reached the engine without its capability: {error:?}");
            assert_ne!(error.kind, ErrorKind::Unsupported, "{id}: {error:?}");
        }
        draw(&mut app, &ctx);
    }
}
