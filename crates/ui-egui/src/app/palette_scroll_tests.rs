//! #964: wheel input belongs to the open menu, and every row can be reached.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};

fn open_palette() -> Harness<'static, NodeCraftApp> {
    let mut h = super::tests::harness();
    let pos = Pos2::new(150.0, 600.0);
    h.hover_at(pos);
    h.step();
    for _ in 0..2 {
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
        }
    }
    h.run();
    assert!(h.state().palette.is_some());
    h
}

fn menu_rect(h: &Harness<'static, NodeCraftApp>, id: &str) -> Rect {
    egui::AreaState::load(&h.ctx, egui::Id::new(id)).unwrap().rect()
}

fn wheel(h: &mut Harness<'static, NodeCraftApp>, pos: Pos2, modifiers: egui::Modifiers) {
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, phase: egui::TouchPhase::Move, delta: Vec2::new(0.0, -300.0), modifiers });
    h.run();
}

/// A row's accessible name as the harness spells it (kittest reports Linux on every host).
fn label(h: &Harness<'static, NodeCraftApp>, entry: &palette::Entry) -> String {
    palette::row_name(&h.ctx, entry)
}

#[test]
fn wheel_reaches_a_later_palette_row_without_moving_canvas_and_click_selects_it() {
    let mut h = open_palette();
    let entry = h.state().palette_entries().get(16).unwrap().clone();
    let palette::Target::Kind(kind) = &entry.target else { panic!("fixture must offer a kind") };
    assert!(h.query_by_label(&label(&h, &entry)).is_none(), "later row starts outside the viewport");
    let view = h.state().view;
    let document = h.state().session.document().clone();
    let pos = menu_rect(&h, "quick_add_palette").center();
    wheel(&mut h, pos, egui::Modifiers::NONE);
    assert!(h.state().palette.is_some(), "wheel must scroll the palette instead of closing it");
    assert_eq!(h.state().view, view);
    assert_eq!(h.state().session.document(), &document);
    h.get_by_label(&label(&h, &entry)).click();
    h.run();
    assert!(h.state().palette.is_none());
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert!(canvas.nodes().values().any(|node| &node.kind == kind));
    assert_eq!(h.state().session.revision(), 1);
}

#[test]
fn keyboard_reaches_later_rows_and_filtering_resets_the_scroll() {
    let mut h = open_palette();
    let entry = h.state().palette_entries().get(17).unwrap().clone();
    for _ in 0..17 {
        h.key_press(egui::Key::ArrowDown);
        h.run();
    }
    assert_eq!(h.state().palette.as_ref().unwrap().highlighted, 17);
    let row = h.get_by_label(&label(&h, &entry)).rect();
    assert!(menu_rect(&h, "quick_add_palette").contains_rect(row), "keyboard highlight is visible");
    h.event(egui::Event::Text("Audio".into()));
    h.run();
    assert_eq!(h.state().palette.as_ref().unwrap().highlighted, 0);
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().palette.is_none());
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert!(canvas.nodes().values().any(|node| node.kind == "source.audio"));
}

#[test]
fn scrolling_outside_or_with_zoom_modifier_keeps_palette_open_but_outside_click_closes() {
    let mut h = open_palette();
    let view = h.state().view;
    let outside = Pos2::new(900.0, 80.0);
    for (pos, modifiers) in [(outside, egui::Modifiers::NONE), (menu_rect(&h, "quick_add_palette").center(), egui::Modifiers::COMMAND)] {
        wheel(&mut h, pos, modifiers);
        assert!(h.state().palette.is_some());
        assert_eq!(h.state().view, view);
    }
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.hover_at(outside);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: outside, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.run();
    }
    assert!(h.state().palette.is_none());
    assert_eq!(h.state().session.revision(), 0);
}

fn open_turn_into() -> Harness<'static, NodeCraftApp> {
    let mut h = super::tests::harness();
    install_turn_into(&mut h);
    h
}

fn install_turn_into(h: &mut Harness<'static, NodeCraftApp>) {
    let node = h.state().session.document().canvases().get(&h.state().canvas).unwrap().nodes().values().next().unwrap().id;
    let mut menu = TurnInto::new(
        h.state().session.kinds(),
        Endpoint { node, port: "text".into() },
        nodecraft_types::PortType { modality: nodecraft_types::Modality::Text, many: false },
        Pos2::ZERO,
        Pos2::new(40.0, 80.0),
    );
    // Exercise a registry with more compatible entries than fit, independently of built-in count.
    menu.entries = (0..30).map(|i| turn_into::Entry { kind: nodecraft_nodes::router::ID.into(), label: format!("Choice {i:02}"), key: None }).collect();
    h.state_mut().turn_into = Some(menu);
    h.run();
}

#[test]
fn reopening_each_menu_resets_its_scroll_to_the_highlighted_first_row() {
    for palette in [true, false] {
        let mut h = if palette { open_palette() } else { open_turn_into() };
        let area = if palette { "quick_add_palette" } else { "turn_into_menu" };
        let first_label = if palette { label(&h, h.state().palette_entries().first().unwrap()) } else { "Turn into Choice 00".into() };
        let pos = menu_rect(&h, area).center();
        wheel(&mut h, pos, egui::Modifiers::NONE);
        assert!(h.query_by_label(&first_label).is_none());
        h.hover_at(Pos2::new(900.0, 80.0));
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().palette.is_none() && h.state().turn_into.is_none());
        if palette {
            h.state_mut().palette = Some(Palette::new(Pos2::ZERO, Pos2::new(150.0, 600.0)));
            h.run();
            assert_eq!(h.state().palette.as_ref().unwrap().highlighted, 0);
        } else {
            install_turn_into(&mut h);
            assert_eq!(h.state().turn_into.as_ref().unwrap().highlighted, 0);
        }
        assert!(menu_rect(&h, area).contains_rect(h.get_by_label(&first_label).rect()), "reopened {area} shows the initial selection");
    }
}

#[test]
fn turn_into_scrolls_and_keyboard_highlight_stays_visible() {
    let mut h = open_turn_into();
    let view = h.state().view;
    let pos = menu_rect(&h, "turn_into_menu").center();
    wheel(&mut h, pos, egui::Modifiers::NONE);
    assert!(h.state().turn_into.is_some(), "wheel must not dismiss Turn into");
    assert_eq!(h.state().view, view);
    h.get_by_label("Turn into Choice 16");
    // Pointer movement during scrolling may highlight a row; start from the first explicitly.
    h.state_mut().turn_into.as_mut().unwrap().highlighted = 0;
    for _ in 0..25 {
        h.key_press(egui::Key::ArrowDown);
        h.run();
    }
    assert_eq!(h.state().turn_into.as_ref().unwrap().highlighted, 25);
    assert!(menu_rect(&h, "turn_into_menu").contains_rect(h.get_by_label("Turn into Choice 25").rect()));
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().turn_into.is_none());
    assert_eq!(h.state().session.revision(), 0);
}

#[test]
#[ignore = "manual local screenshot capture; requires NODECRAFT_PALETTE964_RENDER_ROOT"]
fn render_palette_scroll_evidence() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_PALETTE964_RENDER_ROOT").unwrap());
    let mut h = open_palette();
    h.render().unwrap().save(root.join("palette-open.png")).unwrap();
    let pos = menu_rect(&h, "quick_add_palette").center();
    wheel(&mut h, pos, egui::Modifiers::NONE);
    h.render().unwrap().save(root.join("palette-scrolled.png")).unwrap();
    let mut h = open_turn_into();
    h.render().unwrap().save(root.join("turn-into-open.png")).unwrap();
    let pos = menu_rect(&h, "turn_into_menu").center();
    wheel(&mut h, pos, egui::Modifiers::NONE);
    h.render().unwrap().save(root.join("turn-into-scrolled.png")).unwrap();
}

/// #1117: a row's shortcut is spelled for this platform and painted right-aligned, apart from
/// its label, rather than a few spaces after it.
#[test]
fn palette_shortcuts_are_platform_spelled_and_right_aligned() {
    let h = open_palette();
    let entry = h.state().palette_entries().into_iter().find(|entry| entry.shortcut.as_deref() == Some("A")).expect("a kind with a one-key shortcut");
    let name = palette::row_name(&h.ctx, &entry);
    assert_eq!(name, format!("{} (A)", entry.label));
    let row = h.get_by_label(&name).rect();
    let galleys = |text: &str| -> Vec<egui::Rect> {
        h.output()
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::epaint::Shape::Text(shape) if shape.galley.text() == text => Some(shape.visual_bounding_rect()),
                _ => None,
            })
            .filter(|rect| rect.center().y > row.top() && rect.center().y < row.bottom())
            .collect()
    };
    let label = galleys(&entry.label).into_iter().next().expect("the label is painted in its row");
    let key = galleys("A").into_iter().next().expect("the shortcut is painted in its row");
    assert!(key.left() > label.right() + 20.0, "a gap separates the shortcut from the label: {label:?} {key:?}");
    assert!(row.right() - key.right() < 16.0, "the shortcut sits against the row's right edge: {row:?} {key:?}");
    // The engine's raw "Command" never reaches the screen: the palette's own entries are spelled.
    let spelled: Vec<String> =
        h.state().palette_entries().iter().filter_map(|entry| entry.shortcut.as_deref()).map(|key| crate::shortcut::format_shortcut(&h.ctx, key)).collect();
    assert!(!spelled.is_empty(), "some entries have shortcuts");
    assert!(spelled.iter().all(|key| !key.contains("Command")), "{spelled:?}");
}
