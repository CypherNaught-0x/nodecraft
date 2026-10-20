//! The desktop's update check (#123), with the release list served by a fixture.
use super::*;
use crate::app::NodeCraftApp;
use crate::app::tests::harness;
use crate::palette;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn release(version: &str) -> Release {
    Release { version: version.into(), url: format!("https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v{version}"), prerelease: version.contains('-') }
}

fn with_releases(fetched: Result<Vec<Release>, Error>) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let (checker, mut service) = UpdateChecker::spawn(Box::new(move || fetched.clone())).unwrap();
    let app = h.state_mut();
    app.session.register_host_service(UPDATE_SERVICE, move |request| service(request)).unwrap();
    app.updates = Some(checker);
    h
}

/// Frames until the check is recorded.
fn answered(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..500 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_updates(&ctx);
        if !h.state().updates.as_ref().unwrap().is_waiting() {
            h.run();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the check never answered");
}

fn banner(h: &Harness<'static, NodeCraftApp>) -> Option<String> {
    h.state().updates.as_ref().unwrap().banner.as_ref().map(|available| available.version.clone())
}

#[test]
fn checking_from_the_palette_offers_a_newer_release_until_it_is_dismissed() {
    let mut h = with_releases(Ok(vec![release("0.0.1"), release("99.0.0"), release("99.1.0-rc.1")]));
    assert!(h.state().palette_entries().iter().any(|entry| entry.target == palette::Target::Command("app.check_updates")));
    h.state_mut().dispatch_palette_command("app.check_updates");
    // A second ask while the first runs is refused, not queued behind it.
    let again = h.state_mut().session.dispatch("app.check_updates", &json!({})).unwrap_err();
    assert_eq!(again.kind, ErrorKind::Busy);
    answered(&mut h);
    // The stable channel skips the release candidate.
    assert_eq!(banner(&h).as_deref(), Some("99.0.0"));
    h.get_by_label_contains("NodeCraft 99.0.0 is available");
    h.get_by_label("Release notes");
    let status = h.state_mut().session.dispatch("app.update_status", &json!({})).unwrap();
    assert_eq!(status["last"]["available"]["version"], "99.0.0");
    assert_eq!(status["current"], VERSION);

    h.get_by_label("Dismiss").click();
    h.run();
    assert_eq!(banner(&h), None);
    assert!(h.query_by_label("Release notes").is_none());
}

#[test]
fn asking_when_up_to_date_or_offline_says_so() {
    let mut h = with_releases(Ok(vec![release("0.0.1")]));
    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(&mut h);
    assert_eq!(banner(&h), None);
    assert_eq!(h.state().notice.as_deref(), Some(format!("NodeCraft {VERSION} is up to date.").as_str()));

    let mut h = with_releases(Err(Error::new(ErrorKind::Network, "GitHub couldn't be reached: offline")));
    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(&mut h);
    assert_eq!(h.state().notice.as_deref(), Some("Couldn't check for updates: GitHub couldn't be reached: offline"));
}

#[test]
fn the_startup_check_is_silent_unless_it_finds_an_undismissed_release() {
    let settings = UpdateSettings::default();
    for fetched in [Ok(vec![release("0.0.1")]), Err(Error::new(ErrorKind::Network, "offline"))] {
        let mut h = with_releases(fetched);
        h.state_mut().check_updates_at_startup(&settings);
        answered(&mut h);
        assert_eq!(banner(&h), None);
        assert_eq!(h.state().notice, None);
    }

    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    h.state_mut().check_updates_at_startup(&settings);
    answered(&mut h);
    assert_eq!(banner(&h).as_deref(), Some("99.0.0"));

    // Turned off, nothing is asked at all.
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    h.state_mut().check_updates_at_startup(&UpdateSettings { check: false, ..UpdateSettings::default() });
    assert!(!h.state().updates.as_ref().unwrap().is_waiting());
}

#[test]
fn a_dismissed_release_returns_only_when_asked_for() {
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    let dismissed = UpdateSettings { dismissed: Some("99.0.0".into()), ..UpdateSettings::default() };
    h.state_mut().check_updates_at_startup(&dismissed);
    // `tick_updates` reads the store, which the harness hasn't; tick with the dismissal directly.
    for _ in 0..500 {
        let app = h.state_mut();
        let checker = app.updates.as_mut().unwrap();
        assert_eq!(checker.tick(&mut app.session, &dismissed), None);
        if !checker.is_waiting() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(banner(&h), None);
    assert_eq!(h.state().notice, None);

    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(&mut h);
    assert_eq!(banner(&h).as_deref(), Some("99.0.0"));
}

#[test]
fn turning_off_the_startup_check_from_the_banner_closes_it_and_says_how_to_check() {
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(&mut h);
    h.get_by_label("Don't check at startup").click();
    h.run();
    assert_eq!(banner(&h), None);
    assert!(h.state().notice.as_deref().is_some_and(|notice| notice.contains("won't check for updates at startup")));
}
