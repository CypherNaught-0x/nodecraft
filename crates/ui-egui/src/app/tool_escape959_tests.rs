//! The shell owns Escape arbitration; a tool receives its ordinary close lifecycle only after
//! every earlier owner declines. These exercise actual application frames and key events.
use super::*;
use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable};

type Toggle = fn(&mut NodeCraftApp, bool);
const SIMPLE_TOOLS: &[(&str, Toggle, fn(&NodeCraftApp) -> bool)] = &[
    ("Sheets", |app, open| app.sheet.open = open, |app| app.sheet.open),
    ("Techniques", |app, open| app.library.open = open, |app| app.library.open),
    ("Elements", |app, open| app.element_library.open = open, |app| app.element_library.open),
    ("Budgets", |app, open| app.budgets.open = open, |app| app.budgets.open),
    ("API keys", |app, open| app.keys.open = open, |app| app.keys.open),
    ("Plugins", |app, open| app.plugin_panel.open = open, |app| app.plugin_panel.open),
    ("ComfyUI servers", |app, open| app.comfy_server.open = open, |app| app.comfy_server.open),
];

fn leave_text(h: &mut Harness<'static, NodeCraftApp>) {
    if let Some(id) = h.ctx.memory(|m| m.focused()) {
        h.ctx.memory_mut(|m| m.surrender_focus(id));
    }
    h.run();
}

#[test]
fn every_simple_tool_closes_with_escape_without_changing_document_selection_or_undo() {
    for (title, set_open, is_open) in SIMPLE_TOOLS {
        let mut h = super::tests::harness();
        h.run();
        let node = super::tests::node(h.state()).id;
        h.state_mut().selection.only(node);
        set_open(h.state_mut(), true);
        h.run_steps(12);
        leave_text(&mut h);
        assert!(h.query_by_role_and_label(Role::Window, title).is_some(), "{title} was drawn");
        h.get_by_role_and_label(Role::Button, "Close window");
        let document = h.state().session.document().clone();
        let revision = h.state().session.revision();
        let undo = (h.state().session.can_undo(), h.state().session.can_redo());
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!is_open(h.state()), "Escape closes {title}");
        assert_eq!(h.state().session.document(), &document, "closing {title} changes no document data");
        assert_eq!(h.state().session.revision(), revision, "closing {title} creates no revision");
        assert_eq!((h.state().session.can_undo(), h.state().session.can_redo()), undo, "closing {title} leaves undo/redo alone");
        assert!(h.state().selection.contains(node), "the canvas never receives {title}'s Escape");
    }
}

#[test]
fn escape_closes_only_the_frontmost_tool_regardless_of_render_order() {
    for front in ["nodecraft.keys", "nodecraft.budgets"] {
        let mut h = super::tests::harness();
        h.state_mut().keys.open = true;
        h.state_mut().budgets.open = true;
        h.run_steps(12);
        leave_text(&mut h);
        h.ctx.move_to_top(crate::floating::layer(egui::Id::new(front)));
        h.run();
        h.key_press(egui::Key::Escape);
        h.run();
        assert_eq!(h.state().keys.open, front != "nodecraft.keys");
        assert_eq!(h.state().budgets.open, front != "nodecraft.budgets");
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(!h.state().keys.open && !h.state().budgets.open);
    }
}

#[test]
fn a_text_editor_gets_the_first_escape_and_the_tool_gets_the_next() {
    let mut h = super::tests::harness();
    h.state_mut().sheet.open = true;
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Title").focus();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().sheet.open, "Escape leaves the editor before closing the tool");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().sheet.open);
}

#[test]
fn focused_notices_keep_their_escape_priority_over_tools() {
    let mut h = super::tests::harness();
    h.state_mut().keys.open = true;
    h.state_mut().notices.error("A public example failure");
    h.run();
    h.get_by_label("Dismiss notice").focus();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().notices.is_empty());
    assert!(h.state().keys.open, "the focused notice consumes this press");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(!h.state().keys.open);
}

#[test]
fn other_escape_owners_keep_a_generic_tool_open() {
    for owner in ["palette", "search", "discussions", "popup", "box select"] {
        let mut h = super::tests::harness();
        h.state_mut().keys.open = true;
        h.run();
        match owner {
            "palette" => h.state_mut().palette = Some(Palette::new(Pos2::ZERO, Pos2::new(150.0, 600.0))),
            "search" => h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::F),
            "discussions" => h.state_mut().comments.open = true,
            "popup" => {
                super::topbar_tests::reveal(&mut h, "API keys");
                h.get_by_role_and_label(Role::Button, "More").click();
            }
            _ => {
                let from = Pos2::new(150.0, 600.0);
                h.hover_at(from);
                h.event(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::SHIFT });
                h.run();
                h.hover_at(from + Vec2::new(60.0, 40.0));
                h.run();
                assert!(h.state().marquee.is_some(), "a real canvas drag is active");
            }
        }
        h.run();
        if owner == "popup" {
            assert!(egui::Popup::is_any_open(&h.ctx));
        }
        h.key_press(egui::Key::Escape);
        h.run();
        assert!(h.state().keys.open, "{owner} owns the press before API keys");
    }
}
