use super::*;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_engine::commands::Services;
use nodecraft_engine::secrets::{KeyOutcome, KeyResults, KeyWorker, MemoryStore, SecretStore, register_key_service, register_key_worker};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
use std::sync::Arc;

struct State {
    panel: KeysPanel,
    session: Session,
    settings: KeySettings,
    changes: Vec<AccountChange>,
    results: Option<KeyResults>,
    completed: Vec<KeyOutcome>,
}

const KEY: &str = "sk-proj-abcdefghij1234";

fn session(services: Services) -> Session {
    Session::new(Project::new(ProjectId::from_bytes([1; 16]), "Keys").unwrap()).unwrap().with_services(services)
}

fn harness(state: State) -> Harness<'static, State> {
    harness_at_dt(state, None)
}
fn harness_at_dt(state: State, step_dt: Option<f32>) -> Harness<'static, State> {
    let mut builder = Harness::builder().with_size(egui::Vec2::new(900.0, 600.0));
    if let Some(step_dt) = step_dt {
        builder = builder.with_step_dt(step_dt);
    }
    builder.build_ui_state(
        |ui, s: &mut State| {
            // As the host does: tick every frame, open or not, then take the keychain's answers.
            s.panel.tick(ui.input(|i| i.time));
            if let Some(results) = &s.results {
                let mut outcomes = std::mem::take(&mut s.completed);
                outcomes.extend(results.drain());
                let (changes, _) = s.panel.receive(outcomes);
                s.record(changes);
            }
            let changes = s.panel.show(ui.ctx(), &mut s.session, &s.settings, crate::theme::ThemeKind::Dark.tokens());
            s.record(changes);
        },
        state,
    )
}

impl State {
    /// Saves account changes in settings, as the host does.
    fn record(&mut self, changes: Vec<AccountChange>) {
        for change in &changes {
            match change {
                AccountChange::Remember(account) | AccountChange::Found(account) => {
                    self.settings.remember(account);
                }
                AccountChange::Forget(account) => self.settings.forget(account),
            }
        }
        self.changes.extend(changes);
    }

    /// Waits for `n` keychain answers and hands them to the panel, without drawing a frame.
    fn settle(&mut self, n: usize) {
        let outcomes = take_outcomes(self.results.as_ref().unwrap(), n);
        let (changes, others) = self.panel.receive(outcomes);
        assert!(others.is_empty(), "every answer is one of the panel's requests");
        self.record(changes);
    }
}

/// Waits for exactly `n` keychain answers.
fn take_outcomes(results: &KeyResults, n: usize) -> Vec<KeyOutcome> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut outcomes = Vec::new();
    while outcomes.len() < n {
        outcomes.extend(results.drain());
        assert!(std::time::Instant::now() < deadline, "the keychain answer never arrived");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(outcomes.len(), n, "exactly the requested operations completed");
    outcomes
}

/// Hands the next `n` answers to the panel through the harness's normal frames.
fn deliver(h: &mut Harness<'static, State>, n: usize) {
    let outcomes = take_outcomes(h.state().results.as_ref().unwrap(), n);
    h.state_mut().completed.extend(outcomes);
    h.run_steps(2);
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
        completed: vec![],
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
fn runway_key_preset_is_selected_without_reading_or_sending_a_credential() {
    let mut h = harness(sync_state(Arc::new(MemoryStore::default()), &[]));
    h.run();
    h.get_by_label("runway").click();
    h.run();
    assert_eq!(h.state().panel.account, "runway");
    assert!(h.query_by_label("Powered by Runway").is_some());
    assert!(h.state().panel.key.is_empty() && h.state().changes.is_empty());
}

#[test]
#[ignore = "manual public GPU capture; requires NODECRAFT_RUNWAY_CAPTURE_DIR"]
fn runway25_visual() {
    let output = std::path::PathBuf::from(std::env::var("NODECRAFT_RUNWAY_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&output).unwrap();
    let before = std::env::var("NODECRAFT_RUNWAY_BASELINE").as_deref() == Ok("1");
    let mut h = harness(sync_state(Arc::new(MemoryStore::default()), &[]));
    h.run();
    type_into(&mut h, "Account", "runway");
    h.run();
    assert_eq!(h.query_by_label("Powered by Runway").is_some(), !before);
    assert!(h.state().panel.key.is_empty() && h.state().changes.is_empty());
    h.render().unwrap().save(output.join(if before { "before.png" } else { "after.png" })).unwrap();
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
    // The check found openai's key (so it stays listed); then it was removed.
    assert_eq!(h.state().changes, [AccountChange::Found(Account::new("openai").unwrap()), AccountChange::Forget(Account::new("openai").unwrap())]);
    assert_eq!(h.state().settings.accounts(), [Account::new("fal").unwrap()]);
}

#[test]
fn azure_quick_pick_stores_the_separate_provider_account() {
    let store = Arc::new(MemoryStore::default());
    let mut h = harness(sync_state(Arc::clone(&store), &[]));
    h.run();
    h.get_by_label("azure-openai").click();
    h.run();
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    assert!(store.get(&Account::new("azure-openai").unwrap()).unwrap().is_some());
    assert!(store.get(&Account::new("openai").unwrap()).unwrap().is_none());
    assert!(h.state().panel.key.is_empty());
    assert!(h.query_by_label_contains("abcdefghij").is_none());
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

/// Hold the actual worker's Save until the test has observed the pending UI state.
struct ControlledSaveStore {
    memory: Arc<MemoryStore>,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    exited: std::sync::mpsc::Sender<()>,
}
impl SecretStore for ControlledSaveStore {
    fn get(&self, account: &Account) -> Result<Option<nodecraft_engine::secrets::Secret>, nodecraft_engine::secrets::SecretError> {
        self.memory.get(account)
    }
    fn set(&self, account: &Account, secret: &nodecraft_engine::secrets::Secret) -> Result<(), nodecraft_engine::secrets::SecretError> {
        self.entered.send(()).map_err(|_| nodecraft_engine::secrets::SecretError::Failed("fixture closed".into()))?;
        self.release.lock().unwrap().recv().map_err(|_| nodecraft_engine::secrets::SecretError::Failed("fixture closed".into()))?;
        self.memory.set(account, secret)
    }
    fn delete(&self, account: &Account) -> Result<bool, nodecraft_engine::secrets::SecretError> {
        self.memory.delete(account)
    }
}
impl Drop for ControlledSaveStore {
    fn drop(&mut self) {
        let _ = self.exited.send(());
    }
}

#[test]
fn keychain_answers_arrive_later_from_the_worker() {
    let memory = Arc::new(MemoryStore::default());
    let account = Account::new("openai").unwrap();
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, releasing) = std::sync::mpsc::channel();
    let (exited, exiting) = std::sync::mpsc::channel();
    let store = Arc::new(ControlledSaveStore { memory: Arc::clone(&memory), entered, release: std::sync::Mutex::new(releasing), exited });
    let mut h = harness_at_dt(worker_state(store, &[]), Some(1.0 / 60.0));
    h.run_steps(2);
    h.get_by_label("openai").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run_steps(2);
    h.event(egui::Event::Text(KEY.into()));
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2); // No quiescence requirement while the worker is deliberately blocked.
    entering.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(h.state().panel.is_waiting());
    assert!(h.state().panel.key.is_empty(), "Save clears the password before completion");
    assert!(h.state().changes.is_empty(), "an unfinished write must not remember the account");
    assert!(memory.get(&account).unwrap().is_none());
    // More Grid frames cannot manufacture an answer while the backend is held.
    h.run_steps(2);
    assert!(h.state().panel.is_waiting());
    release.send(()).unwrap();
    wait_for_answer(&mut h);
    assert_eq!(h.state().panel.rows(), &[(account.clone(), KeyStatus::Stored("…1234".into()))]);
    assert_eq!(h.state().changes, [AccountChange::Remember(account.clone())]);
    assert!(h.state().panel.key.is_empty());
    assert_eq!(memory.get(&account).unwrap().map(|key| key.expose().to_owned()).as_deref(), Some(KEY));
    assert!(h.query_by_label_contains("abcdefghij").is_none());
    assert!(h.query_by_label("Stored …1234").is_some());
    drop(h);
    exiting.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
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
    // Even after deleting what was typed, closing clears the undo history.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    h.run();
    assert!(h.state().panel.key.is_empty());
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

/// Keep worker completion separate from Grid layout frames. Neither a quick denial nor an
/// unanswered prompt requires Harness::run to become quiescent within its default four frames.
struct ControlledStore {
    entered: std::sync::mpsc::Sender<()>,
    replies: std::sync::Mutex<std::sync::mpsc::Receiver<Result<Option<nodecraft_engine::secrets::Secret>, nodecraft_engine::secrets::SecretError>>>,
    exited: std::sync::mpsc::Sender<()>,
}
impl SecretStore for ControlledStore {
    fn get(&self, _: &Account) -> Result<Option<nodecraft_engine::secrets::Secret>, nodecraft_engine::secrets::SecretError> {
        self.entered.send(()).map_err(|_| nodecraft_engine::secrets::SecretError::Failed("fixture closed".into()))?;
        self.replies.lock().unwrap().recv().map_err(|_| nodecraft_engine::secrets::SecretError::Failed("fixture closed".into()))?
    }
    fn set(&self, _: &Account, _: &nodecraft_engine::secrets::Secret) -> Result<(), nodecraft_engine::secrets::SecretError> {
        Err(nodecraft_engine::secrets::SecretError::Failed("unexpected fixture write".into()))
    }
    fn delete(&self, _: &Account) -> Result<bool, nodecraft_engine::secrets::SecretError> {
        Err(nodecraft_engine::secrets::SecretError::Failed("unexpected fixture delete".into()))
    }
}
impl Drop for ControlledStore {
    fn drop(&mut self) {
        let _ = self.exited.send(());
    }
}
struct StoreControl {
    entered: std::sync::mpsc::Receiver<()>,
    replies: std::sync::mpsc::Sender<Result<Option<nodecraft_engine::secrets::Secret>, nodecraft_engine::secrets::SecretError>>,
    exited: std::sync::mpsc::Receiver<()>,
}
fn controlled_store() -> (Arc<dyn SecretStore>, StoreControl) {
    let (entered, entering) = std::sync::mpsc::channel();
    let (replies, replying) = std::sync::mpsc::channel();
    let (exited, exiting) = std::sync::mpsc::channel();
    (Arc::new(ControlledStore { entered, replies: std::sync::Mutex::new(replying), exited }), StoreControl { entered: entering, replies, exited: exiting })
}
fn wait_for_answer(h: &mut Harness<'static, State>) {
    assert!(h.state().panel.is_waiting(), "a real request must precede completion");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        // Wait for an actual worker outcome without advancing simulated UI time. A panel
        // timeout is not evidence that the worker completed and cannot satisfy this wait.
        let outcomes = h.state().results.as_ref().unwrap().drain();
        if !outcomes.is_empty() {
            assert_eq!(outcomes.len(), 1, "exactly the requested operation completed");
            h.state_mut().completed.extend(outcomes);
            break;
        }
        assert!(std::time::Instant::now() < deadline, "controlled keychain answer never arrived");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    h.run_steps(2); // Deliver the real outcome through the harness's normal receive callback.
    assert!(!h.state().panel.is_waiting(), "the worker outcome must match the pending request");
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
        completed: vec![],
    }
}

#[test]
fn keychain_errors_and_silence_become_retryable_failures() {
    let (store, control) = controlled_store();
    let mut h = harness_at_dt(worker_state(store, &["openai"]), Some(1.0 / 60.0));
    h.run_steps(2);
    for _ in 0..2 {
        h.get_by_label("Check").click();
        h.run_steps(2);
        control.entered.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(h.state().panel.is_waiting(), "the controlled call is still awaiting its reply");
        control.replies.send(Err(nodecraft_engine::secrets::SecretError::Denied("locked".into()))).unwrap();
        wait_for_answer(&mut h);
        assert!(
            matches!(&h.state().panel.rows()[0].1, KeyStatus::Failed(why) if why.contains("unlock the keychain") || why.contains("denied")),
            "{:?}",
            h.state().panel.rows()
        );
        assert!(h.query_by_label("Check").is_some(), "a denied read can be retried");
        assert!(h.state().changes.is_empty());
    }
    drop(h);
    control.exited.recv_timeout(std::time::Duration::from_secs(10)).unwrap();

    let (store, control) = controlled_store();
    let mut state = worker_state(store, &["openai"]);
    state.panel.timeout_s = 0.2;
    // The default 250ms test frame exceeds this fixture's 200ms timeout. Advance
    // an explicit 60Hz clock so pending and timed-out states are tested separately.
    let mut h = harness_at_dt(state, Some(1.0 / 60.0));
    h.run_steps(2);
    h.get_by_label("Check").click();
    h.run_steps(2);
    control.entered.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(h.state().panel.is_waiting());
    for _ in 0..30 {
        h.step();
    }
    assert!(!h.state().panel.is_waiting());
    assert!(h.state().panel.is_following(), "the keychain may still answer");
    assert!(matches!(&h.state().panel.rows()[0].1, KeyStatus::Unknown(why) if why.contains("No answer")), "not a proven failure");
    assert!(h.query_by_label("Check").is_some(), "the row can be tried again");
    assert!(h.state().changes.is_empty());
    // Release the silent fake and close the worker instead of leaving a sleeping thread.
    control.replies.send(Ok(None)).unwrap();
    drop(h);
    control.exited.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    // A request made and then left with the panel closed still times out, and is still followed.
    let mut panel = KeysPanel { timeout_s: 1.0, ..KeysPanel::default() };
    let fal = Account::new("fal").unwrap();
    panel.track(9, &fal, Op::Status);
    panel.tick(0.5);
    assert!(panel.is_waiting());
    panel.tick(2.0);
    assert!(!panel.is_waiting() && panel.is_following());
    assert!(matches!(&panel.rows()[0].1, KeyStatus::Unknown(why) if why.contains("No answer")));
    // Its late answer is still applied: a key is stored, so the account is listed again.
    let late = nodecraft_engine::secrets::KeyOutcome {
        id: 9,
        account: "fal".into(),
        operation: nodecraft_engine::secrets::KeyOperation::Status,
        result: Ok(serde_json::json!({"stored": true, "masked": "…"})),
    };
    let (changes, others) = panel.receive(vec![late]);
    assert!(others.is_empty());
    assert_eq!(changes, [AccountChange::Found(fal)]);
    assert_eq!(panel.rows()[0].1, KeyStatus::Stored("…".into()));
    assert!(!panel.is_following());
}

/// Runs `keys.test` checks on a tokio runtime, as the app's runs do.
struct TestRuntime(tokio::runtime::Runtime);
impl nodecraft_exec::driver::Runtime for TestRuntime {
    fn spawn(&self, task: nodecraft_exec::driver::TaskFuture<()>) {
        self.0.spawn(task);
    }
    fn sleep(&self, _: u64) -> nodecraft_exec::driver::TaskFuture<()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

/// A provider fixture: OpenAI accepts the key, Anthropic rejects it. No real network.
struct Providers;
impl nodecraft_providers::http::Http for Providers {
    fn send(
        &self,
        _: nodecraft_providers::http::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::http::HttpResponse, nodecraft_providers::ProviderError>> {
        panic!("key checks never follow redirects")
    }
    fn send_no_redirect(
        &self,
        request: nodecraft_providers::http::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::http::HttpResponse, nodecraft_providers::ProviderError>> {
        let status = if request.url.starts_with("https://api.openai.com/") { 200 } else { 401 };
        Box::pin(async move { Ok(nodecraft_providers::http::HttpResponse { status, headers: vec![], body: b"{}".to_vec() }) })
    }
}

#[test]
fn test_asks_the_provider_and_shows_works_rejected_or_unverifiable() {
    let store = Arc::new(MemoryStore::default());
    for account in ["openai", "anthropic", "fal"] {
        store.set(&Account::new(account).unwrap(), &nodecraft_engine::secrets::Secret::new(KEY).unwrap()).unwrap();
    }
    let runtime = TestRuntime(tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap());
    let host = nodecraft_engine::key_test::KeyTestHost { runtime: Arc::new(runtime), http: Arc::new(Providers) };
    let (worker, results) = KeyWorker::spawn_with_tests(store, Some(host)).unwrap();
    let mut services = Services::default();
    register_key_worker(&mut services, worker).unwrap();
    let mut state = worker_state(Arc::new(MemoryStore::default()), &["openai", "anthropic", "fal"]);
    state.session = session(services);
    state.results = Some(results);
    let mut h = harness_at_dt(state, Some(1.0 / 60.0));
    h.run_steps(2);
    for row in 0..3 {
        h.get_all_by_label("Test").nth(row).unwrap().click();
        h.run_steps(1);
        // The check answers on the worker (often within that frame); frames deliver it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while h.state().panel.is_waiting() {
            assert!(std::time::Instant::now() < deadline, "the check never answered");
            std::thread::sleep(std::time::Duration::from_millis(1));
            h.run_steps(1);
        }
        // A longer status widens the grid a frame later; let the buttons settle before the next click.
        h.run_steps(2);
    }
    let statuses: Vec<KeyStatus> = h.state().panel.rows().iter().map(|(_, status)| status.clone()).collect();
    assert_eq!(statuses[0], KeyStatus::Verified("…1234".into()), "a test keeps the stored key's tail");
    assert!(matches!(&statuses[1], KeyStatus::Failed(why) if why.starts_with("rejected: Anthropic")), "{statuses:?}");
    assert!(matches!(&statuses[2], KeyStatus::Unverifiable(masked, _) if masked == "…1234"), "{statuses:?}");
    assert!(h.query_by_label("Stored …1234, works: the provider accepted it").is_some());
    assert!(h.query_by_label_contains("Stored …1234, can't verify").is_some());
    assert!(h.query_by_label_contains("abcdefghij").is_none(), "the key is never shown");
    assert!(h.state().changes.is_empty(), "a test changes no remembered account");
}

#[test]
fn test_without_a_check_host_says_it_is_unsupported() {
    let store = Arc::new(MemoryStore::default());
    store.set(&Account::new("openai").unwrap(), &nodecraft_engine::secrets::Secret::new(KEY).unwrap()).unwrap();
    let mut h = harness(sync_state(store, &["openai"]));
    h.run();
    h.get_by_label("Test").click();
    h.run();
    assert!(h.state().panel.notice.as_deref().is_some_and(|n| n.contains("desktop app")), "{:?}", h.state().panel.notice);
    assert!(!h.state().panel.is_waiting());
}

#[test]
#[ignore = "manual public-safe key panel capture; NODECRAFT_AZURE_KEYS_EVIDENCE"]
fn azure45_render_key_setup_evidence() {
    let path = std::path::PathBuf::from(std::env::var_os("NODECRAFT_AZURE_KEYS_EVIDENCE").unwrap());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1050.0, 620.0)).with_step_dt(1.0 / 60.0).with_max_steps(120).build_ui_state(
        |ui, s: &mut State| {
            s.panel.show(ui.ctx(), &mut s.session, &s.settings, crate::theme::ThemeKind::Dark.tokens());
        },
        sync_state(Arc::new(MemoryStore::default()), &[]),
    );
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h.render().unwrap().save(path).unwrap();
}

use super::fake::{Call, Keychain};

fn denied() -> nodecraft_engine::secrets::SecretError {
    nodecraft_engine::secrets::SecretError::Denied("locked".into())
}

/// Picks the `preset` account, types the key and presses Save.
fn save_in_ui(h: &mut Harness<'static, State>, preset: &str) {
    h.get_by_label(preset).click();
    h.run_steps(2);
    save_again_in_ui(h);
}

/// Types the key for the account already picked and presses Save.
fn save_again_in_ui(h: &mut Harness<'static, State>) {
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run_steps(2);
    h.event(egui::Event::Text(KEY.into()));
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
}

/// Steps past a 0.2 s keychain timeout at 60 Hz.
fn time_out(h: &mut Harness<'static, State>) {
    for _ in 0..30 {
        h.step();
    }
}

#[test]
fn a_save_answered_after_the_timeout_is_remembered_even_with_the_panel_closed() {
    let (store, keychain) = Keychain::new();
    let openai = Account::new("openai").unwrap();
    let mut state = worker_state(store, &[]);
    state.panel.timeout_s = 0.2;
    let mut h = harness_at_dt(state, Some(1.0 / 60.0));
    h.run_steps(2);
    save_in_ui(&mut h, "openai");
    keychain.entered(Call::Set);
    time_out(&mut h);
    assert!(!h.state().panel.is_waiting() && h.state().panel.is_following());
    assert!(matches!(&h.state().panel.rows()[0].1, KeyStatus::Unknown(why) if why.contains("may still be saved")), "{:?}", h.state().panel.rows());
    assert!(h.query_by_label_contains("may still be saved").is_some(), "the unknown outcome is on screen");
    assert!(h.state().changes.is_empty(), "nothing is remembered before the keychain says so");
    // The person closes the panel; then accepts the unlock prompt.
    h.state_mut().panel.open = false;
    h.run_steps(2);
    keychain.answer(Ok(()));
    deliver(&mut h, 1);
    assert_eq!(h.state().changes, [AccountChange::Remember(openai.clone())]);
    assert_eq!(h.state().settings.accounts(), std::slice::from_ref(&openai), "the late save is in settings");
    assert!(keychain.memory.get(&openai).unwrap().is_some());
    assert!(!h.state().panel.is_following());
    // Reopened, the row says what happened.
    h.state_mut().panel.open = true;
    h.run_steps(2);
    assert!(h.query_by_label("Stored …1234").is_some());
    assert!(h.query_by_label_contains("abcdefghij").is_none());
    // Saved and read back, the settings list the account for the next start.
    let saved: KeySettings = serde_json::from_value(serde_json::to_value(&h.state().settings).unwrap()).unwrap();
    let mut fresh = KeysPanel::default();
    fresh.load(&saved);
    assert_eq!(fresh.rows(), &[(openai, KeyStatus::Unchecked)]);
    drop(h);
    keychain.closed();
}

#[test]
fn a_removal_answered_after_the_timeout_is_forgotten() {
    let (store, keychain) = Keychain::new();
    let openai = Account::new("openai").unwrap();
    keychain.memory.set(&openai, &nodecraft_engine::secrets::Secret::new(KEY).unwrap()).unwrap();
    let mut state = worker_state(store, &["openai"]);
    state.panel.timeout_s = 0.2;
    let mut h = harness_at_dt(state, Some(1.0 / 60.0));
    h.run_steps(2);
    h.get_by_label("Remove").click();
    h.run_steps(2);
    h.get_by_label("Confirm remove").click();
    h.run_steps(2);
    keychain.entered(Call::Delete);
    time_out(&mut h);
    assert!(matches!(&h.state().panel.rows()[0].1, KeyStatus::Unknown(why) if why.contains("may still be removed")));
    assert_eq!(h.state().settings.accounts(), std::slice::from_ref(&openai), "still listed until the keychain answers");
    keychain.answer(Ok(()));
    deliver(&mut h, 1);
    assert_eq!(h.state().changes, [AccountChange::Forget(openai.clone())]);
    assert!(h.state().settings.accounts().is_empty());
    assert!(h.state().panel.rows().is_empty());
    assert!(keychain.memory.get(&openai).unwrap().is_none());
    drop(h);
    keychain.closed();
}

#[test]
fn an_older_late_answer_never_overwrites_a_newer_request() {
    let (store, keychain) = Keychain::new();
    let openai = Account::new("openai").unwrap();
    let mut state = worker_state(store, &[]);
    state.panel.timeout_s = 0.2;
    let mut h = harness_at_dt(state, Some(1.0 / 60.0));
    h.run_steps(2);
    save_in_ui(&mut h, "openai");
    keychain.entered(Call::Set);
    time_out(&mut h);
    // Retry while the first save is still with the keychain: the row follows the retry.
    save_again_in_ui(&mut h);
    assert_eq!(h.state().panel.rows()[0].1, KeyStatus::Waiting);
    keychain.answer(Ok(()));
    deliver(&mut h, 1);
    assert_eq!(h.state().panel.rows()[0].1, KeyStatus::Waiting, "the first save's answer doesn't overwrite the retry's row");
    assert_eq!(h.state().changes, [AccountChange::Remember(openai.clone())], "but its key is stored, so it is remembered");
    keychain.entered(Call::Set);
    keychain.answer(Err(denied()));
    deliver(&mut h, 1);
    assert!(matches!(&h.state().panel.rows()[0].1, KeyStatus::Failed(_)), "the retry's own answer is shown");
    assert_eq!(h.state().settings.accounts(), std::slice::from_ref(&openai), "the first key is still in the keychain");
    assert!(!h.state().panel.is_following());
    drop(h);
    keychain.closed();
}

#[test]
fn new_accounts_being_saved_hold_a_place_in_the_remembered_list() {
    let (store, keychain) = Keychain::new();
    let listed: Vec<String> = (0..KeySettings::MAX_ACCOUNTS - 1).map(|i| format!("p{i}")).collect();
    let mut s = worker_state(store, &listed.iter().map(String::as_str).collect::<Vec<_>>());
    let (openai, fal) = (Account::new("openai").unwrap(), Account::new("fal").unwrap());
    // One place left: openai's save takes it while the keychain is busy.
    assert_eq!(s.panel.save(&mut s.session, &s.settings, "openai", KEY.into()), None);
    keychain.entered(Call::Set);
    assert!(s.panel.notice.is_none());
    // Another new account is refused before anything is written.
    s.panel.save(&mut s.session, &s.settings, "fal", KEY.into());
    assert!(s.panel.notice.as_deref().is_some_and(|n| n.contains("at most 64 accounts")), "{:?}", s.panel.notice);
    assert!(!s.panel.rows().iter().any(|(a, _)| *a == fal));
    assert!(!s.panel.notice.as_deref().unwrap_or_default().contains("abcdefghij"));
    // Replacing a listed key, and retrying the reserved account, are allowed.
    s.panel.save(&mut s.session, &s.settings, "p0", KEY.into());
    s.panel.save(&mut s.session, &s.settings, "openai", KEY.into());
    assert!(s.panel.notice.is_none());
    assert_eq!(s.panel.pending.len(), 3);
    // The first save fails, but the retry still holds the place.
    keychain.answer(Err(denied()));
    s.settle(1);
    s.panel.save(&mut s.session, &s.settings, "fal", KEY.into());
    assert!(s.panel.notice.is_some(), "the retry still reserves the place");
    keychain.entered(Call::Set);
    keychain.answer(Ok(()));
    s.settle(1);
    // The retry fails too: its place is released.
    keychain.entered(Call::Set);
    keychain.answer(Err(denied()));
    s.settle(1);
    assert!(!s.panel.is_following());
    assert!(!s.settings.accounts().contains(&openai));
    s.panel.save(&mut s.session, &s.settings, "fal", KEY.into());
    assert!(s.panel.notice.is_none(), "{:?}", s.panel.notice);
    keychain.entered(Call::Set);
    keychain.answer(Ok(()));
    s.settle(1);
    assert_eq!(s.settings.accounts().len(), KeySettings::MAX_ACCOUNTS, "exactly full, and fal is listed");
    assert!(s.settings.accounts().contains(&fal));
    // Full: a new account is refused and the keychain isn't asked; a replacement still works.
    s.panel.save(&mut s.session, &s.settings, "replicate", KEY.into());
    assert!(s.panel.notice.is_some());
    assert!(keychain.idle(), "nothing was written");
    s.panel.save(&mut s.session, &s.settings, "p5", KEY.into());
    assert!(s.panel.notice.is_none());
    keychain.entered(Call::Set);
    keychain.answer(Ok(()));
    s.settle(1);
    assert!(s.changes.iter().all(|c| !matches!(c, AccountChange::Remember(a) if a.as_str() == "replicate")));
    drop(s);
    keychain.closed();
}

#[test]
fn a_full_list_refuses_a_new_account_on_screen_before_writing() {
    // One over the limit (a hand-edited file): the surplus isn't listed and nothing new fits.
    let listed: Vec<String> = (0..=KeySettings::MAX_ACCOUNTS).map(|i| format!("p{i}")).collect();
    let store = Arc::new(MemoryStore::default());
    let state = sync_state(Arc::clone(&store), &listed.iter().map(String::as_str).collect::<Vec<_>>());
    let mut h = Harness::builder().with_size(egui::Vec2::new(900.0, 2600.0)).build_ui_state(
        |ui, s: &mut State| {
            let changes = s.panel.show(ui.ctx(), &mut s.session, &s.settings, crate::theme::ThemeKind::Dark.tokens());
            s.record(changes);
        },
        state,
    );
    h.run();
    assert_eq!(h.state().panel.rows().len(), KeySettings::MAX_ACCOUNTS);
    h.get_by_label("openai").click();
    h.run();
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    assert!(h.query_by_label_contains("at most 64 accounts").is_some(), "the refusal is on screen");
    assert!(store.get(&Account::new("openai").unwrap()).unwrap().is_none(), "no key was written");
    assert!(h.state().panel.key.is_empty(), "the typed key is cleared anyway");
    assert!(h.state().changes.is_empty());
    // Replacing a listed account's key works.
    h.state_mut().panel.account = "p3".into();
    h.run();
    type_into(&mut h, "Key", KEY);
    h.get_by_label("Save").click();
    h.run();
    assert!(store.get(&Account::new("p3").unwrap()).unwrap().is_some());
    assert_eq!(h.state().changes, [AccountChange::Remember(Account::new("p3").unwrap())]);
}

#[test]
fn rows_for_accounts_that_are_not_remembered_stay_bounded() {
    let settings = KeySettings { accounts: vec!["openai".into(), "fal".into()] };
    let mut panel = KeysPanel::default();
    panel.load(&settings);
    let busy = Account::new("busy").unwrap();
    panel.track(1, &busy, Op::Set);
    for i in 0..100 {
        panel.set_status(&Account::new(&format!("x{i}")).unwrap(), KeyStatus::Failed("denied".into()));
    }
    panel.prune(&settings);
    assert_eq!(panel.rows().len(), KeySettings::MAX_ACCOUNTS);
    for kept in ["openai", "fal", "busy", "x99"] {
        assert!(panel.rows().iter().any(|(a, _)| a.as_str() == kept), "{kept} is kept");
    }
    assert!(!panel.rows().iter().any(|(a, _)| a.as_str() == "x0"), "the oldest unlisted rows go first");
    assert!(panel.latest.len() <= panel.rows().len());
}

#[test]
fn requests_beyond_the_follow_limit_are_refused_before_dispatch() {
    let (store, keychain) = Keychain::new();
    let mut s = worker_state(store, &["openai"]);
    let openai = Account::new("openai").unwrap();
    for id in 0..MAX_PENDING as u64 {
        panel_track_overdue(&mut s.panel, 10_000 + id, &openai);
    }
    s.panel.save(&mut s.session, &s.settings, "openai", KEY.into());
    assert!(s.panel.notice.as_deref().is_some_and(|n| n.contains("Too many")), "{:?}", s.panel.notice);
    assert!(keychain.idle());
    drop(s);
    keychain.closed();
}

fn panel_track_overdue(panel: &mut KeysPanel, id: u64, account: &Account) {
    panel.track(id, account, Op::Status);
    if let Some(request) = panel.pending.get_mut(&id) {
        request.overdue = true;
    }
}
