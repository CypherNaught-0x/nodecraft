use super::*;

#[test]
fn the_queue_is_capped_and_keeps_errors_over_older_information() {
    let mut notices = Notices::default();
    notices.error("Provider refused the key");
    notices.info("one");
    notices.success("two");
    notices.info("three");
    let texts: Vec<&str> = notices.iter().map(|notice| notice.text.as_str()).collect();
    assert_eq!(texts, ["three", "two", "Provider refused the key"]);
}

#[test]
fn errors_are_never_evicted_for_passing_notices_and_extra_errors_wait_below() {
    let mut notices = Notices::default();
    for i in 1..=3 {
        notices.error(format!("E{i}"));
    }
    // Only errors are shown: a passing notice is dropped rather than evicting one.
    assert_eq!(notices.push_notice(Notice::new(NoticeKind::Success, "Saved")), None);
    assert_eq!(notices.iter().map(|notice| notice.text.as_str()).collect::<Vec<_>>(), ["E3", "E2", "E1"]);
    // A fourth error is kept, under the shown three.
    notices.error("E4");
    assert_eq!(notices.iter().count(), 4);
    assert_eq!(notices.latest_text(), Some("E4"));
    // Bounded all the same.
    for i in 0..100 {
        notices.error(format!("more {i}"));
    }
    assert_eq!(notices.iter().count(), MAX_STICKY);
    assert_eq!(notices.latest_text(), Some("more 99"));
}

#[test]
fn clear_keeps_sticky_notices_and_their_owner_takes_them_back() {
    let mut notices = Notices::default();
    let id = notices.report_id(Error::new(ErrorKind::InvalidInput, "Bad name"));
    notices.push_notice(Notice::new(NoticeKind::Warning, "Runs can't be kept for a restart").keep());
    notices.info("Copied");
    notices.clear();
    notices.outcome(Ok::<(), Error>(()));
    notices.set(NoticeKind::Info, None);
    assert_eq!(notices.iter().map(|notice| notice.text.as_str()).collect::<Vec<_>>(), ["Runs can't be kept for a restart", "Bad name"]);
    notices.dismiss(id.unwrap());
    assert_eq!(notices.latest_text(), Some("Runs can't be kept for a restart"));
}

#[test]
fn the_same_message_again_refreshes_instead_of_stacking() {
    let mut notices = Notices::default();
    notices.info("Saved");
    notices.error("Failed");
    notices.info("Saved");
    let texts: Vec<&str> = notices.iter().map(|notice| notice.text.as_str()).collect();
    assert_eq!(texts, ["Saved", "Failed"]);
}

#[test]
fn engine_errors_map_to_a_kind_in_one_place() {
    assert_eq!(NoticeKind::of_error(ErrorKind::Busy), NoticeKind::Warning);
    for kind in [ErrorKind::Budget, ErrorKind::Authentication, ErrorKind::Network, ErrorKind::InvalidInput, ErrorKind::Internal] {
        assert_eq!(NoticeKind::of_error(kind), NoticeKind::Error);
    }
    let mut notices = Notices::default();
    notices.report(Error::new(ErrorKind::Budget, "Over budget"));
    assert_eq!(notices.latest().map(|notice| (notice.kind, notice.sticky)), Some((NoticeKind::Error, true)));
}

#[test]
fn long_text_is_cut_at_a_character_boundary() {
    let notice = Notice::new(NoticeKind::Info, "é".repeat(MAX_TEXT_BYTES));
    assert!(notice.text.len() <= MAX_TEXT_BYTES);
    assert!(notice.text.chars().all(|c| c == 'é'));
}

#[test]
fn the_clock_ignores_non_finite_and_backward_time() {
    let mut notices = Notices::default();
    notices.info("hello");
    for now in [0.0, f64::NAN, -5.0, f64::INFINITY, 1.0] {
        notices.tick(now, true);
    }
    assert_eq!(notices.latest_text(), Some("hello"));
    // Time while the stack wasn't drawn doesn't count.
    notices.tick(1000.0, false);
    assert_eq!(notices.latest_text(), Some("hello"));
    notices.tick(1000.0 + TIMEOUT_SECONDS, true);
    assert!(notices.is_empty());
}

mod kittest {
    use super::*;
    use crate::theme::ThemeKind;
    use egui::Vec2;
    use egui_kittest::{
        Harness,
        kittest::{NodeT, Queryable},
    };

    fn stack(kinds: &[(NoticeKind, &str)]) -> Harness<'static, Notices> {
        let mut notices = Notices::default();
        for (kind, text) in kinds {
            notices.push(*kind, *text);
        }
        Harness::builder().with_size(Vec2::new(520.0, 360.0)).build_ui_state(
            |ui, notices: &mut Notices| {
                theme::apply(ui.ctx(), ThemeKind::Dark);
                ui.set_width(ThemeKind::Dark.tokens().notice_width);
                notices.show(ui, ThemeKind::Dark.tokens(), None);
            },
            notices,
        )
    }

    /// Whether a pixel of `colour` lies left of `icon` (the kind's icon) on its centre row: the
    /// notice's edge, not the icon, which is tinted the same.
    fn edge_has(pixel: impl Fn(u32, u32) -> [u8; 4], icon: egui::Rect, colour: Color32) -> bool {
        let y = icon.center().y.round() as u32;
        (0..icon.left().max(0.0) as u32).any(|x| [colour.r(), colour.g(), colour.b()].iter().zip(pixel(x, y)).all(|(want, got)| want.abs_diff(got) <= 12))
    }

    #[test]
    fn an_error_and_a_success_render_with_different_icons_and_edges() {
        let t = ThemeKind::Dark.tokens();
        let mut h = stack(&[(NoticeKind::Error, "The provider refused the key."), (NoticeKind::Success, "Report copied.")]);
        h.run();
        // Each kind has its own icon, named for screen readers, so colour isn't the only cue.
        assert_ne!(NoticeKind::Error.icon(), NoticeKind::Success.icon());
        h.get_by_label(NoticeKind::Error.label());
        h.get_by_label(NoticeKind::Success.label());
        let error = h.get_by_label(NoticeKind::Error.label()).rect();
        let success = h.get_by_label(NoticeKind::Success.label()).rect();
        let image = h.render().unwrap();
        let pixel = |x, y| image.get_pixel(x, y).0;
        assert!(edge_has(pixel, error, t.danger) && !edge_has(pixel, error, t.accent), "the error has a danger edge");
        assert!(edge_has(pixel, success, t.accent) && !edge_has(pixel, success, t.danger), "the success has an accent edge");
    }

    #[test]
    fn three_notices_stack_newest_on_top() {
        let mut h = stack(&[(NoticeKind::Info, "First"), (NoticeKind::Warning, "Second"), (NoticeKind::Error, "Third")]);
        h.run();
        let tops: Vec<f32> = ["Third", "Second", "First"].iter().map(|text| h.get_by_label(text).rect().top()).collect();
        assert!(tops.windows(2).all(|pair| pair[0] < pair[1]), "{tops:?}");
        assert_eq!(h.get_all_by_label("Dismiss notice").count(), 3);
    }

    #[test]
    fn the_close_button_dismisses_its_notice() {
        let mut h = stack(&[(NoticeKind::Info, "Older"), (NoticeKind::Error, "Newest")]);
        h.run();
        // The newest is on top, so the first close button is its own.
        h.get_all_by_label("Dismiss notice").next().unwrap().click();
        h.run();
        assert_eq!(h.state().iter().map(|notice| notice.text.as_str()).collect::<Vec<_>>(), ["Older"]);
        assert!(h.query_by_label("Newest").is_none());
    }

    #[test]
    fn an_error_stays_after_the_timeout_and_information_goes() {
        let mut h = stack(&[(NoticeKind::Error, "Budget reached"), (NoticeKind::Success, "Saved"), (NoticeKind::Info, "Hello")]);
        // Twice the timeout of frames at the default quarter-second step.
        for _ in 0..(TIMEOUT_SECONDS * 8.0) as usize {
            h.step();
        }
        h.run();
        assert_eq!(h.state().iter().map(|notice| notice.text.as_str()).collect::<Vec<_>>(), ["Budget reached"]);
        assert!(h.query_by_label("Budget reached").is_some());
    }

    #[test]
    fn a_notice_times_out_across_long_gaps_between_frames() {
        // An idle window repaints only when asked: the clock counts the whole gap, so two frames
        // a few seconds apart are enough (#1110 review).
        let mut notices = Notices::default();
        notices.info("Copied");
        let mut h = Harness::builder().with_step_dt(4.0).with_size(Vec2::new(520.0, 200.0)).build_ui_state(
            |ui, notices: &mut Notices| {
                notices.show(ui, ThemeKind::Dark.tokens(), None);
            },
            notices,
        );
        // With a quarter-second cap per frame this took 24 frames; now the gap counts in full.
        let mut frames = 0;
        while !h.state().is_empty() && frames < 24 {
            h.step();
            frames += 1;
        }
        assert!(h.state().is_empty() && frames <= 3, "{frames} frames");
    }

    #[test]
    fn a_hidden_stack_has_no_clock_and_no_focus() {
        let mut notices = Notices::default();
        notices.info("Wait for me");
        let mut h = Harness::builder().with_step_dt(1.0).with_size(Vec2::new(520.0, 200.0)).build_ui_state(
            |ui, (notices, drawn): &mut (Notices, bool)| {
                if *drawn {
                    notices.show(ui, ThemeKind::Dark.tokens(), None);
                }
            },
            (notices, true),
        );
        h.run();
        h.get_by_label("Dismiss notice").focus();
        h.run();
        assert!(h.state().0.focused(&h.ctx).is_some());
        h.state_mut().1 = false;
        for _ in 0..20 {
            h.step();
        }
        assert!(h.state().0.focused(&h.ctx).is_none(), "a hidden notice can't take Escape");
        assert_eq!(h.state().0.latest_text(), Some("Wait for me"), "its clock stopped while hidden");
    }

    #[test]
    fn errors_beyond_the_shown_three_are_counted_below() {
        let mut h = stack(&[(NoticeKind::Error, "E1"), (NoticeKind::Error, "E2"), (NoticeKind::Error, "E3"), (NoticeKind::Error, "E4")]);
        h.run();
        assert!(h.query_by_label("E1").is_none());
        h.get_by_label("1 more notice below; dismiss these to see it.");
        h.get_all_by_label("Dismiss notice").next().unwrap().click();
        h.run();
        h.get_by_label("E1");
    }

    #[test]
    fn hovering_a_notice_pauses_its_timeout() {
        let mut h = stack(&[(NoticeKind::Info, "Hold me")]);
        h.run();
        h.get_by_label("Hold me").hover();
        for _ in 0..(TIMEOUT_SECONDS * 8.0) as usize {
            h.step();
        }
        assert_eq!(h.state().latest_text(), Some("Hold me"));
        h.hover_at(egui::pos2(510.0, 350.0));
        for _ in 0..(TIMEOUT_SECONDS * 8.0) as usize {
            h.step();
        }
        assert!(h.state().is_empty());
    }

    #[test]
    fn each_notice_is_a_live_region_polite_or_assertive_by_kind() {
        let mut h = stack(&[(NoticeKind::Info, "An info"), (NoticeKind::Success, "A success"), (NoticeKind::Warning, "A warning")]);
        h.run();
        for (text, live) in
            [("An info", egui::accesskit::Live::Polite), ("A success", egui::accesskit::Live::Polite), ("A warning", egui::accesskit::Live::Assertive)]
        {
            let node = h.get_by_label(text);
            assert_eq!(node.accesskit_node().live(), live, "{text}");
            assert!(node.accesskit_node().is_live_atomic());
        }
        h.state_mut().error("An error");
        h.run();
        assert_eq!(h.get_by_label("An error").accesskit_node().live(), egui::accesskit::Live::Assertive);
        assert_eq!(h.get_by_label("An error").accesskit_node().description().as_deref(), Some("Error"));
    }

    #[test]
    fn an_action_button_reports_its_command() {
        let mut notices = Notices::default();
        notices.push_notice(Notice::new(NoticeKind::Error, "Out of date").with_action("Show map", "canvas.minimap_toggle"));
        let pressed = std::rc::Rc::new(std::cell::RefCell::new(None));
        let seen = std::rc::Rc::clone(&pressed);
        let mut h = Harness::builder().with_size(Vec2::new(520.0, 200.0)).build_ui_state(
            move |ui, notices: &mut Notices| {
                if let Some(command) = notices.show(ui, ThemeKind::Dark.tokens(), None).action {
                    *seen.borrow_mut() = Some(command);
                }
            },
            notices,
        );
        h.run();
        h.get_by_label("Show map").click();
        h.run();
        assert_eq!(pressed.borrow().as_deref(), Some("canvas.minimap_toggle"));
    }

    #[test]
    fn inline_error_draws_the_error_icon_and_danger_text() {
        let mut h = Harness::builder().with_size(Vec2::new(400.0, 80.0)).build_ui(|ui| {
            inline_error(ui, ThemeKind::Dark.tokens(), "Steps must be at most 50");
        });
        h.run();
        h.get_by_label("Error");
        h.get_by_label("Steps must be at most 50");
    }
}

#[test]
fn background_reports_are_classified_by_their_own_wording() {
    assert_eq!(NoticeKind::of_report("Fetched 2 option lists (9 values)."), NoticeKind::Success);
    assert_eq!(NoticeKind::of_report("Fetched 2 option lists (9 values). Sizes for fal wasn't installed: bad."), NoticeKind::Warning);
    assert_eq!(NoticeKind::of_report("Option lists weren't fetched: offline"), NoticeKind::Error);
    assert_eq!(NoticeKind::of_report("Couldn't check for updates: offline"), NoticeKind::Error);
    assert_eq!(NoticeKind::of_report("NodeCraft 1.2.0 is up to date."), NoticeKind::Success);
    assert_eq!(NoticeKind::of_report("Removing downloaded templates…"), NoticeKind::Info);
}
