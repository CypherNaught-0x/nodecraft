//! #1121: the quick-add palette's sections, ranking, unavailable commands and placement.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};

fn sized(size: Vec2) -> Harness<'static, NodeCraftApp> {
    Harness::builder()
        .with_step_dt(1.0 / 60.0)
        .with_max_steps(120)
        .with_size(size)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap())
}

/// Opens the palette by double-clicking empty canvas at `at`.
fn open_at(h: &mut Harness<'static, NodeCraftApp>, at: Pos2) {
    h.hover_at(at);
    // Let earlier clicks age out, so these two make a double click rather than a triple.
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
    assert!(h.state().palette.is_some(), "double-clicking empty canvas at {at:?} opens the palette");
}

fn type_query(h: &mut Harness<'static, NodeCraftApp>, text: &str) {
    h.event(egui::Event::Text(text.into()));
    h.run();
}

fn menu_rect(h: &Harness<'static, NodeCraftApp>) -> Rect {
    egui::AreaState::load(&h.ctx, egui::Id::new("quick_add_palette")).unwrap().rect()
}

fn header_shown(h: &Harness<'static, NodeCraftApp>, title: &str) -> bool {
    let menu = menu_rect(h);
    h.query_all_by_label(title).any(|node| node.accesskit_node().role() == Role::Label && menu.contains_rect(node.rect()))
}

/// The rows on screen, top to bottom, by accessible name.
fn shown_rows(h: &Harness<'static, NodeCraftApp>, entries: &[palette::Entry]) -> Vec<String> {
    let menu = menu_rect(h);
    let mut rows: Vec<(f32, String)> = entries
        .iter()
        .map(|entry| palette::row_name(&h.ctx, entry))
        .flat_map(|name| {
            h.query_all_by_label(&name).filter(|n| n.accesskit_node().role() == Role::Button).map(|n| (n.rect().top(), name.clone())).collect::<Vec<_>>()
        })
        .filter(|(top, _)| *top >= menu.top() && *top < menu.bottom())
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.dedup();
    rows.into_iter().map(|(_, name)| name).collect()
}

#[test]
fn an_empty_query_shows_section_headers_and_recent_after_a_choice() {
    let mut h = sized(Vec2::new(1440.0, 900.0));
    open_at(&mut h, Pos2::new(200.0, 700.0));
    for title in ["Text", "Image", "Video"] {
        assert!(header_shown(&h, title), "the {title} heading is on screen");
    }
    assert!(!header_shown(&h, "Recent"), "nothing chosen yet, so no Recent section");
    // Every section is in the list, in order, even those below the fold.
    let entries = h.state().palette_entries();
    let titles: Vec<&str> =
        palette::layout(&entries, "", &[]).into_iter().filter_map(|item| if let palette::Item::Header(t) = item { Some(t) } else { None }).collect();
    assert_eq!(titles, ["Text", "Image", "Video", "Audio", "3D", "Document", "Other", "Actions", "Commands"]);
    // Each row has a one-line summary.
    assert!(entries.iter().filter(|entry| entry.available()).all(|entry| !entry.description().is_empty()), "every entry has a summary");
    // Choose Blur; the next palette lists it first under Recent.
    type_query(&mut h, "blur");
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().palette.is_none());
    // Away from the new Blur card.
    open_at(&mut h, Pos2::new(200.0, 200.0));
    assert!(header_shown(&h, "Recent"));
    let entries = h.state().palette_entries();
    assert_eq!(shown_rows(&h, &entries).first().map(String::as_str), Some("Blur"));
    assert_eq!(palette::recent(&h.ctx), [palette::Target::Kind("image.blur".into())]);
    // Recent is view state: the document never holds it.
    assert!(!serde_json::to_string(h.state().session.document()).unwrap().contains("quick_add"));
}

#[test]
fn typing_blur_ranks_blur_first_and_names_its_section() {
    let mut h = sized(Vec2::new(1440.0, 900.0));
    open_at(&mut h, Pos2::new(200.0, 700.0));
    type_query(&mut h, "blur");
    let entries = h.state().palette_entries();
    let ranked = palette::rows(&palette::layout(&entries, "blur", &[]));
    assert_eq!(ranked.first().and_then(|i| entries.get(*i)).map(|e| &e.target), Some(&palette::Target::Kind("image.blur".into())));
    assert_eq!(shown_rows(&h, &entries).first().map(String::as_str), Some("Blur"));
    // The section is named on the row's right, its summary under the label.
    let blur = entries.iter().find(|e| e.target == palette::Target::Kind("image.blur".into())).unwrap();
    let painted: Vec<String> = h
        .output()
        .shapes
        .iter()
        .filter_map(|clipped| if let egui::epaint::Shape::Text(text) = &clipped.shape { Some(text.galley.text().to_owned()) } else { None })
        .collect();
    assert!(painted.iter().any(|text| text == "Action"), "{painted:?}");
    assert!(painted.iter().any(|text| text == blur.summary.as_str()), "{painted:?}");
    h.key_press(egui::Key::Enter);
    h.run();
    let canvas = h.state().session.document().canvases().get(&h.state().canvas).unwrap();
    assert!(canvas.nodes().values().any(|node| node.kind == "image.blur"));
}

#[test]
fn an_unavailable_command_shows_its_reason_and_enter_or_a_click_does_nothing() {
    let mut h = sized(Vec2::new(1440.0, 900.0));
    assert!(!h.state().session.can_undo());
    open_at(&mut h, Pos2::new(200.0, 700.0));
    type_query(&mut h, "undo");
    let entries = h.state().palette_entries();
    let undo = entries.iter().find(|e| e.target == palette::Target::Command("edit.undo")).expect("Undo is listed though it can't run");
    assert_eq!(undo.unavailable.as_deref(), Some("Nothing to undo"));
    let name = palette::row_name(&h.ctx, undo);
    let row = h.get_by_label(&name);
    assert!(row.accesskit_node().is_disabled(), "screen readers hear it is unavailable");
    assert_eq!(row.accesskit_node().description().as_deref(), Some("Nothing to undo"));
    let painted = h.output().shapes.iter().any(|clipped| matches!(&clipped.shape, egui::epaint::Shape::Text(text) if text.galley.text() == "Nothing to undo"));
    assert!(painted, "the reason is its description");
    let document = h.state().session.document().clone();
    let revision = h.state().session.revision();
    assert_eq!(h.state().palette.as_ref().unwrap().highlighted, 0);
    h.key_press(egui::Key::Enter);
    h.run();
    assert!(h.state().palette.is_some(), "Enter on an unavailable command keeps the palette open");
    assert_eq!(h.state().notice, None);
    h.get_by_label(&name).click();
    h.run();
    assert!(h.state().palette.is_some(), "a click does nothing either");
    assert_eq!(h.state().session.document(), &document);
    assert_eq!(h.state().session.revision(), revision);
    assert!(palette::recent(&h.ctx).is_empty(), "it isn't remembered as chosen");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().palette.is_none());
}

#[test]
fn the_palette_opened_at_the_bottom_right_corner_is_fully_on_screen() {
    for size in [Vec2::new(1440.0, 900.0), Vec2::new(640.0, 400.0)] {
        let mut h = sized(size);
        let corner = Pos2::new(size.x - 4.0, size.y - 4.0);
        h.state_mut().palette = Some(Palette::new(Pos2::ZERO, corner));
        h.run();
        let screen = h.ctx.content_rect();
        let menu = menu_rect(&h);
        assert!(screen.contains_rect(menu), "{size:?}: {menu:?} inside {screen:?}");
        assert!(menu.bottom() <= corner.y && menu.right() <= corner.x, "{size:?}: it opens upward and leftward from the corner: {menu:?}");
        // Its first row is on screen and chosen by Enter.
        let entries = h.state().palette_entries();
        let first = shown_rows(&h, &entries);
        assert!(!first.is_empty(), "{size:?}: rows are on screen");
    }
}

/// Every command the palette lists, greyed or not, takes no parameters beyond the active canvas,
/// and the ones that edit a node or a noodle (which need ids) never appear.
pub(super) fn lists_no_command_needing_parameters(app: &NodeCraftApp) {
    let entries = app.palette_entries();
    for entry in &entries {
        let palette::Target::Command(id) = entry.target else { continue };
        let spec = app.session.registry().get(id).unwrap();
        let schema = (spec.input_schema)();
        let required: Vec<&str> = schema.get("required").and_then(|r| r.as_array()).map(|r| r.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
        assert!(required.iter().all(|key| *key == "canvas"), "{id} needs {required:?} but is listed ({:?})", entry.unavailable);
    }
    for id in ["node.add", "node.remove", "node.move", "edge.connect", "edge.disconnect", "canvas.delete"] {
        assert!(!entries.iter().any(|entry| entry.target == palette::Target::Command(id)), "{id} is listed");
    }
}

#[test]
fn a_view_only_share_lists_no_command_that_needs_parameters() {
    let mut h = sized(Vec2::new(1000.0, 700.0));
    h.state_mut().session.set_access(nodecraft_engine::access::Access::new(nodecraft_sync::Role::View, None).unwrap());
    h.run();
    lists_no_command_needing_parameters(h.state());
    // What it can't do now is still explained: the edits a viewer can't make are greyed.
    let entries = h.state().palette_entries();
    assert!(entries.iter().any(|entry| !entry.available()), "something is greyed for a viewer");
}

/// #1121 review: a command whose `enabled` checks the role before its parameters would otherwise
/// pass as "can't run now".
#[test]
fn the_parameter_rule_reads_the_schema_and_the_supplied_params() {
    let h = sized(Vec2::new(1000.0, 700.0));
    let registry = h.state().session.registry();
    let spec = |id: &str| *registry.get(id).unwrap();
    assert!(!palette::takes(&spec("node.add"), &json!({})));
    assert!(!palette::takes(&spec("canvas.delete"), &json!({})));
    assert!(palette::takes(&spec("canvas.stale"), &json!({"canvas": h.state().canvas})));
    assert!(!palette::takes(&spec("canvas.stale"), &json!({})));
    assert!(palette::takes(&spec("edit.undo"), &json!({})));
}

/// Shortcuts sit against the row's right edge in the sectioned list too, not only when ranked.
#[test]
fn sectioned_rows_right_align_their_shortcuts() {
    let mut h = sized(Vec2::new(1440.0, 900.0));
    open_at(&mut h, Pos2::new(200.0, 700.0));
    let menu = menu_rect(&h);
    let entries = h.state().palette_entries();
    let mut checked = 0;
    for entry in entries.iter().filter(|entry| entry.shortcut.is_some()) {
        let name = palette::row_name(&h.ctx, entry);
        let Some(row) = h.query_by_label(&name).map(|node| node.rect()).filter(|row| menu.contains_rect(*row)) else { continue };
        let key = crate::shortcut::format_shortcut(&h.ctx, entry.shortcut.as_deref().unwrap());
        let painted = |text: &str| {
            h.output().shapes.iter().find_map(|clipped| match &clipped.shape {
                egui::epaint::Shape::Text(shape) if shape.galley.text() == text && row.contains(shape.visual_bounding_rect().center()) => {
                    Some(shape.visual_bounding_rect())
                }
                _ => None,
            })
        };
        let label = painted(&entry.label).expect("the label is painted in its row");
        let chip = painted(&key).expect("the shortcut is painted in its row");
        assert!(chip.left() > label.right() + 20.0, "{name}: a gap separates the shortcut from the label: {label:?} {chip:?}");
        assert!(row.right() - chip.right() < 16.0, "{name}: the shortcut sits against the row's right edge: {row:?} {chip:?}");
        checked += 1;
    }
    assert!(checked >= 3, "several on-screen rows have shortcuts, checked {checked}");
}
