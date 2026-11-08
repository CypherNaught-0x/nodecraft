//! The notice stack in the shell (#1110): Escape belongs to a notice only while it holds the
//! focus, and an action button runs its command as the palette would.
use super::tests::{harness, node};
use crate::notices::{Notice, NoticeKind};
use egui_kittest::kittest::Queryable;

#[test]
fn escape_dismisses_a_focused_notice_and_reaches_nothing_else() {
    let mut h = harness();
    let id = node(h.state()).id;
    h.state_mut().selection.only(id);
    h.state_mut().notices.error("The provider refused the key.");
    h.run();
    h.get_by_label("Dismiss notice").focus();
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.state().notices.is_empty(), "{:?}", h.state().notice());
    assert!(h.state().selection.contains(id), "the canvas never saw the Escape");
}

#[test]
fn escape_without_focus_leaves_the_notice_to_the_canvas() {
    let mut h = harness();
    let id = node(h.state()).id;
    h.state_mut().selection.only(id);
    h.state_mut().notices.error("The provider refused the key.");
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().notice(), Some("The provider refused the key."));
    assert!(!h.state().selection.contains(id), "Escape cleared the selection as before");
}

#[test]
fn escape_goes_to_an_open_popup_before_a_notice() {
    let mut h = harness();
    h.state_mut().notices.error("Stays");
    h.run();
    h.get_by_label("Dismiss notice").focus();
    h.run();
    // Quick-add owns Escape while it is open; the notice stays.
    h.state_mut().palette = Some(crate::palette::Palette::new(egui::Pos2::ZERO, egui::pos2(200.0, 200.0)));
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    assert_eq!(h.state().notice(), Some("Stays"));
}

#[test]
fn an_action_button_runs_its_command() {
    let mut h = harness();
    let visible = h.state().navigation.visible;
    h.state_mut().notices.push_notice(Notice::new(NoticeKind::Info, "The map is hidden.").with_action("Toggle map", "canvas.minimap_toggle"));
    h.run();
    h.get_by_label("Toggle map").click();
    h.run();
    assert_eq!(h.state().navigation.visible, !visible);
}

#[test]
fn a_refused_run_survives_undo_delete_paste_and_search() {
    // The run failure isn't shown on any card: losing it to an unrelated success loses money.
    let mut h = harness();
    let id = node(h.state()).id;
    let refused = "The run didn't start: the budget for today is spent";
    h.state_mut().notices.push_notice(Notice::new(NoticeKind::Error, refused).keep());
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut().selection.only(id);
    h.state_mut().run_palette_command(&ctx, "canvas.delete");
    h.run();
    h.state_mut().run_palette_command(&ctx, "edit.undo");
    h.run();
    h.state_mut().run_palette_command(&ctx, "canvas.paste");
    h.run();
    h.state_mut().dispatch_navigation(&ctx, "canvas.search_open", &serde_json::json!({})).ok();
    h.state_mut().dispatch_search(&ctx, "canvas.search_query", &serde_json::json!({"query": "x"})).ok();
    h.run();
    assert!(h.state().notices.iter().any(|notice| notice.text == refused), "{:?}", h.state().notice());
}
