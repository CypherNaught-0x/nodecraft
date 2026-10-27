//! The host keeps the remembered key accounts in saved settings, including answers that arrive
//! after the keys panel's timeout (#240). The keychain is a channel-controlled fake.
#![cfg(not(target_arch = "wasm32"))]

use super::*;
use crate::keys::AccountChange;
use crate::keys::fake::{Call, Keychain};
use nodecraft_engine::secrets::{Account, KeyWorker, register_key_worker};
use nodecraft_engine::settings::{KeySettings, Location};

const KEY: &str = "sk-proj-abcdefghij1234";

fn settings_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "nodecraft-ui-keys-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ))
}

/// One host frame at egui time `time`: the keychain's answers are taken and saved.
fn frame(app: &mut NodeCraftApp, ctx: &egui::Context, time: f64) {
    let mut output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| app.tick_keys(ui.ctx()));
    output.textures_delta.clear();
}

fn saved_accounts(dir: &std::path::Path) -> Vec<Account> {
    let (saved, _) = nodecraft_engine::settings::open(Location::Dir(dir.to_path_buf()));
    saved.get().keys.accounts()
}

#[test]
fn a_late_keychain_answer_reaches_saved_settings() {
    let dir = settings_dir();
    let (store, _) = nodecraft_engine::settings::open(Location::Dir(dir.clone()));
    let mut app = NodeCraftApp::headless().unwrap();
    app.store = Some(store);
    let (keychain_store, keychain) = Keychain::new();
    let (worker, results) = KeyWorker::spawn(keychain_store).unwrap();
    let mut services = nodecraft_engine::commands::Services::default();
    register_key_worker(&mut services, worker).unwrap();
    app.session =
        Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([2; 16]), "Keys").unwrap()).unwrap().with_services(services);
    app.key_results = Some(results);
    app.keys.timeout_s = 1.0;
    let ctx = egui::Context::default();
    let openai = Account::new("openai").unwrap();

    assert_eq!(app.keys.save(&mut app.session, &app.key_accounts, "openai", KEY.into()), None);
    keychain.entered(Call::Set);
    frame(&mut app, &ctx, 0.5);
    assert!(app.keys.is_waiting());
    frame(&mut app, &ctx, 5.0);
    assert!(!app.keys.is_waiting() && app.keys.is_following(), "timed out, still followed");
    assert!(app.key_accounts.accounts().is_empty());
    // The unlock prompt is accepted late.
    keychain.answer(Ok(()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.keys.is_following() {
        assert!(std::time::Instant::now() < deadline, "the late answer never arrived");
        std::thread::sleep(std::time::Duration::from_millis(1));
        frame(&mut app, &ctx, 6.0);
    }
    assert_eq!(app.key_accounts.accounts(), std::slice::from_ref(&openai));
    app.store.as_mut().unwrap().flush().unwrap();
    assert_eq!(saved_accounts(&dir), std::slice::from_ref(&openai), "the late save survives a restart");

    // A list that lost the account (edited elsewhere) gets it back when a check finds the key.
    app.key_accounts.forget(&openai);
    app.store.as_mut().unwrap().update(|settings| settings.keys.forget(&openai));
    app.save_accounts(vec![AccountChange::Found(openai.clone())]);
    app.store.as_mut().unwrap().flush().unwrap();
    assert_eq!(saved_accounts(&dir), std::slice::from_ref(&openai));

    // If the list filled up underneath the panel, the person is told the account won't stay listed.
    let full = KeySettings { accounts: (0..KeySettings::MAX_ACCOUNTS).map(|i| format!("p{i}")).collect() };
    app.key_accounts = full.clone();
    app.store.as_mut().unwrap().update(|settings| settings.keys = full.clone());
    app.save_accounts(vec![AccountChange::Remember(openai.clone())]);
    let notice = app.keys.notice.clone().unwrap_or_default();
    assert!(notice.contains("won't be listed after a restart") && notice.contains("openai"), "{notice}");
    assert!(!notice.contains("abcdefghij"));
    drop(app);
    keychain.closed();
    let _ = std::fs::remove_dir_all(&dir);
}
