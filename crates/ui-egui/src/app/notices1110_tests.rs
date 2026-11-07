//! Typed, stacked, dismissible and announced notices (#1110).
use super::notices::{DISMISS, NoticeKind, TIMEOUT_SECS, VISIBLE};
use super::*;
use egui::accesskit::{Live, Role};
use egui_kittest::kittest::{NodeT, Queryable};

const ERROR: &str = "The provider refused the request: invalid size.";
const SUCCESS: &str = "Saved 2 results to the folder.";

/// Steps past a timed notice's whole timeout, with no pointer over the notices.
fn past_timeout() -> usize {
    (TIMEOUT_SECS * 60.0) as usize + 60
}

#[test]
fn an_error_and_a_success_render_with_different_icons_and_edges() {
    let mut h = super::tests::harness();
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.state_mut().notify(NoticeKind::Success, SUCCESS);
    h.run();
    let error = h.get_by_label(ERROR).rect();
    let success = h.get_by_label(SUCCESS).rect();
    // Each kind has its own icon, beside its own message.
    let error_icon = h.get_by_role_and_label(Role::Image, "Error").rect();
    let success_icon = h.get_by_role_and_label(Role::Image, "Success").rect();
    assert!(error_icon.right() <= error.left() && error_icon.top() < error.bottom() && error_icon.bottom() > error.top());
    assert!(success_icon.right() <= success.left() && success_icon.top() < success.bottom() && success_icon.bottom() > success.top());
    // And its own edge colour, from the theme.
    let t = h.state().theme.tokens();
    assert_eq!(NoticeKind::Error.color(t), t.danger);
    assert_eq!(NoticeKind::Success.color(t), t.accent);
    assert_ne!(NoticeKind::Error.color(t), NoticeKind::Success.color(t));
    assert_ne!(NoticeKind::Error.icon(), NoticeKind::Success.icon());
    // Both stay in the stack; the slot holds the latest.
    assert_eq!(h.state().notice.as_deref(), Some(SUCCESS));
    assert!(success.bottom() <= error.top(), "the newest is on top");
}

#[test]
fn three_notices_stack_newest_on_top() {
    let mut h = super::tests::harness();
    let texts = ["First notice", "Second notice", "Third notice", "Fourth notice"];
    for (kind, text) in [NoticeKind::Info, NoticeKind::Success, NoticeKind::Warning, NoticeKind::Error].into_iter().zip(texts) {
        h.state_mut().notify(kind, text);
    }
    h.run();
    assert!(h.query_by_label(texts[0]).is_none(), "only {VISIBLE} show");
    let tops: Vec<f32> = texts[1..].iter().map(|text| h.get_by_label(text).rect().top()).collect();
    assert!(tops[2] < tops[1] && tops[1] < tops[0], "newest on top: {tops:?}");
    assert_eq!(h.query_all_by_role_and_label(Role::Button, DISMISS).count(), VISIBLE);
    // Dismissing one brings the older one back.
    h.get_all_by_role_and_label(Role::Button, DISMISS).next().unwrap().click();
    h.run();
    assert!(h.query_by_label(texts[3]).is_none());
    h.get_by_label(texts[0]);
}

#[test]
fn the_close_button_and_escape_dismiss() {
    let mut h = super::tests::harness();
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.run();
    h.get_by_role_and_label(Role::Button, DISMISS).click();
    h.run();
    assert!(h.query_by_label(ERROR).is_none());
    assert!(h.state().notice.is_none(), "dismissing the latest message clears it");

    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.run();
    h.get_by_role_and_label(Role::Button, DISMISS).focus();
    h.run();
    assert!(h.get_by_role_and_label(Role::Button, DISMISS).accesskit_node().is_focused());
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label(ERROR).is_none());
    assert!(h.query_by_role_and_label(Role::Button, DISMISS).is_none());
}

#[test]
fn escape_without_focus_keeps_the_notice() {
    let mut h = super::tests::harness();
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.run();
    h.key_press(egui::Key::Escape);
    h.run();
    h.get_by_label(ERROR);
}

#[test]
fn an_error_stays_after_its_timeout_and_a_success_leaves() {
    let mut h = super::tests::harness();
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.state_mut().notify(NoticeKind::Success, SUCCESS);
    h.run();
    h.run_steps(past_timeout());
    h.run();
    h.get_by_label(ERROR);
    assert!(h.query_by_label(SUCCESS).is_none(), "information and successes time out");
}

#[test]
fn hovering_a_notice_pauses_its_timeout() {
    let mut h = super::tests::harness();
    h.state_mut().notify(NoticeKind::Success, SUCCESS);
    h.run();
    h.hover_at(h.get_by_label(SUCCESS).rect().center());
    h.run_steps(past_timeout());
    h.get_by_label(SUCCESS);
    h.hover_at(Pos2::new(900.0, 100.0));
    h.run_steps(past_timeout());
    h.run();
    assert!(h.query_by_label(SUCCESS).is_none());
}

#[test]
fn the_live_region_is_assertive_for_errors_and_polite_otherwise() {
    let mut h = super::tests::harness();
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.state_mut().notify(NoticeKind::Success, SUCCESS);
    h.state_mut().notify(NoticeKind::Warning, "Recent projects could not be saved in settings.");
    h.run();
    assert_eq!(h.get_by_label(ERROR).accesskit_node().live(), Live::Assertive);
    assert_eq!(h.get_by_label(SUCCESS).accesskit_node().live(), Live::Polite);
    assert_eq!(h.get_by_label("Recent projects could not be saved in settings.").accesskit_node().live(), Live::Polite);
    // A message set straight into the slot is information.
    h.state_mut().notice = Some("Plain information".into());
    h.run();
    assert_eq!(h.get_by_label("Plain information").accesskit_node().live(), Live::Polite);
}

#[test]
fn the_latest_message_slot_still_shows_and_follows_its_changes() {
    let mut h = super::tests::harness();
    h.state_mut().notice = Some("Older plain message".into());
    h.run();
    h.get_by_label("Older plain message");
    // A plain message follows the slot: replaced, it goes.
    h.state_mut().notice = Some("Newer plain message".into());
    h.run();
    assert!(h.query_by_label("Older plain message").is_none());
    h.get_by_label("Newer plain message");
    // A typed error replaces the plain message but outlives a cleared slot.
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.run();
    assert!(h.query_by_label("Newer plain message").is_none());
    h.state_mut().notice = None;
    h.run();
    h.get_by_label(ERROR);
    // The same message twice shows once.
    h.state_mut().notify(NoticeKind::Error, ERROR);
    h.run();
    assert_eq!(h.query_all_by_label(ERROR).count(), 1);
}

#[test]
fn a_failed_command_from_the_palette_is_an_error_notice() {
    let mut h = super::tests::harness();
    // A command the session refuses for these params reports through the typed error helper.
    h.state_mut().dispatch_palette_command("canvas.no_such_command");
    h.run();
    let Some(text) = h.state().notice.clone() else { panic!("the failure is reported") };
    assert_eq!(h.get_by_label(&text).accesskit_node().live(), Live::Assertive);
    h.get_by_role_and_label(Role::Image, "Error");
}
