//! The notice stack in the shell (#1110): Escape belongs to a notice only while it holds the
//! focus, and an action button runs its command as the palette would.
use super::tests::{harness, node};
use crate::notices::{Notice, NoticeKind};
use egui_kittest::kittest::Queryable;

#[test]
fn long_notices1110_keep_details_and_controls_inside_small_windows() {
    use egui_kittest::kittest::NodeT;
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        for percent in [100_u16, 200] {
            let scale = f32::from(percent) / 100.0;
            let mut h = harness();
            h.set_size(egui::vec2(640.0, 400.0));
            h.run_steps(3);
            h.state_mut().session.dispatch("view.ui_scale.set", &serde_json::json!({"percent": percent})).unwrap();
            h.state_mut().theme = theme;
            let text = format!("{}Final public detail.", "The provider refused this example; no run was started. ".repeat(45));
            let owner = "long-notice-layout";
            h.state_mut()
                .notices
                .push_notice(Notice::new(NoticeKind::Error, &text).from_run().owned_by(owner).with_action("Show map", "canvas.minimap_toggle"));
            h.run_steps(5);
            let screen = h.ctx.content_rect();
            assert!((screen.width() - 640.0 / scale).abs() < 1.0 && (screen.height() - 400.0 / scale).abs() < 1.0, "{percent}%: {screen:?}");
            for label in ["Dismiss notice", "Show map"] {
                let rect = h.get_by_label(label).rect();
                assert!(screen.contains_rect(rect), "{theme:?} {scale}: {label}: {rect:?} outside {screen:?}");
            }
            let label = h.get_by_label(&text);
            assert_eq!(label.accesskit_node().live(), egui::accesskit::Live::Assertive);
            assert!(label.accesskit_node().is_live_atomic());
            // Scroll within the details. Reading a long error neither discards it nor runs its action.
            let at = label.rect().min + egui::vec2(5.0, 5.0);
            h.hover_at(at);
            h.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0.0, -10_000.0),
                modifiers: egui::Modifiers::NONE,
            });
            h.run_steps(5);
            assert_eq!(h.state().notice(), Some(text.as_str()));
            if let Some(folder) = std::env::var_os("NODECRAFT_REVIEW_EVIDENCE") {
                let folder = std::path::PathBuf::from(folder);
                std::fs::create_dir_all(&folder).unwrap();
                h.render().unwrap().save(folder.join(format!("long-notice-{theme:?}-{scale}.png"))).unwrap();
            }
            h.get_by_label("Dismiss notice").focus();
            h.run_steps(2);
            h.key_press(egui::Key::Escape);
            h.run_steps(2);
            assert!(h.state().notices.is_empty());
        }
    }
}

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
    let canvas = h.state().canvas;
    let refused = "The run didn't start: the budget for today is spent";
    h.state_mut().notices.push_notice(Notice::new(NoticeKind::Error, refused).from_run());
    h.run();
    let ctx = h.ctx.clone();
    let still_there = |h: &egui_kittest::Harness<'static, super::NodeCraftApp>| h.state().notices.iter().any(|notice| notice.text == refused);
    // A card edit that succeeds (and clears passing notices).
    h.state_mut().card_edit(id, "node.title", &serde_json::json!({"canvas": canvas, "id": id, "title": "Renamed"})).unwrap();
    assert!(still_there(&h));
    // Delete, then undo it.
    let revision = h.state().session.revision();
    h.state_mut().selection.only(id);
    h.state_mut().dispatch_delete();
    assert!(h.state().session.revision() > revision, "the delete happened");
    assert!(still_there(&h));
    h.state_mut().run_palette_command(&ctx, "edit.undo");
    h.run();
    assert!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().contains_key(&id), "the undo happened");
    assert!(still_there(&h));
    // A paste of a copied card.
    h.state_mut().selection.only(id);
    h.event(egui::Event::Copy);
    h.step();
    let text = super::tests::copied(&h).expect("a copy");
    h.run();
    let count = h.state().session.document().canvases().get(&canvas).unwrap().nodes().len();
    h.hover_at(egui::pos2(800.0, 600.0));
    h.event(egui::Event::Paste(text));
    h.run();
    assert_eq!(h.state().session.document().canvases().get(&canvas).unwrap().nodes().len(), count + 1, "the paste happened");
    assert!(still_there(&h));
    // A search.
    h.state_mut().dispatch_navigation(&ctx, "canvas.search_open", &serde_json::json!({})).ok();
    h.state_mut().dispatch_search(&ctx, "canvas.search_query", &serde_json::json!({"query": "Renamed"})).ok();
    h.run();
    assert!(still_there(&h), "{:?}", h.state().notice());
}

#[test]
fn a_parameter_error_goes_once_the_parameter_is_fixed() {
    let mut h = harness();
    let card = node(h.state());
    let id = card.id;
    let (name, value) = card.params.iter().next().map(|(name, value)| (name.clone(), serde_json::to_value(value).unwrap())).unwrap();
    let canvas = h.state().canvas;
    h.state_mut().notices.error("A run failed: over budget");
    let bad = serde_json::json!({"canvas": canvas, "id": id, "name": "no_such_parameter", "value": {"type": "text", "value": "x"}});
    assert!(h.state_mut().card_edit(id, "node.param_set", &bad).is_err());
    let failed = h.state().notice().unwrap().to_owned();
    h.run();
    let good = serde_json::json!({"canvas": canvas, "id": id, "name": name, "value": value});
    h.state_mut().card_edit(id, "node.param_set", &good).unwrap();
    h.run();
    let texts: Vec<String> = h.state().notices.iter().map(|notice| notice.text.clone()).collect();
    assert!(!texts.contains(&failed), "the fixed parameter's error is gone: {texts:?}");
    assert_eq!(texts, ["A run failed: over budget"], "an unrelated error stays");
}

#[test]
fn a_failed_id_allocation_is_settled_by_the_next_document_change() {
    let mut h = harness();
    h.state_mut().notices.error_for(crate::notices::CANVAS_ADD, "Couldn't allocate a node id; try again.");
    h.state_mut().notices.error("A run failed: over budget");
    h.run();
    assert_eq!(h.state().notices.iter().count(), 2);
    let id = node(h.state()).id;
    let canvas = h.state().canvas;
    h.state_mut().card_edit(id, "node.title", &serde_json::json!({"canvas": canvas, "id": id, "title": "Renamed"})).unwrap();
    h.run();
    assert_eq!(h.state().notices.iter().map(|notice| notice.text.as_str()).collect::<Vec<_>>(), ["A run failed: over budget"]);
}
