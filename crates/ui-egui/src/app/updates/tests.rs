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

/// A settings store in a fresh directory, which the caller removes.
fn store() -> (nodecraft_engine::settings::SettingsStore, std::path::PathBuf) {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("nodecraft-updates-{}-{nanos}", std::process::id()));
    let (store, _) = nodecraft_engine::settings::open(nodecraft_engine::settings::Location::Dir(dir.clone()));
    (store, dir)
}

fn offered(h: &mut Harness<'static, NodeCraftApp>) {
    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(h);
    assert_eq!(banner(h).as_deref(), Some("99.0.0"));
}

#[test]
fn the_banner_buttons_keep_their_choice_in_settings() {
    let (settings, dir) = store();
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    h.state_mut().store = Some(settings);
    offered(&mut h);
    h.get_by_label("Don't check at startup").click();
    h.run();
    assert_eq!(banner(&h), None);
    assert!(h.state().notice.as_deref().is_some_and(|notice| notice.contains("won't check for updates at startup")));
    assert!(!h.state().store.as_ref().unwrap().get().updates.check);

    offered(&mut h);
    h.get_by_label("Dismiss").click();
    h.run();
    assert_eq!(banner(&h), None);
    assert_eq!(h.state().store.as_ref().unwrap().get().updates.dismissed.as_deref(), Some("99.0.0"));
    drop(h);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn without_settings_the_banner_says_its_choice_wont_last() {
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    offered(&mut h);
    h.get_by_label("Don't check at startup").click();
    h.run();
    assert_eq!(banner(&h), None);
    assert_eq!(h.state().notice.as_deref(), Some(UNSAVED));
}

#[test]
fn an_explicit_check_that_finds_nothing_takes_down_an_earlier_offer() {
    let mut h = with_releases(Ok(vec![release("0.0.1")]));
    h.state_mut().updates.as_mut().unwrap().banner = Some(release("99.0.0"));
    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(&mut h);
    assert_eq!(banner(&h), None);
    assert!(h.state().notice.as_deref().is_some_and(|notice| notice.ends_with("is up to date.")));
}

/// A harness offering 99.0.0 whose installer stages `payload` as an AppImage in a temporary
/// folder, as the real download would once verified.
fn installing(payload: Result<Vec<u8>, Error>) -> (Harness<'static, NodeCraftApp>, tempfile::TempDir) {
    installing_after(payload, None)
}

/// [`installing`], each download finishing only once `gate` says so.
fn installing_after(payload: Result<Vec<u8>, Error>, gate: Option<std::sync::mpsc::Receiver<()>>) -> (Harness<'static, NodeCraftApp>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("NodeCraft.AppImage");
    std::fs::write(&file, b"\x7fELF old").unwrap();
    let kind = install::Kind::AppImage { file };
    let updates = dir.path().join("updates");
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    let staging = updates.clone();
    let job: installer::Job = Box::new(move |available| {
        if let Some(gate) = &gate {
            gate.recv().unwrap();
        }
        let bytes = payload.clone()?;
        install::stage(&kind, &update_install::Downloaded { version: available.version.clone(), file: "payload".into(), bytes }, &staging)
    });
    let handlers = h.state_mut().updates.as_mut().unwrap().with_installer(job, updates).unwrap();
    let (mut install, mut apply) = (handlers.install, handlers.apply);
    let session = &mut h.state_mut().session;
    session.register_host_service(INSTALL_SERVICE, move |request| install(request)).unwrap();
    session.register_host_service(APPLY_SERVICE, move |request| apply(request)).unwrap();
    (h, dir)
}

/// Frames until the install has finished.
fn installed(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..500 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_updates(&ctx);
        if !h.state().updates.as_ref().unwrap().is_installing() {
            h.run();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the install never finished");
}

fn install_state(h: &mut Harness<'static, NodeCraftApp>) -> serde_json::Value {
    h.state_mut().session.dispatch("app.update_status", &json!({})).unwrap()["install"].clone()
}

#[test]
fn the_banner_installs_the_offered_release_and_restarts_into_it() {
    let (open, gate) = std::sync::mpsc::channel();
    let (mut h, dir) = installing_after(Ok(b"\x7fELF new".to_vec()), Some(gate));
    // Nothing is offered before a check.
    let refused = h.state_mut().session.dispatch("app.install_update", &json!({})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);
    offered(&mut h);
    assert!(h.query_by_label("Restart to update").is_none());
    h.get_by_label("Install update").click();
    // The spinner keeps repainting: step, don't run to rest.
    h.step();
    // The frame records it for `app.update_status`.
    let ctx = h.ctx.clone();
    h.state_mut().tick_updates(&ctx);
    assert_eq!(install_state(&mut h)["state"], "downloading");
    h.step();
    h.get_by_label("Downloading and verifying NodeCraft 99.0.0…");
    open.send(()).unwrap();
    installed(&mut h);
    assert_eq!(install_state(&mut h), json!({"state": "ready", "version": "99.0.0"}));
    // Staged, not swapped in: the running copy is untouched until the restart.
    let file = dir.path().join("NodeCraft.AppImage");
    assert_eq!(std::fs::read(&file).unwrap(), b"\x7fELF old");

    h.get_by_label("Restart to update").click();
    h.run();
    let ctx = h.ctx.clone();
    h.state_mut().tick_updates(&ctx);
    assert_eq!(std::fs::read(&file).unwrap(), b"\x7fELF new");
    let installer = h.state().updates.as_ref().unwrap().installer.as_ref().unwrap();
    assert_eq!(installer.relaunch, Some(install::Relaunch::Exec(file)));
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("updates/last-update.json")).unwrap()).unwrap();
    assert_eq!(record["to"], "99.0.0");
}

#[test]
fn a_refused_download_shows_why_and_can_be_tried_again() {
    let (mut h, dir) = installing(Err(Error::new(ErrorKind::InvalidInput, "its signature doesn't match NodeCraft's release key")));
    offered(&mut h);
    h.state_mut().dispatch_palette_command("app.install_update");
    installed(&mut h);
    assert_eq!(install_state(&mut h)["state"], "failed");
    h.get_by_label_contains("The update didn't install: its signature doesn't match");
    h.get_by_label("Try again");
    assert!(h.query_by_label("Restart to update").is_none());
    let refused = h.state_mut().session.dispatch("app.restart_to_update", &json!({})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);
    assert_eq!(std::fs::read(dir.path().join("NodeCraft.AppImage")).unwrap(), b"\x7fELF old");
}

#[test]
fn restarting_waits_for_running_generations() {
    let (mut h, dir) = installing(Ok(b"\x7fELF new".to_vec()));
    offered(&mut h);
    h.state_mut().session.dispatch("app.install_update", &json!({})).unwrap();
    // A second ask while it waits or downloads is refused.
    assert_eq!(h.state_mut().session.dispatch("app.install_update", &json!({})).unwrap_err().kind, ErrorKind::Busy);
    installed(&mut h);
    let app = h.state_mut();
    let checker = app.updates.as_mut().unwrap();
    assert!(!checker.tick_install(&mut app.session, true));
    let refused = app.session.dispatch("app.restart_to_update", &json!({})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Busy);
    assert_eq!(std::fs::read(dir.path().join("NodeCraft.AppImage")).unwrap(), b"\x7fELF old");
    let checker = app.updates.as_mut().unwrap();
    assert!(!checker.tick_install(&mut app.session, false));
    assert_eq!(app.session.dispatch("app.restart_to_update", &json!({})).unwrap(), json!({"restarting": "99.0.0"}));
    // A generation that started after the swap (the busy check was a frame old) holds the close.
    let checker = app.updates.as_mut().unwrap();
    assert!(!checker.tick_install(&mut app.session, true));
    assert!(checker.tick_install(&mut app.session, false), "the window closes to restart");
}

#[test]
fn a_copy_that_cant_replace_itself_says_why_and_offers_no_install() {
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    h.state_mut().updates.as_mut().unwrap().cannot_install =
        Some("To install it, download it from the releases page: this copy isn't an installed app.".into());
    offered(&mut h);
    h.get_by_label_contains("this copy isn't an installed app");
    assert!(h.query_by_label("Install update").is_none());
    // Without the services the commands say this host doesn't install.
    let refused = h.state_mut().session.dispatch("app.install_update", &json!({})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
}

#[test]
#[ignore = "manual public-safe capture; UPDATES123_RENDER_DIR"]
fn updates123_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("UPDATES123_RENDER_DIR").unwrap());
    let mut h = with_releases(Ok(vec![release("99.0.0")]));
    h.state_mut().dispatch_palette_command("app.check_updates");
    answered(&mut h);
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "updates123-dark.png"), (crate::theme::ThemeKind::Light, "updates123-light.png")] {
        h.state_mut().theme = theme;
        let mut shot =
            Harness::builder().wgpu().with_size(egui::Vec2::new(1000.0, 600.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(egui::Vec2::new(1000.0, 600.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
}
