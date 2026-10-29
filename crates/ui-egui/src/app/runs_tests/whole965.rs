//! ⌘/Ctrl+Enter during a run (#965 review): a card still running from an earlier run is never
//! started, and charged, a second time by a whole-canvas run.
use super::*;

#[test]
fn ctrl_enter_while_a_card_runs_does_not_start_it_again() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mock = image(slow);
    let mut h = scene(making(mock.clone()));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    let sent = mock.clone();
    frames_until(&mut h, move |app| app.runs.as_ref().is_some_and(|runs| runs.is_running(C, N)) && sent.requests().len() == 1);
    // The card has no result yet, so a quote of what changed includes it.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert!(h.query_by_label_contains("still running from an earlier run").is_some(), "the dialog says why it can't start");
    let label = h.get_by_label_contains("Run changed for").accesskit_node().label().unwrap_or_default();
    let button = h.get_by_role_and_label(Role::Button, &label);
    assert!(button.accesskit_node().is_disabled(), "{label} must not start a second run of the card");
    press(&mut h, Role::Button, &label);
    for _ in 0..10 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.run();
    }
    assert_eq!(mock.requests().len(), 1, "the running card was not submitted again");
    assert!(h.state().run_confirm.is_some(), "the dialog stays until it is cancelled");
}
