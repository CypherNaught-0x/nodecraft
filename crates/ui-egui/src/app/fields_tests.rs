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
