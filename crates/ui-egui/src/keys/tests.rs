use super::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_engine::commands::Services;
use nodecraft_engine::secrets::{KeyResults, KeyWorker, MemoryStore, SecretStore, register_key_service, register_key_worker};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
use std::sync::Arc;

struct State {
    panel: KeysPanel,
    session: Session,
    settings: KeySettings,
    changes: Vec<AccountChange>,
    results: Option<KeyResults>,
}

const KEY: &str = "sk-proj-abcdefghij1234";

fn session(services: Services) -> Session {
    Session::new(Project::new(ProjectId::from_bytes([1; 16]), "Keys").unwrap()).unwrap().with_services(services)
}

fn harness(state: State) -> Harness<'static, State> {
    Harness::builder().with_size(egui::Vec2::new(900.0, 600.0)).build_ui_state(
        |ui, s: &mut State| {
            if let Some(results) = &s.results {
                let (changes, _) = s.panel.receive(results.drain());
                s.changes.extend(changes);
            }
            let changes = s.panel.show(ui.ctx(), &mut s.session, &s.settings, crate::theme::ThemeKind::Dark.tokens());
            s.changes.extend(changes);
        },
        state,
    )
}

fn sync_state(store: Arc<MemoryStore>, accounts: &[&str]) -> State {
    let mut services = Services::default();
    register_key_service(&mut services, store).unwrap();
    State {
        panel: KeysPanel { open: true, ..KeysPanel::default() },
        session: session(services),
        settings: KeySettings { accounts: accounts.iter().map(|a| (*a).to_owned()).collect() },
        changes: vec![],
        results: None,
    }
}

fn type_into(h: &mut Harness<'static, State>, label: &str, text: &str) {
    let role = if label == "Key" { Role::PasswordInput } else { Role::TextInput };
    h.get_by_role_and_label(role, label).click();
    h.run();
    h.event(egui::Event::Text(text.into()));
    h.run();
}

#[test]
fn saving_a_key_stores_it_clears_the_field_and_remembers_the_account() {
    let store = Arc::new(MemoryStore::default());
    let mut h = harness(sync_state(Arc::clone(&store), &[]));
    h.run();
    assert!(h.query_by_label("No keys yet.").is_some());
    h.get_by_label("anthropic").click();
    h.run();
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    let s = h.state();
    assert_eq!(s.panel.rows(), &[(Account::new("anthropic").unwrap(), KeyStatus::Stored("…1234".into()))]);
    assert_eq!(s.changes, [AccountChange::Remember(Account::new("anthropic").unwrap())]);
    assert!(s.panel.key.is_empty(), "the typed key is gone from the panel");
    assert_eq!(store.get(&Account::new("anthropic").unwrap()).unwrap().map(|k| k.expose().to_owned()).as_deref(), Some(KEY));
    // Only the masked tail is ever on screen.
    assert!(h.query_by_label_contains("abcdefghij").is_none());
    assert!(h.query_by_label("Stored …1234").is_some());
}

#[test]
fn listed_accounts_are_checked_only_on_request_and_removal_asks_first() {
    let store = Arc::new(MemoryStore::default());
    store.set(&Account::new("openai").unwrap(), &nodecraft_engine::secrets::Secret::new(KEY).unwrap()).unwrap();
    let mut h = harness(sync_state(Arc::clone(&store), &["openai", "fal"]));
    h.run();
    assert_eq!(
        h.state().panel.rows().iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
        [KeyStatus::Unchecked, KeyStatus::Unchecked],
        "no keychain reads at startup"
    );
    h.get_all_by_label("Check").next().unwrap().click();
    h.run();
    assert_eq!(h.state().panel.rows()[0].1, KeyStatus::Stored("…1234".into()));
    h.get_all_by_label("Check").nth(1).unwrap().click();
    h.run();
    assert_eq!(h.state().panel.rows()[1].1, KeyStatus::Missing);
    // Remove needs a second click.
    h.get_all_by_label("Remove").next().unwrap().click();
    h.run();
    assert!(store.get(&Account::new("openai").unwrap()).unwrap().is_some(), "not yet");
    h.get_by_label("Confirm remove").click();
    h.run();
    assert!(store.get(&Account::new("openai").unwrap()).unwrap().is_none());
    assert_eq!(h.state().panel.rows().len(), 1);
    assert_eq!(h.state().changes, [AccountChange::Forget(Account::new("openai").unwrap())]);
}

#[test]
fn bad_input_is_reported_without_echoing_the_key() {
    let mut h = harness(sync_state(Arc::new(MemoryStore::default()), &[]));
    h.run();
    type_into(&mut h, "Account", "Not Valid!");
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    let notice = h.state().panel.notice.clone().unwrap_or_default();
    assert!(notice.contains("not a valid key name"), "{notice}");
    assert!(!notice.contains("abcdefghij"));
    assert!(h.state().panel.key.is_empty());
    // A key with a space is refused by the engine, and the message doesn't contain it.
    let mut h = harness(sync_state(Arc::new(MemoryStore::default()), &[]));
    h.run();
    h.get_by_label("openai").click();
    h.run();
    type_into(&mut h, "Key", "sk-secret with space");
    h.get_by_label("Save").click();
    h.run();
    let notice = h.state().panel.notice.clone().unwrap_or_default();
    assert!(!notice.is_empty() && !notice.contains("sk-secret"), "{notice}");
}

#[test]
fn keychain_answers_arrive_later_from_the_worker() {
    let (worker, results) = KeyWorker::spawn(Arc::new(MemoryStore::default())).unwrap();
    let mut services = Services::default();
    register_key_worker(&mut services, worker).unwrap();
    let mut h = harness(State {
        panel: KeysPanel { open: true, ..KeysPanel::default() },
        session: session(services),
        settings: KeySettings::default(),
        changes: vec![],
        results: Some(results),
    });
    h.run();
    h.get_by_label("openai").click();
    h.run();
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().panel.is_waiting() {
        assert!(std::time::Instant::now() < deadline, "no answer from the keychain thread");
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run();
    }
    assert_eq!(h.state().panel.rows()[0].1, KeyStatus::Stored("…1234".into()));
    assert_eq!(h.state().changes, [AccountChange::Remember(Account::new("openai").unwrap())]);
}

#[test]
fn undo_after_save_and_closing_the_window_never_bring_the_key_back() {
    let mut h = harness(sync_state(Arc::new(MemoryStore::default()), &[]));
    h.run();
    h.get_by_label("openai").click();
    h.run();
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(h.state().panel.key.is_empty(), "undo history was cleared");
    // Typed but not saved, then the panel is closed (toolbar toggle): forgotten.
    type_into(&mut h, "Key", KEY);
    assert_eq!(h.state().panel.key, KEY);
    h.state_mut().panel.open = false;
    h.run();
    assert!(h.state().panel.key.is_empty());
    h.state_mut().panel.open = true;
    h.run();
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.run();
    assert!(h.state().panel.key.is_empty());
}

/// A keychain that refuses everything (locked, access denied).
struct Denied;
impl SecretStore for Denied {
    fn get(&self, _: &Account) -> Result<Option<nodecraft_engine::secrets::Secret>, nodecraft_engine::secrets::SecretError> {
        Err(nodecraft_engine::secrets::SecretError::Denied("locked".into()))
    }
    fn set(&self, _: &Account, _: &nodecraft_engine::secrets::Secret) -> Result<(), nodecraft_engine::secrets::SecretError> {
        Err(nodecraft_engine::secrets::SecretError::Denied("locked".into()))
    }
    fn delete(&self, _: &Account) -> Result<bool, nodecraft_engine::secrets::SecretError> {
        Ok(false)
    }
}

/// A keychain that never answers (a prompt nobody clicks).
struct Hangs;
impl SecretStore for Hangs {
    fn get(&self, _: &Account) -> Result<Option<nodecraft_engine::secrets::Secret>, nodecraft_engine::secrets::SecretError> {
        std::thread::sleep(std::time::Duration::from_secs(3600));
        Ok(None)
    }
    fn set(&self, _: &Account, _: &nodecraft_engine::secrets::Secret) -> Result<(), nodecraft_engine::secrets::SecretError> {
        Ok(())
    }
    fn delete(&self, _: &Account) -> Result<bool, nodecraft_engine::secrets::SecretError> {
        Ok(false)
    }
}

fn worker_state(store: Arc<dyn SecretStore>, accounts: &[&str]) -> State {
    let (worker, results) = KeyWorker::spawn(store).unwrap();
    let mut services = Services::default();
    register_key_worker(&mut services, worker).unwrap();
    State {
        panel: KeysPanel { open: true, ..KeysPanel::default() },
        session: session(services),
        settings: KeySettings { accounts: accounts.iter().map(|a| (*a).to_owned()).collect() },
        changes: vec![],
        results: Some(results),
    }
}

#[test]
fn keychain_errors_and_silence_become_retryable_failures() {
    let mut h = harness(worker_state(Arc::new(Denied), &["openai"]));
    h.run();
    h.get_by_label("Check").click();
    h.run();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().panel.is_waiting() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run();
    }
    assert!(
        matches!(&h.state().panel.rows()[0].1, KeyStatus::Failed(why) if why.contains("unlock the keychain") || why.contains("denied")),
        "{:?}",
        h.state().panel.rows()
    );

    let mut state = worker_state(Arc::new(Hangs), &["openai"]);
    state.panel.timeout_s = 0.2;
    let mut h = harness(state);
    h.run();
    h.get_by_label("Check").click();
    h.run();
    for _ in 0..30 {
        h.step();
    }
    assert!(!h.state().panel.is_waiting());
    assert!(matches!(&h.state().panel.rows()[0].1, KeyStatus::Failed(why) if why.contains("No answer")));
    assert!(h.query_by_label("Check").is_some(), "the row can be tried again");
}
