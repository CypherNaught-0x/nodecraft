//! Pointer-driven regressions for display-only discussion placement.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};

fn harness(size: Vec2) -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_size(size).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap())
}
fn pointer(h: &mut Harness<'static, NodeCraftApp>, at: Pos2, pressed: bool) {
    h.hover_at(at);
    h.run();
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    h.run();
}
fn root(h: &Harness<'static, NodeCraftApp>, id: CommentId) -> nodecraft_graph::Comment {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap().comments().get(&id).unwrap().clone()
}

#[test]
fn selected_badge_beside_right_panel_does_not_fall_back_over_the_node_preview() {
    let mut h = harness(Vec2::new(1100.0, 800.0));
    let canvas = h.state().canvas;
    let node = NodeId::from_bytes([3; 16]);
    let id = CommentId::from_bytes([98; 16]);
    let pos = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos;
    h.state_mut().view.zoom = 1.0;
    h.state_mut().view.pan = Vec2::new(650.0 - pos.x, 240.0 - pos.y);
    h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Occluded node","node":node,"pos":{"x":0,"y":0}})).unwrap();
    h.run();
    let original = h.state().session.document().clone();
    let label = format!("Discussion pin {id}");
    let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
    h.get_by_role_and_label(Role::Button, &label).click();
    h.run();
    assert_eq!(h.state().comments.active, Some(id), "notice={:?}, from={from:?}", h.state().notice);
    let shown = h.get_by_role_and_label(Role::Button, &label).rect();
    let body = h.state().view.rect_to_screen(h.state().viewport.min, h.state().navigation.geometry.rects[&node]);
    assert!(!shown.intersects(body));
    assert!(!shown.intersects(h.state().discussion_rect()));
    assert!(h.state().viewport.contains_rect(shown));
    assert_eq!(h.state().session.document(), &original);
}

#[test]
fn right_chrome_stacks_below_search_and_stays_disjoint_from_tags_at_small_sizes() {
    for size in [Vec2::new(420.0, 360.0), Vec2::new(420.0, 800.0), Vec2::new(1100.0, 800.0)] {
        let mut h = harness(size);
        for search in [false, true] {
            h.state_mut().search.open = search;
            h.run();
            let button = h.get_by_role_and_label(Role::Button, "Comments").rect();
            let app = h.state();
            assert!((button.right() - (app.viewport.right() - app.theme.tokens().navigation_margin)).abs() < 1.0);
            assert!(button.top() >= app.search_rect().bottom() + app.theme.tokens().navigation_gap - 1.0);
            let view = h.state().view;
            h.get_by_role_and_label(Role::Button, "Comments").click();
            h.run();
            assert_eq!(h.state().view, view, "Comments click must not also pan the minimap");
            assert!(h.state().comments.open, "real right-side button must own its click: size={size:?}, search={search}, notice={:?}", h.state().notice);
            h.state_mut().tag_filter.open = true;
            h.run();
            let panel = h.state().discussion_rect();
            let actual = h.ctx.memory(|memory| memory.area_rect(egui::Id::new("discussions_panel"))).unwrap();
            assert!(h.state().viewport.contains_rect(actual), "actual panel must fit: {actual:?}");
            assert!(!actual.intersects(h.state().tag_rect()));
            assert!(h.state().viewport.contains_rect(panel));
            if size.y >= 800.0 {
                assert!(panel.bottom() <= h.state().navigation.usable(h.state().viewport, h.state().theme.tokens()).bottom());
            }
            assert!(!panel.intersects(h.state().tag_rect()));
            assert!(panel.top() >= button.bottom());
            assert!((panel.right() - button.right()).abs() < 1.0);
            h.state_mut().comments.close();
            h.state_mut().tag_filter.open = false;
            h.run();
        }
    }
}

#[test]
fn overlapping_attached_pins_move_right_without_editing_explicit_or_canvas_anchors() {
    for zoom in [0.1, 0.5, 1.0, 2.0, 4.0] {
        let mut h = harness(Vec2::new(1800.0, 900.0));
        let node = NodeId::from_bytes([3; 16]);
        let canvas = h.state().canvas;
        h.state_mut().view.zoom = zoom;
        // Put the card's body origin on screen at every zoom, independently of startup fit.
        let pos = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos;
        h.state_mut().view.pan = Vec2::new(140.0 - pos.x * zoom, 240.0 - pos.y * zoom);
        let attached = CommentId::from_bytes([91; 16]);
        let explicit = CommentId::from_bytes([92; 16]);
        let free = CommentId::from_bytes([93; 16]);
        let explicit_pos = Position { x: 320.0 + 140.0 / zoom, y: 60.0 / zoom };
        for (id, anchor, offset) in
            [(attached, Some(node), Position::default()), (explicit, Some(node), explicit_pos), (free, None, Position { x: pos.x, y: pos.y })]
        {
            h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Geometry","node":anchor,"pos":offset})).unwrap();
        }
        let before = h.state().session.document().clone();
        let revision = h.state().session.revision();
        h.run();
        let pin = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {attached}")).rect();
        let body = h.state().view.rect_to_screen(h.state().viewport.min, h.state().navigation.geometry.rects[&node]);
        let t = h.state().theme.tokens();
        let handle = (t.handle_radius * zoom).max(t.handle_min_screen_radius) + card::PORT_HIT;
        assert!(pin.left() - t.comment_marker_ring >= body.right() + handle + t.comment_marker_gap - 1.0, "zoom {zoom}: {pin:?}, {body:?}");
        assert!(!pin.intersects(body));
        let free_rect = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {free}")).rect();
        let anchor = h.state().view.to_screen(h.state().viewport.min, Pos2::new(pos.x, pos.y));
        assert!((free_rect.min - anchor).length() < 1.0, "free canvas pin must not be moved away from a card");
        assert_eq!(h.state().session.document(), &before);
        assert_eq!(h.state().session.revision(), revision);
        assert_eq!(root(&h, explicit).pos, explicit_pos);
        let explicit_rect = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {explicit}")).rect();
        let explicit_screen = h.state().view.to_screen(h.state().viewport.min, Pos2::new(pos.x + explicit_pos.x, pos.y + explicit_pos.y));
        assert!((explicit_rect.min - explicit_screen).length() < 1.0, "an already-clear explicit offset stays where placed");
    }
}

#[test]
fn right_viewport_edge_never_clamps_a_projected_badge_back_over_the_output() {
    let mut h = harness(Vec2::new(1100.0, 800.0));
    let canvas = h.state().canvas;
    let node = NodeId::from_bytes([3; 16]);
    let body = h.state().navigation.geometry.rects[&node];
    h.state_mut().view.zoom = 1.0;
    let origin = h.state().viewport.right() - 20.0 - body.width();
    h.state_mut().view.pan = Vec2::new(origin - body.left() - h.state().viewport.left(), 180.0 - body.top());
    let id = CommentId::from_bytes([99; 16]);
    let offset = Position { x: body.width(), y: body.height() * 0.5 };
    h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Edge output","node":node,"pos":offset})).unwrap();
    h.run();
    let screen_body = h.state().view.rect_to_screen(h.state().viewport.min, body);
    assert!((screen_body.right() - (h.state().viewport.right() - 20.0)).abs() < 1.0);
    let badge = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {id}")).rect();
    assert!(badge.left() > h.state().viewport.right(), "the offscreen badge must not be clamped back onto the visible card: {badge:?}");
    let output = crate::noodle::anchor(screen_body, crate::noodle::Side::Output, 0, 1);
    let before = h.state().session.document().clone();
    pointer(&mut h, output, true);
    let to = output + Vec2::new(-40.0, 30.0);
    h.hover_at(to);
    h.run();
    assert!(h.state().wiring.is_some(), "the visible output must own its actual edge-of-screen drag");
    assert!(h.state().comments.drag.is_none());
    h.key_press(egui::Key::Escape);
    h.run();
    pointer(&mut h, to, false);
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn attached_marker_over_output_does_not_steal_the_real_wire_drag() {
    for zoom in [0.1, 1.0, 2.0] {
        let mut h = harness(Vec2::new(1800.0, 900.0));
        let canvas = h.state().canvas;
        let node = NodeId::from_bytes([3; 16]);
        h.state_mut().view.zoom = zoom;
        let pos = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos;
        h.state_mut().view.pan = Vec2::new(100.0 - pos.x * zoom, 240.0 - pos.y * zoom);
        h.run();
        let body = h.state().navigation.geometry.rects[&node];
        let offset = Position { x: body.width(), y: body.height() * 0.5 };
        let id = CommentId::from_bytes([97; 16]);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Output overlap","node":node,"pos":offset})).unwrap();
        h.run();
        let output = h.state().view.to_screen(h.state().viewport.min, crate::noodle::anchor(body, crate::noodle::Side::Output, 0, 1));
        let before = h.state().session.document().clone();
        pointer(&mut h, output, true);
        h.hover_at(output + Vec2::new(40.0, 30.0));
        h.run();
        assert!(h.state().wiring.is_some(), "output handle must retain primary drag at zoom {zoom}");
        assert!(h.state().comments.drag.is_none());
        h.key_press(egui::Key::Escape);
        h.run();
        pointer(&mut h, output + Vec2::new(40.0, 30.0), false);
        assert_eq!(h.state().session.document(), &before);
    }
}

#[test]
fn collapsed_members_hide_attached_markers_but_keep_their_stored_anchor() {
    let mut h = harness(Vec2::new(1100.0, 800.0));
    let canvas = h.state().canvas;
    let node = NodeId::from_bytes([3; 16]);
    let id = CommentId::from_bytes([95; 16]);
    let field = nodecraft_types::FieldId::from_bytes([96; 16]);
    h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Member discussion","node":node,"pos":{"x":0,"y":0}})).unwrap();
    h.run();
    let label = format!("Discussion pin {id}");
    h.get_by_role_and_label(Role::Button, &label);
    h.state_mut()
        .session
        .dispatch(
            "field.create",
            &json!({"canvas":canvas,"field":{"id":field,"title":"Group","rect":{"pos":{"x":0,"y":0},"width":320,"height":240}},"nodes":[node]}),
        )
        .unwrap();
    h.state_mut().session.dispatch("field.update", &json!({"canvas":canvas,"field":field,"collapsed":true})).unwrap();
    h.run();
    assert!(h.query_by_role_and_label(Role::Button, &label).is_none());
    assert_eq!(root(&h, id).pos, Position::default());
    assert_eq!(root(&h, id).node, Some(node));
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    h.get_by_role_and_label(Role::Button, &label);
}

#[test]
fn projected_node_pin_click_cancel_drag_and_undo_keep_the_original_offset_contract() {
    for (zoom, pixels_per_point) in [(0.5, 1.0), (1.0, 2.0), (2.0, 1.0), (2.0, 2.0)] {
        let mut h = Harness::builder()
            .with_size(Vec2::new(1800.0, 900.0))
            .with_pixels_per_point(pixels_per_point)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        let canvas = h.state().canvas;
        let node = NodeId::from_bytes([3; 16]);
        let id = CommentId::from_bytes([94; 16]);
        h.state_mut().view.zoom = zoom;
        let pos = h.state().session.document().canvases().get(&canvas).unwrap().nodes().get(&node).unwrap().pos;
        h.state_mut().view.pan = Vec2::new(100.0 - pos.x * zoom, 260.0 - pos.y * zoom);
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Projected","node":node,"pos":{"x":0,"y":0}})).unwrap();
        h.run();
        let original = h.state().session.document().clone();
        let label = format!("Discussion pin {id}");
        let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
        h.get_by_role_and_label(Role::Button, &label).click();
        h.run();
        assert_eq!(
            h.state().comments.active,
            Some(id),
            "zoom={zoom}, ppp={pixels_per_point}, viewport={:?}, notice={:?}, from={from:?}",
            h.state().viewport,
            h.state().notice
        );
        assert_eq!(h.state().session.document(), &original);
        for cancel in [true, false] {
            let from = h.get_by_role_and_label(Role::Button, &label).rect().center();
            pointer(&mut h, from, true);
            let to = from + Vec2::new(30.0, 24.0);
            h.hover_at(to);
            h.run();
            assert!(h.state().comments.drag.is_some());
            assert!(h.state().drag.is_none() && h.state().wiring.is_none());
            assert!((h.get_by_role_and_label(Role::Button, &label).rect().center() - to).length() < 1.0);
            if cancel {
                h.key_press(egui::Key::Escape);
                h.run();
            }
            pointer(&mut h, to, false);
            if cancel {
                assert_eq!(h.state().session.document(), &original);
            } else {
                assert_eq!(root(&h, id).node, Some(node));
                assert_eq!(root(&h, id).pos, Position { x: 30.0 / zoom, y: 24.0 / zoom });
                assert!((h.get_by_role_and_label(Role::Button, &label).rect().center() - to).length() < 1.0);
                h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
                h.run();
                assert_eq!(h.state().session.document(), &original);
            }
        }
    }
}
