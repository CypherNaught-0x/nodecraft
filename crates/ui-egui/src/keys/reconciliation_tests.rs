#![allow(clippy::unwrap_used)] // Native-only controlled fixture helpers; never product code.
use super::*;
use nodecraft_engine::commands::Services;
use nodecraft_engine::secrets::{KEY_SERVICE, KeyOutcome, KeyResults, KeyWorker, MemoryStore, Secret, SecretError, SecretStore, register_key_worker};
use nodecraft_graph::Project;
use nodecraft_types::ProjectId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
const FAKE_KEY: &str = "fixture-key-1234";

struct Controlled {
    memory: Arc<MemoryStore>,
    entered: mpsc::Sender<(Op, Account)>,
    replies: Mutex<mpsc::Receiver<Result<(), SecretError>>>,
    exited: mpsc::Sender<()>,
}
impl Controlled {
    fn gate(&self, op: Op, account: &Account) -> Result<(), SecretError> {
        self.entered.send((op, account.clone())).map_err(|_| SecretError::Failed("fixture closed".into()))?;
        self.replies.lock().unwrap().recv().map_err(|_| SecretError::Failed("fixture closed".into()))?
    }
}
impl SecretStore for Controlled {
    fn get(&self, a: &Account) -> Result<Option<Secret>, SecretError> {
        self.gate(Op::Status, a)?;
        self.memory.get(a)
    }
    fn set(&self, a: &Account, s: &Secret) -> Result<(), SecretError> {
        self.gate(Op::Set, a)?;
        self.memory.set(a, s)
    }
    fn delete(&self, a: &Account) -> Result<bool, SecretError> {
        self.gate(Op::Remove, a)?;
        self.memory.delete(a)
    }
}
impl Drop for Controlled {
    fn drop(&mut self) {
        let _ = self.exited.send(());
    }
}
struct Fixture {
    session: Option<Session>,
    results: Option<KeyResults>,
    memory: Arc<MemoryStore>,
    entered: mpsc::Receiver<(Op, Account)>,
    replies: Option<mpsc::Sender<Result<(), SecretError>>>,
    exited: mpsc::Receiver<()>,
    dispatches: Arc<AtomicUsize>,
}
impl Fixture {
    fn new() -> Self {
        let memory = Arc::new(MemoryStore::default());
        let (entered, entering) = mpsc::channel();
        let (replies, replying) = mpsc::channel();
        let (exited, exiting) = mpsc::channel();
        let (worker, results) = KeyWorker::spawn(Arc::new(Controlled { memory: memory.clone(), entered, replies: Mutex::new(replying), exited })).unwrap();
        let mut inner = Services::default();
        register_key_worker(&mut inner, worker).unwrap();
        let mut inner = Session::new(Project::new(ProjectId::from_bytes([2; 16]), "Worker").unwrap()).unwrap().with_services(inner);
        let dispatches = Arc::new(AtomicUsize::new(0));
        let count = dispatches.clone();
        let mut services = Services::default();
        services
            .register(KEY_SERVICE, move |request: &Value| {
                count.fetch_add(1, Ordering::SeqCst);
                let command = match request.get("op").and_then(Value::as_str) {
                    Some("set") => "keys.set",
                    Some("remove") => "keys.remove",
                    _ => "keys.status",
                };
                let mut params = request.clone();
                params.as_object_mut().unwrap().remove("op");
                inner.dispatch(command, &params)
            })
            .unwrap();
        let session = Session::new(Project::new(ProjectId::from_bytes([1; 16]), "Keys240").unwrap()).unwrap().with_services(services);
        Self { session: Some(session), results: Some(results), memory, entered: entering, replies: Some(replies), exited: exiting, dispatches }
    }
    fn send(&mut self, panel: &mut KeysPanel, op: Op, a: &Account) -> Option<AccountChange> {
        let params = match op {
            Op::Set => json!({"account":a.as_str(),"key":FAKE_KEY}),
            _ => json!({"account":a.as_str()}),
        };
        panel.send(self.session.as_mut().unwrap(), op, a, params)
    }
    fn entered(&self, op: Op, a: &Account) {
        assert_eq!(self.entered.recv_timeout(Duration::from_secs(10)).unwrap(), (op, a.clone()));
    }
    fn complete(&self, result: Result<(), SecretError>) -> KeyOutcome {
        self.replies.as_ref().unwrap().send(result).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let mut outcomes = self.results.as_ref().unwrap().drain();
            if !outcomes.is_empty() {
                assert_eq!(outcomes.len(), 1);
                return outcomes.remove(0);
            }
            assert!(std::time::Instant::now() < deadline, "worker result never arrived");
            std::thread::yield_now();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.session.take();
        self.results.take();
        self.replies.take();
        let stopped = self.exited.recv_timeout(Duration::from_secs(10));
        if !std::thread::panicking() {
            assert!(stopped.is_ok(), "fixture worker did not exit");
        }
    }
}
fn account(name: &str) -> Account {
    Account::new(name).unwrap()
}

#[test]
fn late_set_and_remove_still_reconcile_after_timeout_and_close() {
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings::default());
    let a = account("openai");
    f.send(&mut panel, Op::Set, &a);
    f.entered(Op::Set, &a);
    panel.tick(KEYCHAIN_TIMEOUT_S + 1.0);
    panel.open = false;
    let outcome = f.complete(Ok(()));
    let (changes, others) = panel.receive(vec![outcome]);
    assert!(others.is_empty());
    assert_eq!(changes, vec![AccountChange::Remember(a.clone())]);
    assert!(f.memory.get(&a).unwrap().is_some());
    f.send(&mut panel, Op::Remove, &a);
    f.entered(Op::Remove, &a);
    panel.tick(2.0 * (KEYCHAIN_TIMEOUT_S + 1.0));
    let (changes, others) = panel.receive(vec![f.complete(Ok(()))]);
    assert!(others.is_empty());
    assert_eq!(changes, vec![AccountChange::Forget(a.clone())]);
    assert!(f.memory.get(&a).unwrap().is_none());
}

#[test]
fn a_sixty_fifth_new_account_is_refused_before_dispatch() {
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings { accounts: (0..KeySettings::MAX_ACCOUNTS).map(|n| format!("account{n}")).collect() });
    f.send(&mut panel, Op::Set, &account("one-too-many"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), 0, "no accepted keychain write without remembered capacity");
}

#[test]
fn successful_order_survives_reordered_delivery_failed_newer_and_forged_results() {
    let mut f = Fixture::new();
    let a = account("openai");
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings::default());
    f.send(&mut panel, Op::Set, &a);
    f.entered(Op::Set, &a);
    panel.tick(KEYCHAIN_TIMEOUT_S + 1.0);
    f.send(&mut panel, Op::Remove, &a);
    let stored = f.complete(Ok(()));
    f.entered(Op::Remove, &a);
    let failed = f.complete(Err(SecretError::Denied("fixture refusal".into())));
    assert!(panel.receive(vec![failed]).0.is_empty());
    let display = panel.rows().to_vec();
    assert_eq!(panel.receive(vec![stored.clone()]).0, vec![AccountChange::Remember(a.clone())]);
    assert_eq!(panel.rows(), display, "older success must not replace newer failed row");
    assert!(panel.ledger.known.contains(&a));
    assert_eq!(panel.receive(vec![stored]).1.len(), 1, "duplicate delivery cannot mutate again");
    // A newer successful Remove is authoritative even when delivered before the older Set.
    f.send(&mut panel, Op::Set, &a);
    f.entered(Op::Set, &a);
    let stored = f.complete(Ok(()));
    f.send(&mut panel, Op::Remove, &a);
    f.entered(Op::Remove, &a);
    let removed = f.complete(Ok(()));
    assert_eq!(panel.receive(vec![removed]).0, vec![AccountChange::Forget(a.clone())]);
    assert_eq!(panel.ledger.requests.len(), 1, "older accepted correlation retains the successful tombstone");
    for n in 0..ledger::MAX_ROWS + 1 {
        let other = account(&format!("prune{n}"));
        f.send(&mut panel, Op::Status, &other);
        f.entered(Op::Status, &other);
        panel.receive(vec![f.complete(Ok(()))]);
    }
    assert!(panel.receive(vec![stored]).0.is_empty());
    assert!(!panel.ledger.known.contains(&a));
    assert!(!panel.rows().iter().any(|(row, _)| row == &a));
    // Matching id alone is insufficient; malformed operation-specific success remains unknown.
    f.send(&mut panel, Op::Set, &a);
    f.entered(Op::Set, &a);
    let genuine = f.complete(Ok(()));
    let mut forged = genuine.clone();
    forged.account = "different".into();
    assert_eq!(panel.receive(vec![forged]).1.len(), 1);
    assert!(panel.ledger.known.is_empty());
    let mut malformed = genuine.clone();
    malformed.result = Ok(json!({"stored":"yes"}));
    assert!(panel.receive(vec![malformed]).0.is_empty());
    assert!(panel.has_unresolved());
    assert!(!panel.is_waiting());
    assert_eq!(panel.rows().iter().find(|(row, _)| row == &a).unwrap().1, KeyStatus::Unknown);
    assert_eq!(panel.receive(vec![genuine]).0, vec![AccountChange::Remember(a)]);
}

#[test]
fn pending_distinct_additions_reserve_exactly_sixty_four_slots_through_unknown() {
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings::default());
    for n in 0..64 {
        f.send(&mut panel, Op::Set, &account(&format!("account{n}")));
    }
    f.entered(Op::Set, &account("account0"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), 64);
    panel.tick(KEYCHAIN_TIMEOUT_S + 1.0);
    assert!(!panel.is_waiting() && panel.has_unresolved());
    f.send(&mut panel, Op::Set, &account("extra"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), 64);
    // Replacement consumes no new account slot, even with all64pending.
    f.send(&mut panel, Op::Set, &account("account0"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), 65);
    panel.receive(vec![f.complete(Err(SecretError::Denied("fixture refusal".into())))]);
    f.entered(Op::Set, &account("account1"));
    f.send(&mut panel, Op::Set, &account("extra"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), 65, "later replacement still holds account0slot");
    panel.receive(vec![f.complete(Err(SecretError::Denied("fixture refusal".into())))]);
    f.send(&mut panel, Op::Set, &account("extra"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), 66, "failed account1addition released its slot");
    for n in 2..64 {
        f.entered(Op::Set, &account(&format!("account{n}")));
        panel.receive(vec![f.complete(Ok(()))]);
    }
    f.entered(Op::Set, &account("account0"));
    panel.receive(vec![f.complete(Ok(()))]);
    f.entered(Op::Set, &account("extra"));
    panel.receive(vec![f.complete(Ok(()))]);
    assert_eq!(panel.ledger.known.len(), 64);
    assert!(!panel.has_unresolved());
}

#[test]
fn successful_remove_does_not_release_a_later_set_reservation() {
    let settings = KeySettings { accounts: (0..64).map(|n| format!("account{n}")).collect() };
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&settings);
    let a = account("account0");
    f.memory.set(&a, &Secret::new(FAKE_KEY).unwrap()).unwrap();
    f.send(&mut panel, Op::Remove, &a);
    f.entered(Op::Remove, &a);
    panel.tick(KEYCHAIN_TIMEOUT_S + 1.0);
    f.send(&mut panel, Op::Set, &a);
    let removed = f.complete(Ok(()));
    f.entered(Op::Set, &a);
    assert_eq!(panel.receive(vec![removed]).0, vec![AccountChange::Forget(a.clone())]);
    let dispatches = f.dispatches.load(Ordering::SeqCst);
    f.send(&mut panel, Op::Set, &account("extra"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), dispatches);
    panel.receive(vec![f.complete(Err(SecretError::Denied("fixture refusal".into())))]);
    f.send(&mut panel, Op::Set, &account("extra"));
    assert_eq!(f.dispatches.load(Ordering::SeqCst), dispatches + 1);
    f.entered(Op::Set, &account("extra"));
    panel.receive(vec![f.complete(Ok(()))]);
}

#[test]
fn retained_request_and_row_limits_refuse_without_losing_correlation() {
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings::default());
    let a = account("openai");
    let mut delivered = Vec::new();
    for _ in 0..ledger::MAX_REQUESTS {
        f.send(&mut panel, Op::Status, &a);
        f.entered(Op::Status, &a);
        delivered.push(f.complete(Err(SecretError::Denied("fixture refusal".into()))));
    }
    panel.tick(KEYCHAIN_TIMEOUT_S + 1.0);
    let accepted = f.dispatches.load(Ordering::SeqCst);
    for op in [Op::Status, Op::Set, Op::Remove] {
        f.send(&mut panel, op, &a);
    }
    assert_eq!(f.dispatches.load(Ordering::SeqCst), accepted);
    assert_eq!(panel.ledger.requests.len(), ledger::MAX_REQUESTS);
    assert!(panel.receive(delivered).1.is_empty());
    assert!(!panel.has_unresolved());
    // Definitive transient rows may be evicted; accepted ownership never is.
    for n in 0..ledger::MAX_ROWS + 20 {
        let a = account(&format!("transient{n}"));
        f.send(&mut panel, Op::Status, &a);
        f.entered(Op::Status, &a);
        panel.receive(vec![f.complete(Err(SecretError::Denied("fixture refusal".into())))]);
    }
    assert!(panel.rows().len() <= ledger::MAX_ROWS);
    assert!(!panel.has_unresolved());
}

#[test]
fn immediate_results_update_capacity_before_host_persistence_and_refusal_is_truthful() {
    let memory = Arc::new(MemoryStore::default());
    let mut services = Services::default();
    nodecraft_engine::secrets::register_key_service(&mut services, memory.clone()).unwrap();
    let mut session = Session::new(Project::new(ProjectId::from_bytes([3; 16]), "Immediate").unwrap()).unwrap().with_services(services);
    let settings = KeySettings { accounts: (0..63).map(|n| format!("account{n}")).collect() };
    let mut panel = KeysPanel::default();
    panel.load(&settings);
    let a = account("last-slot");
    let changes = panel.send(&mut session, Op::Set, &a, json!({"account":a.as_str(),"key":FAKE_KEY}));
    assert_eq!(changes, Some(AccountChange::Remember(a.clone())));
    let b = account("extra");
    assert!(panel.send(&mut session, Op::Set, &b, json!({"account":b.as_str(),"key":FAKE_KEY})).is_none());
    assert!(memory.get(&b).unwrap().is_none());
    assert_eq!(settings.accounts().len(), 63, "host has not applied returned change yet");
    let full = KeySettings { accounts: (0..64).map(|n| format!("account{n}")).collect() };
    panel.remember_refused(&full, &a);
    assert!(matches!(panel.rows().iter().find(|(row, _)| row == &a).unwrap().1, KeyStatus::Failed(_)));
    // An older success's host refusal must not overwrite a newer waiting row or lose reservation.
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings::default());
    f.send(&mut panel, Op::Set, &a);
    f.entered(Op::Set, &a);
    let old = f.complete(Ok(()));
    panel.tick(KEYCHAIN_TIMEOUT_S + 1.0);
    f.send(&mut panel, Op::Set, &a);
    f.entered(Op::Set, &a);
    panel.receive(vec![old]);
    panel.remember_refused(&full, &a);
    assert_eq!(panel.rows()[0].1, KeyStatus::Waiting);
    assert!(panel.has_unresolved());
    panel.receive(vec![f.complete(Err(SecretError::Denied("fixture refusal".into())))]);
}

struct TestDir(std::path::PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("nodecraft-key240-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn late_mutations_survive_settings_save_reload_and_panel_reopen() {
    use nodecraft_engine::settings::{Location, open};
    let dir = TestDir::new();
    let (mut settings_store, _) = open(Location::Dir(dir.0.clone()));
    let mut settings = settings_store.get().keys.clone();
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&settings);
    let a = account("openai");
    for op in [Op::Set, Op::Remove] {
        f.send(&mut panel, op, &a);
        f.entered(op, &a);
        panel.tick(panel.now + KEYCHAIN_TIMEOUT_S + 1.0);
        panel.open = false;
        assert!(panel.has_unresolved() && !panel.is_waiting());
        let (changes, others) = panel.receive(vec![f.complete(Ok(()))]);
        assert!(others.is_empty());
        for change in changes {
            match change {
                AccountChange::Remember(a) => settings.try_remember(&a).unwrap(),
                AccountChange::Forget(a) => settings.forget(&a),
            }
        }
        settings_store.update(|saved| saved.keys = settings.clone());
        settings_store.flush().unwrap();
        let (reloaded, _) = open(Location::Dir(dir.0.clone()));
        assert_eq!(reloaded.get().keys, settings);
        panel.open = true;
        assert_eq!(settings.accounts().contains(&a), op == Op::Set);
        if op == Op::Set {
            assert!(matches!(panel.rows()[0].1, KeyStatus::Stored(_)));
            panel.settings_failed();
            assert!(matches!(panel.rows()[0].1, KeyStatus::Stored(_)));
            panel.settings_saved();
        }
    }
}

#[test]
fn every_operation_requires_a_definitive_shape_before_releasing_ownership() {
    let mut f = Fixture::new();
    let mut panel = KeysPanel::default();
    panel.load(&KeySettings::default());
    let a = account("openai");
    for op in [Op::Set, Op::Remove, Op::Status] {
        f.send(&mut panel, op, &a);
        f.entered(op, &a);
        let genuine = f.complete(Ok(()));
        let mut invalid = genuine.clone();
        invalid.result = Ok(json!({}));
        assert!(panel.receive(vec![invalid]).0.is_empty());
        assert!(panel.has_unresolved());
        assert_eq!(panel.rows().iter().find(|(row, _)| row == &a).unwrap().1, KeyStatus::Unknown);
        let mut wrong = genuine.clone();
        wrong.id = wrong.id.saturating_add(1000);
        assert_eq!(panel.receive(vec![wrong]).1.len(), 1);
        assert!(panel.receive(vec![genuine]).1.is_empty());
        assert!(!panel.has_unresolved());
    }
}

struct UiState {
    panel: KeysPanel,
    fixture: Fixture,
    settings: KeySettings,
    changes: Vec<AccountChange>,
}
fn ui_harness(settings: KeySettings) -> egui_kittest::Harness<'static, UiState> {
    let state = UiState { panel: KeysPanel { open: true, timeout_s: 0.2, ..KeysPanel::default() }, fixture: Fixture::new(), settings, changes: vec![] };
    egui_kittest::Harness::builder().with_step_dt(1.0 / 60.0).with_size(egui::vec2(1000.0, 700.0)).build_ui_state(
        |ui, s: &mut UiState| {
            s.panel.tick(ui.input(|i| i.time));
            let (mut changes, others) = s.panel.receive(s.fixture.results.as_ref().unwrap().drain());
            assert!(others.is_empty());
            changes.extend(s.panel.show(ui.ctx(), s.fixture.session.as_mut().unwrap(), &s.settings, crate::theme::ThemeKind::Dark.tokens()));
            for change in &changes {
                match change {
                    AccountChange::Remember(a) => s.settings.try_remember(a).unwrap(),
                    AccountChange::Forget(a) => s.settings.forget(a),
                }
            }
            s.changes.extend(changes);
        },
        state,
    )
}

#[test]
fn controlled_unknown_ui_clears_the_key_and_reconciles_while_closed() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    let mut h = ui_harness(KeySettings::default());
    h.run_steps(2);
    assert_eq!(h.state().fixture.dispatches.load(Ordering::SeqCst), 0, "no startup reads");
    h.get_by_label("openai").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run_steps(2);
    h.event(egui::Event::Text(FAKE_KEY.into()));
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    h.state().fixture.entered(Op::Set, &account("openai"));
    assert!(h.state().panel.key.is_empty());
    h.run_steps(20);
    assert!(h.query_by_label_contains("outcome unknown").is_some());
    assert!(!h.state().panel.is_waiting() && h.state().panel.has_unresolved());
    assert!(h.query_by_label_contains("fixture-key").is_none());
    h.state_mut().panel.open = false;
    h.run_steps(2);
    h.state().fixture.replies.as_ref().unwrap().send(Ok(())).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while h.state().panel.has_unresolved() {
        assert!(std::time::Instant::now() < deadline);
        h.step();
        std::thread::yield_now();
    }
    assert_eq!(h.state().changes, vec![AccountChange::Remember(account("openai"))]);
    assert_eq!(h.state().settings.accounts(), vec![account("openai")]);
    h.state_mut().panel.open = true;
    h.run_steps(2);
    assert!(h.query_by_label("Stored …1234").is_some());
    assert!(h.state().panel.key.is_empty());
}

#[test]
fn capacity_refusal_through_scrolled_ui_clears_the_key_before_any_worker_call() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable;
    let mut h = ui_harness(KeySettings { accounts: (0..64).map(|n| format!("account{n}")).collect() });
    h.run_steps(3);
    h.hover_at(egui::pos2(300.0, 300.0));
    h.event(egui::Event::MouseWheel {
        phase: egui::TouchPhase::Move,
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -10000.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Account").click();
    h.run_steps(2);
    h.event(egui::Event::Text("extra".into()));
    h.run_steps(2);
    h.get_by_role_and_label(Role::PasswordInput, "Key").click();
    h.run_steps(2);
    h.event(egui::Event::Text(FAKE_KEY.into()));
    h.run_steps(2);
    assert_eq!(h.state().panel.account, "extra");
    assert_eq!(h.state().panel.key.len(), FAKE_KEY.len(), "password input really received typed bytes");
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert!(h.state().panel.key.is_empty());
    assert_eq!(h.state().fixture.dispatches.load(Ordering::SeqCst), 0);
    assert!(h.state().panel.notice.as_deref().is_some_and(|n| n.contains("At most 64")), "{:?}", h.state().panel.notice);
    assert!(h.query_by_label_contains("At most 64").is_some());
    assert!(h.query_by_label_contains("fixture-key").is_none());
    assert!(!h.state().panel.has_unresolved());
}
