//! Fields on the canvas (#66), driven through real key and pointer events.
use super::tests::harness;
use super::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use nodecraft_types::FieldId;

fn canvas<'a>(h: &'a Harness<'static, NodeCraftApp>) -> &'a Canvas {
    h.state().session.document().canvases().get(&h.state().canvas).unwrap()
}
fn member(h: &Harness<'static, NodeCraftApp>) -> NodeId {
    *canvas(h).nodes().keys().next().unwrap()
}
fn only_field(h: &Harness<'static, NodeCraftApp>) -> FieldId {
    assert_eq!(canvas(h).fields().len(), 1);
    *canvas(h).fields().keys().next().unwrap()
}
fn press(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, button: egui::PointerButton, pressed: bool) {
    h.hover_at(pos);
    h.event(egui::Event::PointerButton { pos, button, pressed, modifiers: egui::Modifiers::NONE });
    h.run();
}
fn drag(h: &mut Harness<'static, NodeCraftApp>, from: Pos2, by: Vec2) {
    press(h, from, egui::PointerButton::Primary, true);
    h.hover_at(from + by * 0.5);
    h.run();
    h.hover_at(from + by);
    h.run();
    press(h, from + by, egui::PointerButton::Primary, false);
}
/// The fixture card's rect on screen, if it is drawn.
fn card(h: &Harness<'static, NodeCraftApp>) -> Option<Rect> {
    h.query_all_by_role(Role::Button).find(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("Node "))).map(|n| n.rect())
}
fn header(h: &Harness<'static, NodeCraftApp>, name: &str) -> Rect {
    h.get_by_role_and_label(Role::Button, &format!("Field {name}")).rect()
}
fn open_menu(h: &mut Harness<'static, NodeCraftApp>, at: Pos2) {
    press(h, at, egui::PointerButton::Secondary, true);
    press(h, at, egui::PointerButton::Secondary, false);
}
/// Selects the fixture card, then presses F over empty canvas.
fn field_around_card(h: &mut Harness<'static, NodeCraftApp>) -> FieldId {
    let id = member(h);
    h.state_mut().selection.only(id);
    h.hover_at(Pos2::new(20.0, 680.0));
    h.key_press(egui::Key::F);
    h.run();
    only_field(h)
}

#[test]
fn f_makes_an_empty_field_at_the_pointer_in_one_undo_step() {
    let mut h = harness();
    h.run();
    let before = h.state().session.document().clone();
    h.hover_at(Pos2::new(40.0, 60.0));
    h.key_press(egui::Key::F);
    h.run();
    let id = only_field(&h);
    let field = canvas(&h).fields().get(&id).unwrap().clone();
    let at = h.state().view.to_world(h.state().viewport.min, Pos2::new(40.0, 60.0));
    assert_eq!((field.rect.pos.x, field.rect.pos.y), (at.x, at.y));
    assert_eq!((field.rect.width, field.rect.height), (320.0, 240.0));
    assert_eq!(h.state().fields.selected, Some(id));
    assert!(canvas(&h).nodes().values().all(|n| n.field.is_none()), "an empty field takes no members");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn f_groups_the_selection_and_the_zone_encloses_it() {
    let mut h = harness();
    h.run();
    let before = h.state().session.document().clone();
    let id = field_around_card(&mut h);
    let node = member(&h);
    assert_eq!(canvas(&h).nodes().get(&node).unwrap().field, Some(id));
    assert!(h.state().selection.is_empty(), "the new field is selected instead");
    let rect = canvas(&h).fields().get(&id).unwrap().rect;
    let zone = Rect::from_min_size(Pos2::new(rect.pos.x, rect.pos.y), Vec2::new(rect.width, rect.height));
    assert!(zone.contains_rect(*h.state().navigation.geometry.rects.get(&node).unwrap()));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
}

#[test]
fn f_yields_to_a_focused_text_field() {
    let mut text = String::new();
    let mut h = Harness::builder().with_size(Vec2::new(1000.0, 700.0)).build_ui_state(
        move |ui, app: &mut NodeCraftApp| {
            ui.text_edit_singleline(&mut text).request_focus();
            app.show(ui)
        },
        NodeCraftApp::headless().unwrap(),
    );
    h.run();
    h.key_press(egui::Key::F);
    h.run();
    assert!(canvas(&h).fields().is_empty());
}

#[test]
fn header_drag_moves_the_field_and_its_members_and_escape_cancels() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    h.run();
    let node = member(&h);
    let before = h.state().session.document().clone();
    let (field_at, node_at) = (canvas(&h).fields().get(&id).unwrap().rect.pos, canvas(&h).nodes().get(&node).unwrap().pos);
    let grab = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    drag(&mut h, grab, Vec2::new(80.0, 40.0));
    let zoom = h.state().view.zoom;
    let moved = (80.0 / zoom, 40.0 / zoom);
    let field = canvas(&h).fields().get(&id).unwrap().rect.pos;
    let at = canvas(&h).nodes().get(&node).unwrap().pos;
    assert!((field.x - field_at.x - moved.0).abs() < 0.01 && (field.y - field_at.y - moved.1).abs() < 0.01);
    assert!((at.x - node_at.x - moved.0).abs() < 0.01 && (at.y - node_at.y - moved.1).abs() < 0.01, "members move with the field");
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before, "one undo step");

    let grab = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    press(&mut h, grab, egui::PointerButton::Primary, true);
    h.hover_at(grab + Vec2::new(50.0, 30.0));
    h.run();
    h.hover_at(grab + Vec2::new(90.0, 45.0));
    h.run();
    assert!(h.state().fields.drag.is_some());
    let cards = canvas(&h).nodes().len();
    h.key_press(egui::Key::T);
    h.run();
    assert_eq!(canvas(&h).nodes().len(), cards, "kind keys wait for the field drag");
    assert!(h.state().fields.drag.is_some(), "and the drag goes on");
    h.key_press(egui::Key::Escape);
    h.run();
    press(&mut h, grab + Vec2::new(90.0, 45.0), egui::PointerButton::Primary, false);
    assert_eq!(h.state().session.document(), &before, "Escape cancels the drag");
}

#[test]
fn corner_drag_resizes_the_zone_without_moving_members() {
    let mut h = harness();
    h.run();
    // Clear of the minimap in the lower right.
    let id = FieldId::from_bytes([8; 16]);
    let (c, node) = (h.state().canvas, member(&h));
    let at = h.state().view.to_world(h.state().viewport.min, Pos2::new(40.0, 60.0));
    let field = json!({"id": id, "title": "Field", "rect": {"pos": {"x": at.x, "y": at.y}, "width": 240, "height": 160}});
    h.state_mut().session.dispatch("field.create", &json!({"canvas": c, "field": field, "nodes": [node]})).unwrap();
    h.run();
    let nodes = canvas(&h).nodes().clone();
    let width = canvas(&h).fields().get(&id).unwrap().rect.width;
    let corner = h.get_by_role_and_label(Role::Button, "Resize field Field").rect().center();
    drag(&mut h, corner, Vec2::new(60.0, 30.0));
    assert!(canvas(&h).fields().get(&id).unwrap().rect.width > width);
    assert_eq!(canvas(&h).nodes(), &nodes);
    // Shrinking stops at the smallest zone.
    let corner = h.get_by_role_and_label(Role::Button, "Resize field Field").rect().center();
    drag(&mut h, corner, Vec2::new(-2000.0, -2000.0));
    let rect = canvas(&h).fields().get(&id).unwrap().rect;
    let t = h.state().theme.tokens();
    assert_eq!((rect.width, rect.height), (t.field_min_width, t.field_min_height));
}

#[test]
fn collapse_from_the_header_menu_hides_members_until_expanded() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    h.run();
    let node = member(&h);
    assert!(card(&h).is_some());
    let at = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    open_menu(&mut h, at);
    h.get_by_role_and_label(Role::Button, "Collapse field").click();
    h.run();
    h.run();
    assert!(canvas(&h).fields().get(&id).unwrap().collapsed);
    assert!(card(&h).is_none(), "a collapsed field hides its cards");
    h.hover_at(Pos2::new(20.0, 680.0));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run();
    assert!(!h.state().selection.contains(node), "select all skips hidden cards");
    // Double-clicking the header expands it again. Wait out the menu's right-click first, or
    // egui counts a triple click.
    let at = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    h.hover_at(at);
    for _ in 0..30 {
        h.step();
    }
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
    }
    h.run();
    assert!(!canvas(&h).fields().get(&id).unwrap().collapsed);
    assert!(card(&h).is_some());
}

#[test]
fn card_menu_moves_nodes_into_a_field_and_out_again() {
    let mut h = harness();
    h.run();
    let id = FieldId::from_bytes([8; 16]);
    let c = h.state().canvas;
    h.state_mut()
        .session
        .dispatch(
            "field.create",
            &json!({"canvas": c, "field": {"id": id, "title": "Shots", "rect": {"pos": {"x": 2000, "y": 2000}, "width": 320, "height": 240}}}),
        )
        .unwrap();
    h.run();
    let node = member(&h);
    let at = card(&h).unwrap().left_top() + Vec2::new(30.0, 12.0);
    open_menu(&mut h, at);
    h.get_by_role_and_label(Role::Button, "Field ⏵").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Move into Shots").click();
    h.run();
    assert_eq!(canvas(&h).nodes().get(&node).unwrap().field, Some(id));
    let at = card(&h).unwrap().left_top() + Vec2::new(30.0, 12.0);
    open_menu(&mut h, at);
    h.get_by_role_and_label(Role::Button, "Field ⏵").click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Remove from field").click();
    h.run();
    assert_eq!(canvas(&h).nodes().get(&node).unwrap().field, None);
    assert!(canvas(&h).fields().contains_key(&id), "the zone stays, empty");
}

#[test]
fn rename_saves_through_the_dialog_and_oversized_titles_keep_the_draft() {
    let mut h = harness();
    h.run();
    h.hover_at(Pos2::new(40.0, 60.0));
    h.key_press(egui::Key::F);
    h.run();
    let id = only_field(&h);
    let at = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    open_menu(&mut h, at);
    h.get_by_role_and_label(Role::Button, "Rename field…").click();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Field name").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text("Hero shots".into()));
    h.run();
    h.get_by_role_and_label(Role::Button, "Save field name").click();
    h.run();
    assert_eq!(canvas(&h).fields().get(&id).unwrap().title, "Hero shots");
    assert!(h.state().fields.rename.is_none());

    let before = h.state().session.document().clone();
    let draft = "界".repeat(1366);
    h.state_mut().fields.rename = Some((id, draft.clone()));
    h.run();
    h.get_by_role_and_label(Role::Button, "Save field name").click();
    h.run();
    assert_eq!(h.state().session.document(), &before);
    assert_eq!(h.state().fields.rename.as_ref().map(|(_, title)| title.as_str()), Some(draft.as_str()));
    assert!(h.state().notice.as_deref().unwrap_or_default().contains("4096"));
}

#[test]
fn undoing_a_field_mid_drag_drops_the_drag() {
    let mut h = harness();
    h.run();
    field_around_card(&mut h);
    h.run();
    let grab = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    press(&mut h, grab, egui::PointerButton::Primary, true);
    h.hover_at(grab + Vec2::new(40.0, 20.0));
    h.run();
    h.hover_at(grab + Vec2::new(60.0, 30.0));
    h.run();
    assert!(h.state().fields.drag.is_some());
    h.state_mut().session.dispatch("edit.undo", &json!({})).unwrap();
    h.run();
    press(&mut h, grab + Vec2::new(60.0, 30.0), egui::PointerButton::Primary, false);
    assert!(canvas(&h).fields().is_empty());
    assert!(h.state().fields.drag.is_none());
}

#[test]
fn a_wire_inside_a_collapsed_field_is_hidden_and_stand_ins_follow_a_drag() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    let node = member(&h);
    let t = h.state().theme.tokens();
    let mut hidden = std::collections::BTreeSet::new();
    let outside = NodeId::from_bytes([7; 16]);
    assert!(!fields::inside_collapsed(canvas(&h), &hidden, node, node), "an open field hides nothing");
    hidden.insert(node);
    assert!(fields::inside_collapsed(canvas(&h), &hidden, node, node));
    assert!(!fields::inside_collapsed(canvas(&h), &hidden, node, outside), "a wire out of the field still shows");
    let still: Vec<_> = fields::stand_ins(canvas(&h), &hidden, t, 1.0, None).collect();
    let moved: Vec<_> = fields::stand_ins(canvas(&h), &hidden, t, 1.0, Some((id, Vec2::new(30.0, 10.0)))).collect();
    assert_eq!(moved[0].1, still[0].1.translate(Vec2::new(30.0, 10.0)));
}

#[test]
fn delete_removes_a_selected_field_keeping_its_cards_in_one_undo_step() {
    let mut h = harness();
    h.run();
    let before = h.state().session.document().clone();
    let id = field_around_card(&mut h);
    let node = member(&h);
    let grouped = h.state().session.document().clone();
    let title = canvas(&h).fields().get(&id).unwrap().title.clone();
    let at = header(&h, &title).center();
    press(&mut h, at, egui::PointerButton::Primary, true);
    press(&mut h, at, egui::PointerButton::Primary, false);
    assert_eq!(h.state().fields.selected, Some(id));
    assert!(h.state().selection.is_empty());
    h.key_press(egui::Key::Delete);
    h.run();
    assert!(canvas(&h).fields().is_empty());
    assert_eq!(canvas(&h).nodes().get(&node).map(|n| n.field), Some(None), "the card stays, out of the field");
    assert_eq!(h.state().fields.selected, None);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &grouped);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &before);
    // Backspace deletes a selected field too.
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
    h.run();
    h.state_mut().fields.selected = Some(id);
    h.key_press(egui::Key::Backspace);
    h.run();
    assert!(canvas(&h).fields().is_empty());
}

#[test]
fn f_fits_the_zone_around_cards_added_this_frame() {
    let mut h = harness();
    h.run();
    let first = member(&h);
    let mut added = canvas(&h).nodes().get(&first).unwrap().clone();
    added.id = NodeId::from_bytes([71; 16]);
    added.pos = nodecraft_graph::Position { x: added.pos.x + 900.0, y: added.pos.y + 500.0 };
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("node.add", &json!({"canvas": canvas_id, "node": added})).unwrap();
    // No frame in between: the new card has no geometry yet when F runs.
    assert!(!h.state().navigation.geometry.rects.contains_key(&added.id));
    h.state_mut().selection.replace([first, added.id].into_iter().collect());
    h.state_mut().quick_field(None);
    h.run();
    let id = only_field(&h);
    let rect = canvas(&h).fields().get(&id).unwrap().rect;
    let zone = Rect::from_min_size(Pos2::new(rect.pos.x, rect.pos.y), Vec2::new(rect.width, rect.height));
    for node in [first, added.id] {
        assert_eq!(canvas(&h).nodes().get(&node).unwrap().field, Some(id));
        assert!(zone.contains_rect(*h.state().navigation.geometry.rects.get(&node).unwrap()), "every member's card is inside the zone");
    }
}

#[test]
fn shift_drag_from_a_header_draws_a_marquee_instead_of_moving_the_field() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    h.run();
    let node = member(&h);
    let before = h.state().session.document().clone();
    let grab = header(&h, "Field").left_center() + Vec2::new(60.0, 0.0);
    let to = card(&h).unwrap().right_bottom() + Vec2::new(20.0, 20.0);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    h.hover_at(grab);
    h.run();
    h.event(egui::Event::PointerButton { pos: grab, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
    h.run();
    h.hover_at(grab + (to - grab) * 0.5);
    h.run();
    h.hover_at(to);
    h.run();
    assert!(h.state().marquee.is_some(), "shift-drag on a header starts a marquee");
    assert!(h.state().fields.drag.is_none(), "and doesn't move the field");
    h.event(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::SHIFT });
    h.run();
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.run();
    assert_eq!(h.state().session.document(), &before, "the field stays where it was");
    assert!(h.state().selection.contains(node), "the marquee selected the card it covered");
    assert_eq!(canvas(&h).fields().get(&id).unwrap().rect, before.canvases().get(&h.state().canvas).unwrap().fields().get(&id).unwrap().rect);
}

#[test]
fn membership_is_worked_out_once_per_revision() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    h.run();
    let node = member(&h);
    let first = h.state_mut().membership();
    assert_eq!(first.counts.get(&id), Some(&1));
    assert!(first.hidden.is_empty());
    h.run();
    assert!(Arc::ptr_eq(&first, &h.state_mut().membership()), "an unchanged document reuses it");
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("field.update", &json!({"canvas": canvas_id, "field": id, "collapsed": true})).unwrap();
    let collapsed = h.state_mut().membership();
    assert!(collapsed.hidden.contains(&node), "an edit recomputes it");
}

/// Collapses a field around the fixture card, so the card is hidden.
fn collapsed_around_card(h: &mut Harness<'static, NodeCraftApp>) -> FieldId {
    let id = field_around_card(h);
    let canvas = h.state().canvas;
    h.state_mut().session.dispatch("field.update", &json!({"canvas": canvas, "field": id, "collapsed": true})).unwrap();
    h.state_mut().selection.clear();
    h.run();
    assert!(card(h).is_none(), "a collapsed field hides its card");
    id
}

#[test]
fn search_next_expands_a_collapsed_field_to_show_its_match_in_one_undo_step() {
    let mut h = harness();
    h.run();
    let node = member(&h);
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("node.title", &json!({"canvas": canvas_id, "id": node, "title": "Needle"})).unwrap();
    let id = collapsed_around_card(&mut h);
    let collapsed = h.state().session.document().clone();
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_search(&ctx, "canvas.search_open", &json!({})).unwrap();
    h.state_mut().dispatch_search(&ctx, "canvas.search_query", &json!({"query": "needle"})).unwrap();
    h.state_mut().dispatch_search(&ctx, "canvas.search_next", &json!({})).unwrap();
    h.run();
    assert!(
        !canvas(&h).fields().get(&id).unwrap().collapsed,
        "query {:?} results {:?} current {:?} notice {:?}",
        h.state().search.query,
        h.state().search.results,
        h.state().search.current,
        h.state().notice
    );
    assert!(h.state().selection.contains(node), "the match is focused");
    let drawn = card(&h).expect("the match is drawn");
    assert!(h.state().viewport.contains_rect(drawn), "the match is framed in view");
    h.key_press(egui::Key::Escape);
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert_eq!(h.state().session.document(), &collapsed, "expanding is one undo step");
}

#[test]
fn tag_navigation_expands_a_collapsed_field_to_show_its_card() {
    let mut h = harness();
    h.run();
    let id = collapsed_around_card(&mut h);
    let node = member(&h);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_open", &json!({})).unwrap();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_set", &json!({"tags": [], "untagged": true})).unwrap();
    h.state_mut().dispatch_tag_filter(&ctx, "canvas.tag_filter_next", &json!({})).unwrap();
    h.run();
    assert!(!canvas(&h).fields().get(&id).unwrap().collapsed);
    assert!(h.state().selection.contains(node), "notice {:?}", h.state().notice);
    assert!(card(&h).is_some(), "the card is drawn");
}

#[test]
fn the_minimap_shows_a_collapsed_field_as_its_header_without_its_cards() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    let node = member(&h);
    let shapes = |h: &mut Harness<'static, NodeCraftApp>| {
        let (membership, collapsed) = {
            let zoom = h.state().view.zoom;
            h.state_mut().collapsed_fields(zoom)
        };
        h.state().navigation.shapes(None, &membership.hidden, &collapsed)
    };
    let open = shapes(&mut h);
    assert!(open.iter().any(|(shown, _)| *shown == Some(node)), "an open field's card is drawn");
    assert!(open.iter().all(|(shown, _)| shown.is_some()), "an open field adds no header");
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("field.update", &json!({"canvas": canvas_id, "field": id, "collapsed": true})).unwrap();
    h.run();
    let closed = shapes(&mut h);
    assert!(closed.iter().all(|(shown, _)| *shown != Some(node)), "a hidden card is not drawn");
    let field = canvas(&h).fields().get(&id).unwrap().clone();
    let header = super::fields::world_rect(&field, h.state().theme.tokens(), h.state().view.zoom);
    assert_eq!(closed.iter().filter(|(shown, _)| shown.is_none()).map(|(_, rect)| *rect).collect::<Vec<_>>(), vec![header]);
    assert!(header.height() < field.rect.height, "only the header, not the whole zone");
}

#[test]
fn fit_all_frames_a_collapsed_field_by_its_header_not_its_hidden_cards() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    h.run();
    let node = member(&h);
    let card = *h.state().navigation.geometry.rects.get(&node).unwrap();
    // A tall zone whose header is far above its only card.
    let canvas_id = h.state().canvas;
    let rect = json!({"pos": {"x": card.min.x - 40.0, "y": card.min.y - 3000.0}, "width": card.width() + 80.0, "height": 3000.0 + card.height() + 40.0});
    h.state_mut().session.dispatch("field.update", &json!({"canvas": canvas_id, "field": id, "rect": rect, "collapsed": true})).unwrap();
    h.run();
    let field = canvas(&h).fields().get(&id).unwrap().clone();
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.run();
    let (view, viewport) = (h.state().view, h.state().viewport);
    let header = view.rect_to_screen(viewport.min, super::fields::world_rect(&field, h.state().theme.tokens(), view.zoom));
    assert!(viewport.contains_rect(header), "Fit all frames the collapsed header");
    assert!(!viewport.intersects(view.rect_to_screen(viewport.min, card)), "and not the card hidden far below it");
}

/// Put a selected card far below its field header and collapse the field. The collapse drops
/// the hidden card from the selection; its cached geometry stays available though the canvas no
/// longer paints it.
fn selected_card_in_distant_collapsed_field(h: &mut Harness<'static, NodeCraftApp>) -> (FieldId, NodeId, Rect) {
    h.run();
    let id = field_around_card(h);
    h.run();
    let node = member(h);
    let card = *h.state().navigation.geometry.rects.get(&node).unwrap();
    let canvas_id = h.state().canvas;
    let rect = json!({"pos": {"x": card.min.x - 40.0, "y": card.min.y - 3000.0}, "width": card.width() + 80.0, "height": 3000.0 + card.height() + 40.0});
    // Select background padding, not the compact card's generated text controls.
    let screen = h.state().view.rect_to_screen(h.state().viewport.min, card);
    let target = Pos2::new(screen.center().x, screen.top() + h.state().theme.tokens().card_padding * h.state().view.zoom * 0.5);
    press(h, target, egui::PointerButton::Primary, true);
    press(h, target, egui::PointerButton::Primary, false);
    assert!(h.state().selection.contains(node), "pointer selects the member card");
    h.state_mut().session.dispatch("field.update", &json!({"canvas": canvas_id, "field": id, "rect": rect, "collapsed": true})).unwrap();
    h.run();
    assert!(!h.state().selection.contains(node), "collapse drops the hidden card from the selection");
    assert!(self::card(h).is_none(), "the hidden card is not painted");
    (id, node, card)
}

fn assert_fit_frames_header_not_card(h: &Harness<'static, NodeCraftApp>, id: FieldId, card: Rect) {
    let (view, viewport) = (h.state().view, h.state().viewport);
    let field = canvas(h).fields().get(&id).unwrap();
    let header = view.rect_to_screen(viewport.min, super::fields::world_rect(field, h.state().theme.tokens(), view.zoom));
    assert!(viewport.contains_rect(header), "selected hidden card frames its collapsed field header: {header:?} outside {viewport:?}");
    assert!(!viewport.intersects(view.rect_to_screen(viewport.min, card)), "hidden card's own rect stays outside view");
}

#[test]
fn collapse_drops_a_selected_card_so_delete_cannot_reach_it() {
    let mut h = harness();
    let (_, node, _) = selected_card_in_distant_collapsed_field(&mut h);
    h.key_press(egui::Key::Delete);
    h.run();
    assert!(canvas(&h).nodes().contains_key(&node), "Delete never removes a card the user cannot see");
}

#[test]
fn fit_selection_frames_only_hidden_cards_by_their_header() {
    let mut h = harness();
    let (id, node, card) = selected_card_in_distant_collapsed_field(&mut h);
    // A test, an agent or a same-frame race can still hand Fit selected a hidden card; set it
    // after this frame's selection sync so it is in the selection when the fit runs.
    h.state_mut().selection.only(node);
    let ctx = h.ctx.clone();
    assert!(h.state_mut().navigation_enabled(&ctx, "canvas.fit_selection", &json!({})).is_ok(), "hidden-only selection keeps Fit selected enabled");
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_selection", &json!({})).unwrap();
    h.run();
    assert_fit_frames_header_not_card(&h, id, card);
}

#[test]
fn named_fit_frames_hidden_header_without_selecting_its_card() {
    let mut h = harness();
    let (id, node, card) = selected_card_in_distant_collapsed_field(&mut h);
    h.hover_at(Pos2::new(60.0, 590.0));
    h.key_press(egui::Key::T);
    h.run();
    let other = *canvas(&h).nodes().keys().find(|candidate| **candidate != node).unwrap();
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("node.move", &json!({"canvas":canvas_id,"id":other,"pos":{"x":card.min.x + 1000.0,"y":card.min.y - 2900.0}})).unwrap();
    h.run();
    let selection = h.state().selection.clone();
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_selection", &json!({"nodes":[node,other]})).unwrap();
    h.run();
    assert_fit_frames_header_not_card(&h, id, card);
    assert_eq!(h.state().selection, selection, "named framing does not select hidden cards");
}

#[test]
fn fit_selection_command_frames_visible_card_and_hidden_field_header() {
    let mut h = harness();
    let (id, hidden, card) = selected_card_in_distant_collapsed_field(&mut h);
    h.hover_at(Pos2::new(60.0, 590.0));
    h.key_press(egui::Key::T);
    h.run();
    let visible = *canvas(&h).nodes().keys().find(|node| **node != hidden).unwrap();
    let pos = canvas(&h).nodes().get(&visible).unwrap().pos;
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("node.move", &json!({"canvas": canvas_id, "id": visible, "pos": {"x": pos.x, "y": card.min.y - 2900.0}})).unwrap();
    h.run();
    let visible_rect = *h.state().navigation.geometry.rects.get(&visible).unwrap();
    h.state_mut().selection.only(visible);
    h.state_mut().selection.add(hidden);
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_selection", &json!({})).unwrap();
    h.run();
    assert_fit_frames_header_not_card(&h, id, card);
    let view = h.state().view;
    assert!(h.state().viewport.contains_rect(view.rect_to_screen(h.state().viewport.min, visible_rect)), "visible selected card is framed too");
}

#[test]
fn fit_all_from_far_out_frames_a_collapsed_header_the_same_twice() {
    let mut h = harness();
    h.run();
    let id = field_around_card(&mut h);
    h.run();
    let canvas_id = h.state().canvas;
    h.state_mut().session.dispatch("field.update", &json!({"canvas": canvas_id, "field": id, "collapsed": true})).unwrap();
    h.state_mut().view.zoom = 0.1;
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.run();
    let first = h.state().view;
    h.state_mut().dispatch_navigation(&ctx, "canvas.fit_all", &json!({})).unwrap();
    h.run();
    let second = h.state().view;
    assert!((first.zoom - second.zoom).abs() < 1e-4 && (first.pan - second.pan).length() < 0.5, "Fit all is stable: {first:?} then {second:?}");
}
