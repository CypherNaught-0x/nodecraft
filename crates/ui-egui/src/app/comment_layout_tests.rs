//! Pointer-driven regressions for display-only discussion placement.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};

#[test]
fn fully_offscreen_cards_keep_natural_projection_in_all_four_directions() {
    let t = crate::theme::ThemeKind::default().tokens();
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 800.0));
    for min in [Pos2::new(-1000.0, 300.0), Pos2::new(1300.0, 300.0), Pos2::new(400.0, -1000.0), Pos2::new(400.0, 1000.0)] {
        let body = Rect::from_min_size(min, Vec2::new(320.0, 140.0));
        let decorated = body.expand(26.0);
        assert!(!decorated.intersects(viewport));
        let at = projected_marker(body.min, body, decorated, viewport, t);
        let ordinary = Pos2::new(decorated.right() + t.comment_marker_gap + t.comment_marker_ring, body.top());
        assert_eq!(at, ordinary, "offscreen cards must not acquire viewport-edge pins: {body:?}");
        assert!(!viewport.expand(t.comment_marker_size).contains(at));
    }
}

#[test]
fn panning_a_commented_card_offscreen_removes_ghost_pins_and_panning_back_restores_it() {
    for (zoom, pixels_per_point) in [(0.1, 1.0), (1.0, 2.0), (4.0, 1.0)] {
        for delta in [Vec2::new(-2500.0, 0.0), Vec2::new(2500.0, 0.0), Vec2::new(0.0, -2500.0), Vec2::new(0.0, 2500.0)] {
            let mut h = Harness::builder()
                .with_size(Vec2::new(1800.0, 1000.0))
                .with_pixels_per_point(pixels_per_point)
                .with_max_steps(200)
                .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
            let canvas = h.state().canvas;
            let node = NodeId::from_bytes([3; 16]);
            let id = CommentId::from_bytes([111; 16]);
            let body = h.state().navigation.geometry.rects[&node];
            h.state_mut().view.zoom = zoom;
            h.state_mut().view.pan = Vec2::new(100.0 - body.left() * zoom, 240.0 - body.top() * zoom);
            h.state_mut()
                .session
                .dispatch("comment.create", &json!({"canvas":canvas,"id":id,"node":node,"text":"Pan visibility","pos":{"x":0,"y":0}}))
                .unwrap();
            h.run();
            let document = h.state().session.document().clone();
            let label = format!("Discussion pin {id}");
            h.get_by_role_and_label(Role::Button, &label);
            let original_view = h.state().view;
            for pan in [delta, -delta] {
                h.hover_at(Pos2::new(40.0, 850.0));
                h.event(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: pan,
                    modifiers: egui::Modifiers::NONE,
                });
                h.run();
                let offscreen = pan == delta;
                let shown = h.state().view.rect_to_screen(h.state().viewport.min, body);
                if offscreen {
                    assert!(!shown.expand(40.0).intersects(h.state().viewport), "real pan must move card offscreen: {shown:?}");
                    assert!(h.query_by_role_and_label(Role::Button, &label).is_none(), "no ghost after {delta:?}, zoom={zoom}, dpi={pixels_per_point}");
                } else {
                    h.get_by_role_and_label(Role::Button, &label);
                    assert!((h.state().view.pan - original_view.pan).length() < 1.0);
                }
                assert_eq!(h.state().session.document(), &document);
            }
        }
    }
}

#[test]
fn exact_fractional_clamp_repro_orders_bounds_after_subtraction() {
    let t = crate::theme::ThemeKind::default().tokens();
    let body = Rect::from_min_size(Pos2::new(50.0, -13.97), Vec2::new(32.0, 14.1));
    assert!((body.top() + 30.0) - 30.0 < body.top(), "pin the original f32 cancellation");
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 800.0));
    let at = projected_marker(body.min, body, body.expand(14.0), viewport, t);
    assert!(viewport.contains_rect(Rect::from_min_size(at, Vec2::splat(t.comment_marker_size))));
}

#[test]
fn projected_marker_uses_a_visible_interior_when_the_card_fills_the_viewport() {
    let t = crate::theme::ThemeKind::default().tokens();
    let viewport = Rect::from_min_size(Pos2::ZERO, Vec2::new(420.0, 360.0));
    let body = Rect::from_min_max(Pos2::new(-100.0, -100.0), Pos2::new(800.0, 700.0));
    let at = projected_marker(body.min, body, body.expand(26.0), viewport, t);
    let marker = Rect::from_min_size(at, Vec2::splat(t.comment_marker_size)).expand(t.comment_marker_ring);
    assert!(viewport.contains_rect(marker));
    assert!(marker.left() > body.left() + 26.0 && marker.right() < body.right() - 26.0);
}

#[test]
fn fractional_overview_card_top_does_not_panic_while_projecting_a_marker() {
    let mut h = harness(Vec2::new(1100.0, 800.0));
    let node = NodeId::from_bytes([3; 16]);
    let canvas = h.state().canvas;
    let body = h.state().navigation.geometry.rects[&node];
    h.state_mut().view.zoom = 0.1;
    // Find a representable pan whose *actual transformed* top triggers the -13.97/30
    // cancellation. The viewport origin can otherwise round that literal away.
    let size = h.state().theme.tokens().comment_marker_size;
    let pan = (-20000..0)
        .map(|n| n as f32 / 1000.0 - h.state().viewport.top() - body.top() * 0.1)
        .find(|pan| {
            let top = (h.state().viewport.top() + pan) + body.top() * 0.1;
            (top + size) - size < top
        })
        .unwrap();
    h.state_mut().view.pan = Vec2::new(100.0 - body.left() * 0.1, pan);
    let id = CommentId::from_bytes([101; 16]);
    h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Fractional overview","node":node,"pos":{"x":0,"y":0}})).unwrap();
    h.run();
    h.get_by_role_and_label(Role::Button, &format!("Discussion pin {id}"));
}

#[test]
fn composed_node_anchor_is_world_only_at_every_zoom() {
    let mut saved = None;
    for zoom in [0.1, 0.5, 1.0, 2.0, 4.0] {
        let mut h = harness(Vec2::new(1800.0, 900.0));
        let node = NodeId::from_bytes([3; 16]);
        h.state_mut().view.zoom = zoom;
        let ctx = h.ctx.clone();
        h.state_mut().compose_on_node(&ctx, node, None);
        let key = h.state().comments.editing.unwrap();
        h.state_mut().comments.drafts.get_mut(&key).unwrap().text = "World anchor".into();
        let id = h.state().comments.drafts[&key].target.root();
        h.state_mut().save_discussion(&ctx, false);
        let anchor = root(&h, id).pos;
        assert_eq!(*saved.get_or_insert(anchor), anchor, "saved anchor must not depend on zoom={zoom}");
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        assert!(!h.state().session.document().canvases()[&h.state().canvas].comments().contains_key(&id));
    }
}

#[test]
fn minimap_whitespace_below_actual_short_discussion_panel_still_pans() {
    for (pixels_per_point, one_thread) in [(1.0, false), (2.0, false), (1.0, true), (2.0, true)] {
        let mut h = Harness::builder()
            .with_size(Vec2::new(420.0, 360.0))
            .with_pixels_per_point(pixels_per_point)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        if one_thread {
            let canvas = h.state().canvas;
            h.state_mut()
                .session
                .dispatch("comment.create", &json!({"canvas":canvas,"id":CommentId::from_bytes([107;16]),"text":"One thread","pos":{"x":-1000,"y":-1000}}))
                .unwrap();
        }
        h.state_mut().comments.open = true;
        h.run();
        let actual = h.ctx.memory(|m| m.area_rect(egui::Id::new("discussions_panel"))).unwrap();
        let map = crate::navigation::layout(h.state().viewport, true, h.state().theme.tokens()).map;
        let at = Pos2::new(map.center().x, map.bottom() - 8.0);
        assert!(h.state().discussion_rect().contains(at) && !actual.contains(at), "actual={actual:?}, map={map:?}");
        let before = h.state().view;
        pointer(&mut h, at, true);
        pointer(&mut h, at, false);
        assert_ne!(h.state().view, before, "unpainted panel budget must not steal minimap clicks");
    }
}

#[test]
fn field_drag_preview_moves_attached_badge_and_one_undo_restores_everything() {
    for (zoom, pixels_per_point) in [(0.1, 1.0), (0.5, 2.0), (1.0, 1.0), (2.0, 2.0), (4.0, 1.0)] {
        let mut h = Harness::builder()
            .with_size(Vec2::new(1800.0, 1100.0))
            .with_pixels_per_point(pixels_per_point)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        let canvas = h.state().canvas;
        let node = NodeId::from_bytes([3; 16]);
        let id = CommentId::from_bytes([102; 16]);
        let field = nodecraft_types::FieldId::from_bytes([103; 16]);
        let pos = h.state().session.document().canvases()[&canvas].nodes()[&node].pos;
        h.state_mut().view.zoom = zoom;
        h.state_mut().view.pan = Vec2::new(240.0 - pos.x * zoom, 280.0 - pos.y * zoom);
        h.state_mut().session.dispatch("field.create", &json!({"canvas":canvas,"field":{"id":field,"title":"Badge group","rect":{"pos":{"x":pos.x-100.0/zoom,"y":pos.y-100.0/zoom},"width":400,"height":380}},"nodes":[node]})).unwrap();
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Moving member","node":node,"pos":{"x":0,"y":0}})).unwrap();
        h.run();
        let before = h.state().session.document().clone();
        let label = format!("Discussion pin {id}");
        let badge = h.get_by_role_and_label(Role::Button, &label).rect().min;
        let grab = h.get_by_role_and_label(Role::Button, "Field Badge group").rect().left_center() + Vec2::new(30.0, 0.0);
        pointer(&mut h, grab, true);
        let delta = Vec2::new(70.0, 45.0);
        h.hover_at(grab + delta);
        h.run();
        assert!(h.state().fields.drag.is_some(), "zoom={zoom}");
        assert_eq!(h.state().session.document(), &before, "preview must not mutate the document");
        let moved = h.get_by_role_and_label(Role::Button, &label).rect().min;
        assert!((moved - badge - delta).length() < 1.0, "badge follows field preview at zoom={zoom}: {badge:?} -> {moved:?}");
        pointer(&mut h, grab + delta, false);
        assert_eq!(root(&h, id).pos, Position::default());
        h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
        h.run();
        assert_eq!(h.state().session.document(), &before, "field and pin restore in one undo");
    }
}

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
            let button = h.state().comments_toolbar_rect();
            let app = h.state();
            assert!((button.right() - (app.viewport.right() - app.theme.tokens().navigation_margin)).abs() < 1.0);
            assert!(app.topbar_rect().contains_rect(button));
            if search {
                assert!(app.search_rect().top() >= app.topbar_rect().bottom() + app.theme.tokens().navigation_gap - 1.0);
            }
            let view = h.state().view;
            crate::app::topbar_tests::reveal(&mut h, "Comments");
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
            assert!((panel.right() - (h.state().viewport.right() - h.state().theme.tokens().navigation_margin)).abs() < 1.0);
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
    let ids = [99, 108, 109, 110].map(|n| CommentId::from_bytes([n; 16]));
    let offset = Position { x: body.width(), y: body.height() * 0.5 };
    for id in ids {
        h.state_mut().session.dispatch("comment.create", &json!({"canvas":canvas,"id":id,"text":"Edge output","node":node,"pos":offset})).unwrap();
    }
    h.run();
    let screen_body = h.state().view.rect_to_screen(h.state().viewport.min, body);
    assert!((screen_body.right() - (h.state().viewport.right() - 20.0)).abs() < 1.0);
    for id in ids {
        let badge = h.get_by_role_and_label(Role::Button, &format!("Discussion pin {id}")).rect();
        let t = h.state().theme.tokens();
        let hit = t.handle_radius.max(t.handle_min_screen_radius) + card::PORT_HIT;
        assert!(h.state().viewport.contains_rect(badge), "the projected badge must remain reachable: {badge:?}");
        assert!(
            !badge.expand(t.comment_marker_ring).intersects(screen_body.expand(hit)),
            "coincident left fallbacks must not scatter into input hits: {badge:?}"
        );
    }
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
fn edge_projected_pin_stays_reachable_without_stealing_either_port_at_all_zoom_and_dpi() {
    for zoom in [0.1, 0.5, 1.0, 2.0, 4.0] {
        for pixels_per_point in [1.0, 2.0] {
            for right_edge in [false, true] {
                let mut h = Harness::builder()
                    .with_size(Vec2::new(1800.0, 1000.0))
                    .with_pixels_per_point(pixels_per_point)
                    .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
                let canvas = h.state().canvas;
                let source = NodeId::from_bytes([3; 16]);
                let node = NodeId::from_bytes([104; 16]);
                let id = CommentId::from_bytes([105; 16]);
                let router =
                    h.state().session.kinds().get("structure.router").unwrap().instantiate_instance(node, Default::default(), &Default::default()).unwrap();
                h.state_mut().session.dispatch("node.add", &json!({"canvas":canvas,"node":router})).unwrap();
                h.state_mut().session.dispatch("edge.connect", &json!({"canvas":canvas,"edge":{"id":nodecraft_types::EdgeId::from_bytes([106;16]),"from":{"node":source,"port":"out"},"to":{"node":node,"port":"in"},"order":0}})).unwrap();
                h.run();
                let world = h.state().navigation.geometry.rects[&node];
                h.state_mut().view.zoom = zoom;
                let left = if right_edge { h.state().viewport.right() - 20.0 - world.width() * zoom } else { h.state().viewport.left() + 20.0 };
                h.state_mut().view.pan = Vec2::new(left - h.state().viewport.left() - world.left() * zoom, 220.0 - world.top() * zoom);
                h.state_mut()
                    .session
                    .dispatch(
                        "comment.create",
                        &json!({"canvas":canvas,"id":id,"text":"Port ownership","node":node,"pos":{"x":world.width(),"y":world.height()/2.0}}),
                    )
                    .unwrap();
                h.run();
                let label = format!("Discussion pin {id}");
                let badge = h.get_by_role_and_label(Role::Button, &label).rect();
                assert!(h.state().viewport.contains_rect(badge), "zoom={zoom}, dpi={pixels_per_point}, right={right_edge}: {badge:?}");
                let body = h.state().view.rect_to_screen(h.state().viewport.min, world);
                let before = h.state().session.document().clone();
                for side in [crate::noodle::Side::Output, crate::noodle::Side::Input] {
                    let port = crate::noodle::anchor(body, side, 0, 1);
                    assert!(!badge.expand(h.state().theme.tokens().comment_marker_ring).contains(port));
                    pointer(&mut h, port, true);
                    h.hover_at(port + Vec2::new(if right_edge { -40.0 } else { 40.0 }, 30.0));
                    h.run();
                    assert!(h.state().wiring.is_some(), "port retains drag: zoom={zoom}, dpi={pixels_per_point}, right={right_edge}, side={side:?}");
                    assert!(h.state().comments.drag.is_none());
                    h.key_press(egui::Key::Escape);
                    h.run();
                    pointer(&mut h, port, false);
                    assert_eq!(h.state().session.document(), &before);
                }
                h.get_by_role_and_label(Role::Button, &label).click();
                h.run();
                assert_eq!(h.state().comments.active, Some(id), "fallback pin is actually clickable");
                assert_eq!(h.state().session.document(), &before);
            }
        }
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
