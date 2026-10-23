//! The app's runs (#409): Generate waits for the card's provider to be set up, quotes, and the
//! run host drives the accepted run against the mock provider, shown on the card. The adapter
//! maker is a test one; nothing here reads the keychain or calls a real provider.
use super::runs::{Make, Runs};
use super::*;
use egui::accesskit::Role;
use egui_kittest::{
    Harness,
    kittest::{NodeT, Queryable},
};
use nodecraft_engine::secrets::{Account, KeyWorker, MemoryStore, Secret, SecretError, SecretStore, register_key_service, register_key_worker};
use nodecraft_engine::{
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    provider_setup::Readiness,
    run_cmds::RunCatalog,
};
use nodecraft_provider_testkit::mock::{MockFailure, MockModel, MockOutput, MockProvider};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

const C: CanvasId = CanvasId::from_bytes([91; 16]);
const N: NodeId = NodeId::from_bytes([92; 16]);

const MOCK_IMAGE: &str = r#"
[[model]]
id = "mock-square"
provider = "mock"
provider_model = "mock-image"
label = "Mock image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "mock-round"
provider = "mock"
provider_model = "mock-image"
label = "Mock round image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "mock-metered"
provider = "mock"
provider_model = "mock-image"
label = "Mock metered image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "by_usage", currency = "USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "other-writer"
provider = "other"
provider_model = "other-text"
label = "Other writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A maker that builds `mock` for the `mock` adapter.
fn making(mock: MockProvider) -> Make {
    Box::new(move |adapter, _| if adapter == "mock" { Ok(Arc::new(mock.clone())) } else { Err(Readiness::NotSetUp("not in this test".into())) })
}

fn image(model: MockModel) -> MockProvider {
    MockProvider::new("mock", [("mock-image".to_owned(), model)])
}

fn scene(make: Make) -> Harness<'static, NodeCraftApp> {
    scene_with(make, |_, _| {})
}

/// The scene, with `edit` applied to the project before the session opens it.
fn scene_with(make: Make, edit: impl FnOnce(&mut Project, &nodecraft_nodes::Registry)) -> Harness<'static, NodeCraftApp> {
    scene_in(MOCK_IMAGE, "mock-square", make, edit)
}

/// The card on `model`, from the `catalog` given.
fn scene_in(catalog: &str, model: &str, make: Make, edit: impl FnOnce(&mut Project, &nodecraft_nodes::Registry)) -> Harness<'static, NodeCraftApp> {
    scene_on(catalog, model, Runs::with_maker, make, edit)
}

/// [`scene_in`], its runs built by `build`.
fn scene_on(
    catalog: &str,
    model: &str,
    build: fn(&mut Services, Make) -> Result<Runs, nodecraft_types::Error>,
    make: Make,
    edit: impl FnOnce(&mut Project, &nodecraft_nodes::Registry),
) -> Harness<'static, NodeCraftApp> {
    scene_on_with_keys(catalog, model, build, make, edit, |_| (None, None))
}

fn scene_on_with_keys(
    catalog: &str,
    model: &str,
    build: fn(&mut Services, Make) -> Result<Runs, nodecraft_types::Error>,
    make: Make,
    edit: impl FnOnce(&mut Project, &nodecraft_nodes::Registry),
    keys: impl FnOnce(&mut Services) -> (Option<nodecraft_engine::secrets::KeyResults>, Option<Arc<key_changes::KeyChanges>>),
) -> Harness<'static, NodeCraftApp> {
    let catalog = nodecraft_catalog::Catalog::from_toml(catalog).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([93; 16]), "Runs").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let mut node = kinds
        .get(nodecraft_nodes::generation::KIND)
        .unwrap()
        .instantiate_instance(
            N,
            Default::default(),
            &[("model".into(), Value::Text(model.into())), ("prompt".into(), Value::Text("A red square".into()))].into(),
        )
        .unwrap();
    node.pos = nodecraft_graph::Position { x: 150.0, y: 120.0 };
    Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
    edit(&mut project, &kinds);
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut services = Services::default();
    let runs = build(&mut services, make).unwrap();
    let (key_results, key_changes) = keys(&mut services);
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    runs.install(&mut session);
    let mut app = NodeCraftApp::from_session(session, C).unwrap();
    app.runs = Some(runs);
    app.key_results = key_results;
    app.key_changes = key_changes;
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).with_max_steps(120).build_ui_state(
        |ui, app: &mut NodeCraftApp| {
            app.tick_keys(ui.ctx());
            app.show(ui);
        },
        app,
    );
    h.run();
    h
}

fn press(h: &mut Harness<'static, NodeCraftApp>, role: Role, label: &str) {
    let pos = h.get_by_role_and_label(role, label).rect().center();
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
}

/// Frames, as the window's loop runs them, until `done`.
fn frames_until(h: &mut Harness<'static, NodeCraftApp>, done: impl Fn(&NodeCraftApp) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let ctx = h.ctx.clone();
        h.state_mut().tick_runs(&ctx);
        h.run();
        if done(h.state()) {
            return;
        }
        assert!(Instant::now() < deadline, "never got there; notice: {:?}", h.state().notice);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn accept(h: &mut Harness<'static, NodeCraftApp>) {
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
}

fn node(app: &NodeCraftApp) -> &nodecraft_graph::Node {
    app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap()
}

fn idle(app: &NodeCraftApp) -> bool {
    app.runs.as_ref().is_some_and(|runs| !runs.is_busy())
}

#[test]
fn generate_sets_up_the_provider_first_then_quotes_and_records_the_run() {
    let mock = image(MockModel::new(MockOutput::Image));
    // The keychain may prompt: the maker answers only once the gate opens, and frames go on.
    let (open, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let inner = making(mock.clone());
    let mut h = scene(Box::new(move |adapter, settings| {
        let _ = gate.lock().unwrap().recv();
        inner(adapter, settings)
    }));
    press(&mut h, Role::Button, "Generate");
    assert!(h.state().run_confirm.is_none(), "no quote before the provider is set up");
    assert_eq!(h.state().notice.as_deref(), Some(super::runs::SETTING_UP));
    frames_until(&mut h, |_| true);
    assert!(h.state().run_confirm.is_none());
    open.send(()).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(h.state().notice, None);
    assert!(mock.requests().is_empty(), "the quote alone sends nothing");
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1, "one accepted run, one request");
    assert!(h.query_by_label("Run failed").is_none(), "notice: {:?}", h.state().notice);
}

#[test]
fn the_browsers_inline_maker_hands_the_provider_over_on_the_next_tick() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene_on(MOCK_IMAGE, "mock-square", Runs::with_inline_maker, making(mock.clone()), |_, _| {});
    press(&mut h, Role::Button, "Generate");
    assert!(h.state().run_confirm.is_none(), "the adapter is handed over on a tick, not inside the click");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(h.state().notice, None);
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1);
}

/// The scene, its card on the by-usage model, which its provider quotes at 0.3 USD.
fn metered() -> (Harness<'static, NodeCraftApp>, MockProvider) {
    let price = nodecraft_types::Money { currency: nodecraft_types::Currency::USD, micros: 300_000 };
    let mock = image(MockModel::new(MockOutput::Image)).with_quote("mock-image", Ok(price));
    let mut h = scene(making(mock.clone()));
    let session = &mut h.state_mut().session;
    let preview = session.dispatch("node.model_preview", &json!({"canvas": C, "nodes": [N], "model": "mock-metered"})).unwrap();
    let revision = session.revision();
    let set = json!({"canvas": C, "nodes": [N], "model": "mock-metered", "expected_revision": revision, "preview_binding": preview["binding"]});
    session.dispatch("node.set_model", &set).unwrap();
    h.run();
    (h, mock)
}

#[test]
fn a_by_usage_card_is_priced_by_its_providers_quote_before_it_runs() {
    let (mut h, mock) = metered();
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.as_ref().is_some_and(|confirm| !confirm.estimating()));
    assert_eq!(mock.quoted().len(), 1, "the provider was asked once");
    assert!(mock.requests().is_empty(), "asking for a price sends nothing");
    // The dialog grew with the provider's price: let it settle where it is shown.
    h.run();
    h.run();
    h.get_by_label_contains("priced by their provider's quote");
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    assert!(label.ends_with("for 0.3 USD"), "the quoted price is the whole price: {label}");
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn a_budget_refusing_an_unknown_price_is_checked_again_with_the_providers_quote() {
    let (mut h, mock) = metered();
    let usd = |micros| nodecraft_types::Money { currency: nodecraft_types::Currency::USD, micros };
    let budgets = nodecraft_exec::cost::Budgets { session: Some(usd(1_000_000)), ..Default::default() };
    h.state_mut().session.swap_resource(Some(nodecraft_engine::run_cmds::BudgetState { budgets, spent: Default::default() }));
    let request = json!({"canvas": C, "mode": "selected", "nodes": [N]});
    assert_eq!(h.state_mut().session.dispatch("run.quote", &request).unwrap()["allowed"], false, "a capped session refuses an unknown price");
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.as_ref().is_some_and(|confirm| !confirm.estimating()));
    assert_eq!(mock.quoted().len(), 1, "the provider is asked even though the run couldn't start yet");
    h.run();
    h.run();
    let label = h.get_by_label_contains("Generate for").accesskit_node().label().unwrap_or_default();
    assert!(label.ends_with("for 0.3 USD"), "the quoted price is within the cap: {label}");
}

/// A stand-in for Higgsfield's Soul ID trainer, by its catalog id, and a model that picks from
/// the account's Soul IDs, so the lists can be refreshed.
const SOUL_TRAINING: &str = r#"
[[model]]
id = "higgsfield.soul-id-trainer"
provider = "mock"
provider_model = "mock-image"
label = "Mock Soul ID trainer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "custom_reference_id"
label = "Soul ID"
control = { type = "text", multiline = false, max_bytes = 64 }
[model.parameter_contract]
version = { major = 1, minor = 0 }
namespace = "higgsfield.soul-id-trainer"
owner = "model"
[[model.parameter_contract.rules]]
param = "custom_reference_id"
options = { id = "higgsfield.soul_ids" }
[[model.parameter_contract.schema.params]]
name = "custom_reference_id"
label = "Soul ID"
control = { type = "text", multiline = false, max_bytes = 64 }
"#;

#[test]
fn a_recorded_soul_id_training_fetches_the_option_lists_again() {
    let mut h = scene_in(SOUL_TRAINING, "higgsfield.soul-id-trainer", making(image(MockModel::new(MockOutput::Image))), |_, _| {});
    let asked = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let seen = asked.clone();
    h.state_mut()
        .session
        .register_host_service(nodecraft_engine::option_fetch::OPTION_SERVICE, move |request| {
            seen.lock().unwrap().push(request.clone());
            Ok(serde_json::json!({"pending": 1}))
        })
        .unwrap();
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    assert!(asked.lock().unwrap().is_empty(), "nothing is fetched before the training ends");
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 1, "one fetch for the recorded training: {asked:?}");
    assert!(asked[0]["sources"].as_array().unwrap().iter().any(|source| source == "higgsfield.soul_ids"), "{asked:?}");
}

#[test]
fn a_running_card_says_so_until_its_run_ends() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mut h = scene(making(image(slow)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.card_status(C, N).is_some_and(|s| s.starts_with("Generating "))));
    // The mock reports its progress: the card shows it.
    h.get_by_label_contains("Generating ");
}

#[test]
fn a_failed_run_is_shown_on_its_card_with_the_reason() {
    let mut refusing = MockModel::new(MockOutput::Image);
    refusing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    let mut h = scene(making(image(refusing)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && app.runs.as_ref().is_some_and(|runs| runs.failure(C, N).is_some()));
    h.get_by_label("Run failed: input not supported");
    assert!(h.state().notice.as_deref().is_some_and(|n| n.starts_with("A run failed")), "{:?}", h.state().notice);
    assert!(node(h.state()).history.is_empty());
}

#[test]
fn a_provider_without_its_key_says_what_to_do_and_asks_again_next_time() {
    let asked = Arc::new(Mutex::new(0u32));
    let count = asked.clone();
    let mut h = scene(Box::new(move |_, _| {
        *count.lock().unwrap() += 1;
        Err(Readiness::NeedsKey { account: "mock".into() })
    }));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.notice.as_deref() != Some(super::runs::SETTING_UP));
    let notice = h.state().notice.clone().unwrap_or_default();
    assert!(notice.contains("needs an API key"), "{notice}");
    assert!(h.state().run_confirm.is_none(), "nothing is quoted for a provider that can't run");
    // The person adds the key; the next Generate reads it again rather than remembering the miss.
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.notice.as_deref() != Some(super::runs::SETTING_UP));
    assert_eq!(*asked.lock().unwrap(), 2);
}

/// A maker that answers once per message on the returned gate, counting what it built.
fn gated(mock: MockProvider) -> (Make, std::sync::mpsc::Sender<()>, Arc<Mutex<u32>>) {
    let (open, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let built = Arc::new(Mutex::new(0u32));
    let count = built.clone();
    let inner = making(mock);
    let make: Make = Box::new(move |adapter, settings| {
        let _ = gate.lock().unwrap().recv();
        *count.lock().unwrap() += 1;
        inner(adapter, settings)
    });
    (make, open, built)
}

const OLD_KEY: &str = "fixture-old-not-a-real-key";
const NEW_KEY: &str = "fixture-new-not-a-real-key";

type KeyReads = Arc<Mutex<Vec<Option<bool>>>>;

/// A refusing keychain/origin store: reads still work and rejected writes never change the key.
struct RejectWrites {
    inner: Arc<MemoryStore>,
    attempts: Arc<AtomicUsize>,
}

impl SecretStore for RejectWrites {
    fn get(&self, account: &Account) -> Result<Option<Secret>, SecretError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.inner.get(account)
    }

    fn set(&self, _: &Account, _: &Secret) -> Result<(), SecretError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(SecretError::Denied("allow NodeCraft access to the key store".into()))
    }

    fn delete(&self, _: &Account) -> Result<bool, SecretError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(SecretError::Denied("allow NodeCraft access to the key store".into()))
    }
}

fn key_scene() -> (Harness<'static, NodeCraftApp>, KeyReads) {
    key_scene_with_store(false).0
}

fn key_scene_with_store(reject: bool) -> ((Harness<'static, NodeCraftApp>, KeyReads), Arc<AtomicUsize>) {
    let store = Arc::new(MemoryStore::default());
    store.set(&Account::new("mock").unwrap(), &Secret::new(OLD_KEY).unwrap()).unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let worker_store: Arc<dyn SecretStore> =
        if reject { Arc::new(RejectWrites { inner: Arc::clone(&store), attempts: Arc::clone(&attempts) }) } else { store.clone() };
    let reads = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&reads);
    let maker_store = Arc::clone(&store);
    let make: Make = Box::new(move |_, _| {
        let key = maker_store.get(&Account::new("mock").unwrap()).map_err(|e| Readiness::NotSetUp(e.to_string()))?;
        observed.lock().unwrap().push(key.as_ref().map(|key| key.expose() == NEW_KEY));
        match key {
            Some(_) => Ok(Arc::new(image(MockModel::new(MockOutput::Image)))),
            None => Err(Readiness::NeedsKey { account: "mock".into() }),
        }
    });
    let h = scene_on_with_keys(
        MOCK_IMAGE,
        "mock-square",
        Runs::with_maker,
        make,
        |_, _| {},
        move |services| {
            let (worker, results) = KeyWorker::spawn(worker_store).unwrap();
            register_key_worker(services, worker).unwrap();
            (Some(results), None)
        },
    );
    ((h, reads), attempts)
}

#[test]
fn a_session_dispatched_key_set_rebuilds_the_next_run_with_the_new_key() {
    let (mut h, reads) = key_scene();
    generated(&mut h);
    assert_eq!(*reads.lock().unwrap(), [Some(false)]);
    let response = h.state_mut().session.dispatch("keys.set", &json!({"account": "mock", "key": NEW_KEY})).unwrap();
    assert!(response.get("pending").is_some());
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| !runs.has_cached_adapters()));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(*reads.lock().unwrap(), [Some(false), Some(true)]);
}

#[test]
fn a_session_dispatched_key_remove_cannot_reuse_the_old_adapter() {
    let (mut h, reads) = key_scene();
    generated(&mut h);
    let response = h.state_mut().session.dispatch("keys.remove", &json!({"account": "mock"})).unwrap();
    assert!(response.get("pending").is_some());
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| !runs.has_cached_adapters()));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.notice.as_deref().is_some_and(|notice| notice.contains("needs an API key")));
    assert_eq!(*reads.lock().unwrap(), [Some(false), None]);
    assert!(h.state().run_confirm.is_none());
}

fn failed_native_write_preserves_adapter(command: &str, params: serde_json::Value) {
    let ((mut h, reads), attempts) = key_scene_with_store(true);
    generated(&mut h);
    assert_eq!(*reads.lock().unwrap(), [Some(false)]);
    assert!(h.state_mut().session.dispatch(command, &params).unwrap().get("pending").is_some());
    // FIFO status completes after the failed write; observing its store read guarantees the
    // prior outcome has reached tick_keys without relying on thread scheduling or a delay.
    h.state_mut().session.dispatch("keys.status", &json!({"account": "mock"})).unwrap();
    frames_until(&mut h, |_| attempts.load(Ordering::SeqCst) >= 2);
    h.run();
    assert!(h.state().runs.as_ref().unwrap().has_cached_adapters(), "{command} failed");
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(*reads.lock().unwrap(), [Some(false)], "{command} must reuse the old adapter");
}

#[test]
fn failed_native_key_set_preserves_the_cached_adapter() {
    failed_native_write_preserves_adapter("keys.set", json!({"account": "mock", "key": NEW_KEY}));
}

#[test]
fn failed_native_key_remove_preserves_the_cached_adapter() {
    failed_native_write_preserves_adapter("keys.remove", json!({"account": "mock"}));
}

fn browser_key_scene(reject: bool) -> ((Harness<'static, NodeCraftApp>, KeyReads), Arc<AtomicUsize>) {
    let store = Arc::new(MemoryStore::default());
    store.set(&Account::new("mock").unwrap(), &Secret::new(OLD_KEY).unwrap()).unwrap();
    let attempts = Arc::new(AtomicUsize::new(0));
    let service_store: Arc<dyn SecretStore> =
        if reject { Arc::new(RejectWrites { inner: Arc::clone(&store), attempts: Arc::clone(&attempts) }) } else { store.clone() };
    let reads = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&reads);
    let maker_store = Arc::clone(&store);
    let make: Make = Box::new(move |_, _| {
        let key = maker_store.get(&Account::new("mock").unwrap()).unwrap();
        observed.lock().unwrap().push(key.as_ref().map(|key| key.expose() == NEW_KEY));
        match key {
            Some(_) => Ok(Arc::new(image(MockModel::new(MockOutput::Image)))),
            None => Err(Readiness::NeedsKey { account: "mock".into() }),
        }
    });
    let changed = Arc::new(key_changes::KeyChanges::default());
    let observed_changes = Arc::clone(&changed);
    let h = scene_on_with_keys(
        MOCK_IMAGE,
        "mock-square",
        Runs::with_inline_maker,
        make,
        |_, _| {},
        move |services| {
            register_key_service(services, Arc::new(key_changes::ObservedStore::new(service_store, Arc::clone(&observed_changes)))).unwrap();
            (None, Some(observed_changes))
        },
    );
    ((h, reads), attempts)
}

#[test]
fn a_synchronous_key_service_change_outside_the_panel_forgets_browser_adapters() {
    let ((mut h, reads), _) = browser_key_scene(false);
    generated(&mut h);
    assert_eq!(*reads.lock().unwrap(), [Some(false)]);
    h.state_mut().session.dispatch("keys.status", &json!({"account": "mock"})).unwrap();
    h.run();
    assert!(h.state().runs.as_ref().unwrap().has_cached_adapters(), "checking a key changes no adapter");
    h.state_mut().session.dispatch("keys.set", &json!({"account": "mock", "key": NEW_KEY})).unwrap();
    h.run();
    assert!(!h.state().runs.as_ref().unwrap().has_cached_adapters());
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(*reads.lock().unwrap(), [Some(false), Some(true)]);
    h.state_mut().run_confirm = None;
    h.state_mut().session.dispatch("keys.remove", &json!({"account": "mock"})).unwrap();
    h.run();
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.notice.as_deref().is_some_and(|notice| notice.contains("needs an API key")));
    assert_eq!(*reads.lock().unwrap(), [Some(false), Some(true), None]);
}

fn failed_browser_style_write_preserves_adapter(command: &str, params: serde_json::Value) {
    let ((mut h, reads), attempts) = browser_key_scene(true);
    generated(&mut h);
    assert_eq!(*reads.lock().unwrap(), [Some(false)]);
    assert!(h.state_mut().session.dispatch(command, &params).is_err(), "{command} must fail");
    assert_eq!(attempts.load(Ordering::SeqCst), 1, "{command} reached the rejecting browser store");
    h.run();
    assert!(h.state().runs.as_ref().unwrap().has_cached_adapters(), "{command} failed");
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(*reads.lock().unwrap(), [Some(false)], "{command} must reuse the old adapter");
}

#[test]
fn failed_browser_style_key_set_preserves_the_cached_adapter() {
    failed_browser_style_write_preserves_adapter("keys.set", json!({"account": "mock", "key": NEW_KEY}));
}

#[test]
fn failed_browser_style_key_remove_preserves_the_cached_adapter() {
    failed_browser_style_write_preserves_adapter("keys.remove", json!({"account": "mock"}));
}

#[test]
fn a_key_change_while_the_provider_is_set_up_builds_it_again_with_the_new_key() {
    let (make, open, built) = gated(image(MockModel::new(MockOutput::Image)));
    let mut h = scene(make);
    press(&mut h, Role::Button, "Generate");
    assert_eq!(h.state().notice.as_deref(), Some(super::runs::SETTING_UP));
    // The key is replaced while the first build is still reading the old one.
    let app = h.state_mut();
    app.runs.as_mut().unwrap().forget_adapters(&mut app.session).unwrap();
    open.send(()).unwrap();
    frames_until(&mut h, |_| *built.lock().unwrap() == 1);
    // Time for the stale answer to arrive and be dropped.
    std::thread::sleep(Duration::from_millis(200));
    frames_until(&mut h, |_| true);
    assert!(h.state().run_confirm.is_none(), "an adapter built with the old key is not used");
    open.send(()).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(*built.lock().unwrap(), 2);
    assert_eq!(h.state().notice, None);
}

#[test]
fn leaving_the_canvas_while_its_provider_is_set_up_clears_the_waiting_notice() {
    let (make, open, _) = gated(image(MockModel::new(MockOutput::Image)));
    let mut h = scene(make);
    press(&mut h, Role::Button, "Generate");
    assert_eq!(h.state().notice.as_deref(), Some(super::runs::SETTING_UP));
    h.state_mut().canvas = CanvasId::from_bytes([94; 16]);
    open.send(()).unwrap();
    frames_until(&mut h, |app| app.notice.is_none());
    assert!(h.state().run_confirm.is_none(), "nothing is quoted for a card on another canvas");
}

const BADGE: &str = "Stale result: refresh";

fn refresh_accept(h: &mut Harness<'static, NodeCraftApp>) {
    let label = h.get_by_label_contains("Refresh for").accesskit_node().label().unwrap_or_default();
    press(h, Role::Button, &label);
}

/// Generates the card once and waits for its result.
fn generated(h: &mut Harness<'static, NodeCraftApp>) {
    press(h, Role::Button, "Generate");
    frames_until(h, |app| app.run_confirm.is_some());
    accept(h);
    frames_until(h, |app| idle(app) && !node(app).history.is_empty());
}

#[test]
fn an_edited_card_shows_a_stale_badge_and_refreshing_runs_it_again() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    generated(&mut h);
    assert!(h.query_by_label(BADGE).is_none(), "a fresh result has no badge");
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "prompt", "value": Value::Text("A blue square".into())})).unwrap();
    h.run();
    press(&mut h, Role::Button, BADGE);
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert!(mock.requests().len() == 1, "the badge quotes; it starts nothing");
    h.get_by_label("Refresh");
    refresh_accept(&mut h);
    frames_until(&mut h, |app| idle(app) && node(app).history.len() == 2);
    h.run();
    assert_eq!(mock.requests().len(), 2);
    assert!(h.query_by_label(BADGE).is_none(), "the refreshed result matches its inputs");
}

#[test]
fn a_model_change_marks_the_result_stale() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    generated(&mut h);
    h.run();
    assert!(h.query_by_label(BADGE).is_none());
    let session = &mut h.state_mut().session;
    let preview = session.dispatch("node.model_preview", &json!({"canvas": C, "nodes": [N], "model": "mock-round"})).unwrap();
    let revision = session.revision();
    session
        .dispatch(
            "node.set_model",
            &json!({"canvas": C, "nodes": [N], "model": "mock-round", "expected_revision": revision, "preview_binding": preview["binding"]}),
        )
        .unwrap();
    for _ in 0..3 {
        h.run();
    }
    assert!(h.query_by_label(BADGE).is_some(), "the result was made by the other model");
    assert_eq!(mock.requests().len(), 1, "marking stale runs nothing");
}

#[test]
fn the_toolbar_expands_a_result_into_the_viewer() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock));
    generated(&mut h);
    h.state_mut().selection.only(N);
    h.run();
    press(&mut h, Role::Button, "Expand");
    frames_until(&mut h, |app| app.viewer == Some(N));
    assert_eq!(h.state().viewer, Some(N));
    assert!(h.query_by_label("Image").is_some(), "the viewer window is open");
    h.state_mut().selection.clear();
    let canvas = C;
    h.state_mut().session.dispatch("node.remove", &json!({"canvas": canvas, "id": N})).unwrap();
    h.run();
    h.run();
    assert_eq!(h.state().viewer, None, "the viewer closes with its node");
}

#[test]
fn an_edited_upstream_text_marks_the_card_stale_and_refresh_runs_with_it() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock.clone()));
    let text = NodeId::from_bytes([95; 16]);
    let session = &mut h.state_mut().session;
    session.dispatch("node.create", &json!({"canvas": C, "id": text, "kind": "source.text", "pos": {"x": -250.0, "y": 120.0}})).unwrap();
    session.dispatch("node.param_set", &json!({"canvas": C, "id": text, "name": "text", "value": Value::Text("A red square".into())})).unwrap();
    let edge = EdgeId::from_bytes([96; 16]);
    session
        .dispatch(
            "edge.connect",
            &json!({"canvas": C, "edge": {"id": edge, "from": {"node": text, "port": "out"}, "to": {"node": N, "port": "prompt"}, "order": 0}}),
        )
        .unwrap();
    h.run();
    generated(&mut h);
    assert!(h.query_by_label(BADGE).is_none());
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": C, "id": text, "name": "text", "value": Value::Text("A blue square".into())})).unwrap();
    h.run();
    press(&mut h, Role::Button, BADGE);
    frames_until(&mut h, |app| app.run_confirm.is_some());
    refresh_accept(&mut h);
    frames_until(&mut h, |app| idle(app) && node(app).history.len() == 2);
    h.run();
    assert!(h.query_by_label(BADGE).is_none());
    let prompts: Vec<String> = mock.requests().iter().map(|request| format!("{request:?}")).collect();
    assert!(prompts.last().is_some_and(|last| last.contains("A blue square")), "{prompts:?}");
}

#[test]
fn a_running_card_offers_no_second_refresh() {
    // The first build answers at once; after the key is replaced, the next one never finishes.
    let built = Arc::new(Mutex::new(0u32));
    let count = built.clone();
    let mut h = scene(Box::new(move |_, _| {
        let mut count = count.lock().unwrap();
        *count += 1;
        let mut model = MockModel::new(MockOutput::Image);
        if *count > 1 {
            model.polls = u32::MAX;
        }
        Ok(Arc::new(image(model)))
    }));
    generated(&mut h);
    let app = h.state_mut();
    app.runs.as_mut().unwrap().forget_adapters(&mut app.session).unwrap();
    app.session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "prompt", "value": Value::Text("A blue square".into())})).unwrap();
    h.run();
    press(&mut h, Role::Button, BADGE);
    frames_until(&mut h, |app| app.run_confirm.is_some());
    refresh_accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.is_running(C, N)));
    assert!(h.query_by_label(BADGE).is_none(), "no second paid refresh while the first runs");
}

#[test]
fn a_refresh_needs_no_provider_for_an_upstream_card_with_a_current_result() {
    // A text card with an answer, on a provider this test never sets up, feeds the prompt.
    let writer = NodeId::from_bytes([97; 16]);
    let mut h = scene_with(making(image(MockModel::new(MockOutput::Image))), |project, kinds| {
        let mut node = kinds
            .get(nodecraft_nodes::generation::TEXT_KIND)
            .unwrap()
            .instantiate_instance(writer, Default::default(), &[("model".into(), Value::Text("other-writer".into()))].into())
            .unwrap();
        let answer = nodecraft_types::GenerationId::from_bytes([98; 16]);
        node.history.push(nodecraft_graph::Generation::new(answer, vec![Value::Text("A red square".into())]));
        node.active = Some(answer);
        Change::AddNode { canvas: C, node }.apply(project).unwrap();
        let edge = nodecraft_graph::Edge {
            id: EdgeId::from_bytes([99; 16]),
            from: Endpoint { node: writer, port: "out".into() },
            to: Endpoint { node: N, port: "prompt".into() },
            order: 0,
        };
        Change::Connect { canvas: C, edge }.apply(project).unwrap();
    });
    h.state_mut().request_run(N, super::generate::RunKind::Refresh).unwrap();
    frames_until(&mut h, |app| app.run_confirm.is_some() || app.notice.as_deref().is_some_and(|n| n != super::runs::SETTING_UP));
    assert!(h.state().run_confirm.is_some(), "notice: {:?}", h.state().notice);
}

#[test]
#[ignore = "manual public-safe capture; RUNS409_RENDER_DIR"]
fn runs409_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("RUNS409_RENDER_DIR").unwrap());
    let render = |h: Harness<'static, NodeCraftApp>| {
        let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
        h.run();
        h
    };
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mut h = scene(making(image(slow)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.is_running(C, N)));
    render(h).render().unwrap().save(dir.join("runs409-running.png")).unwrap();
    let mut refusing = MockModel::new(MockOutput::Image);
    refusing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::UnsupportedInput));
    let mut h = scene(making(image(refusing)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && app.runs.as_ref().is_some_and(|runs| runs.failure(C, N).is_some()));
    render(h).render().unwrap().save(dir.join("runs409-failed.png")).unwrap();
}

#[test]
#[ignore = "manual public-safe capture; STALE211_RENDER_DIR"]
fn stale211_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("STALE211_RENDER_DIR").unwrap());
    let mut h = scene(making(image(MockModel::new(MockOutput::Image))));
    generated(&mut h);
    h.state_mut().session.dispatch("node.param_set", &json!({"canvas": C, "id": N, "name": "prompt", "value": Value::Text("A blue square".into())})).unwrap();
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "stale211-dark.png"), (crate::theme::ThemeKind::Light, "stale211-light.png")] {
        h.state_mut().theme = theme;
        let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
}

// The queue panel (#82).

fn open_queue(h: &mut Harness<'static, NodeCraftApp>) {
    let label = h
        .query_all_by_label_contains("Queue")
        .find(|node| node.accesskit_node().role() == Role::Button)
        .and_then(|node| node.accesskit_node().label())
        .unwrap_or_default();
    press(h, Role::Button, &label);
    assert!(h.state().queue_open);
}

#[test]
fn the_top_right_buttons_do_not_open_search_under_them() {
    // The keys row was drawn over the search and tag buttons; a click opened search too and
    // focused an input that was not drawn yet.
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mut h = scene(making(image(slow)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.card_status(C, N).is_some()));
    press(&mut h, Role::Button, "API keys");
    assert!(h.state().keys.open);
    open_queue(&mut h);
    assert!(!h.state().search.open && !h.state().tag_filter.open);
}

#[test]
fn the_queue_lists_a_running_job_and_cancelling_it_leaves_its_cost_unresolved() {
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mock = image(slow);
    let mut h = scene(making(mock.clone()));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.card_status(C, N).is_some_and(|s| s.starts_with("Generating "))));
    h.get_by_label("Queue (1)");
    open_queue(&mut h);
    h.get_by_label("mock · mock-image");
    h.get_by_label("0.04 USD est.");
    press(&mut h, Role::Button, "Cancel");
    frames_until(&mut h, idle);
    h.get_by_label("Cancelled after it was sent (cost unresolved)");
    h.get_by_label("0.04 USD est., unresolved");
    assert!(!mock.cancelled().is_empty(), "the provider was asked to cancel");
    assert!(node(h.state()).history.is_empty());
    // A cancel is not a failure: the card says nothing went wrong, and no failure notice shows.
    assert!(h.query_by_label_contains("Run failed").is_none());
    assert!(h.state().runs.as_ref().is_some_and(|runs| runs.failure(C, N).is_none()));
    assert!(!h.state().notice.as_deref().is_some_and(|notice| notice.contains("failed")), "{:?}", h.state().notice);
    h.get_by_role_and_label(Role::Button, "Queue");
    press(&mut h, Role::Button, "Clear finished");
    h.get_by_label_contains("Nothing has run yet");
}

#[test]
fn a_finished_job_shows_done_next_to_its_recorded_image() {
    let mut h = scene(making(image(MockModel::new(MockOutput::Image))));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    open_queue(&mut h);
    h.get_by_label("Done");
    h.get_by_label("0.04 USD est.");
    assert!(node(h.state()).active.is_some(), "the result is on its card");
}

#[test]
fn a_failed_job_names_its_cause_and_retry_quotes_again_first() {
    let mut refusing = MockModel::new(MockOutput::Image);
    refusing.fail = Some(MockFailure::Submit(nodecraft_providers::ErrorKind::Auth));
    let mock = image(refusing);
    let mut h = scene(making(mock.clone()));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && app.runs.as_ref().is_some_and(|runs| runs.failure(C, N).is_some()));
    h.get_by_label("Run failed: API key");
    open_queue(&mut h);
    h.get_by_label("Failed: API key");
    assert_eq!(mock.requests().len(), 1);
    press(&mut h, Role::Button, "Retry");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert_eq!(mock.requests().len(), 1, "a retry is quoted first; nothing is sent until it is accepted");
}

#[test]
#[ignore = "manual public-safe capture; QUEUE82_RENDER_DIR"]
fn queue82_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("QUEUE82_RENDER_DIR").unwrap());
    let mut slow = MockModel::new(MockOutput::Image);
    slow.polls = u32::MAX;
    let mut h = scene(making(image(slow)));
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| app.runs.as_ref().is_some_and(|runs| runs.card_status(C, N).is_some_and(|s| s.starts_with("Generating "))));
    open_queue(&mut h);
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "queue82-dark.png"), (crate::theme::ThemeKind::Light, "queue82-light.png")] {
        h.state_mut().theme = theme;
        let mut shot = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), h.into_state());
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), shot.into_state());
    }
}

#[test]
fn the_toolbar_downloads_the_shown_result_once_there_is_one() {
    let mock = image(MockModel::new(MockOutput::Image));
    let mut h = scene(making(mock));
    h.state_mut().selection.only(N);
    h.run();
    assert!(h.query_by_label_contains("Model ").is_some(), "the toolbar is up");
    assert!(h.query_by_label("Download").is_none(), "nothing to save before a result");
    generated(&mut h);
    h.state_mut().selection.only(N);
    h.run();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("take.png");
    h.state_mut().exports.answer = Some(path.clone());
    press(&mut h, Role::Button, "Download");
    h.run();
    h.run();
    let outputs = node(h.state()).history.first().map(|g| g.outputs.clone()).unwrap_or_default();
    let asset = match outputs.first() {
        Some(Value::Media(asset)) => asset.clone(),
        Some(Value::List(items)) => match items.first() {
            Some(Value::Media(asset)) => asset.clone(),
            other => panic!("an image result, not {other:?}"),
        },
        other => panic!("an image result, not {other:?}"),
    };
    let bytes = h.state().session.assets().get(&asset.hash).unwrap().to_vec();
    assert_eq!(std::fs::read(&path).unwrap(), bytes, "{:?}", h.state().notice);
    assert_eq!(h.state().notice.as_deref(), Some("Exported take.png."));
}

/// Three variations of the card, run against the mock host with the canvas's preference set by
/// the card's toggle (`as_cards`) or left alone.
fn three_variations(as_cards: bool) -> Harness<'static, NodeCraftApp> {
    let mut h = scene(making(image(MockModel::new(MockOutput::Image))));
    press(&mut h, Role::Button, "More variations");
    press(&mut h, Role::Button, "More variations");
    if as_cards {
        press(&mut h, Role::Button, "Variations as cards");
        assert!(h.state().session.document().canvases().get(&C).unwrap().variation_cards());
    }
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    accept(&mut h);
    frames_until(&mut h, |app| idle(app) && !node(app).history.is_empty());
    h
}

#[test]
fn variations_arrive_in_the_cards_history_by_default() {
    let h = three_variations(false);
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    assert_eq!(canvas.nodes().len(), 1, "no new cards");
    assert_eq!(node(h.state()).history.len(), 3, "three variations, three history entries");
}

#[test]
fn variations_arrive_as_cards_once_the_canvas_asks_for_them() {
    let h = three_variations(true);
    let canvas = h.state().session.document().canvases().get(&C).unwrap();
    assert_eq!(canvas.nodes().len(), 3, "the source and two new cards");
    for card in canvas.nodes().values() {
        assert_eq!(card.history.len(), 1, "one variation on each card");
    }
    let source = node(h.state());
    for card in canvas.nodes().values().filter(|card| card.id != N) {
        assert_eq!(card.kind, source.kind);
        assert_eq!(card.params, source.params);
        assert!(card.pos.x > source.pos.x, "beside the source");
    }
}

const SEEDED: &str = r#"
[[model]]
id = "mock-seeded"
provider = "mock"
provider_model = "mock-image"
label = "Mock seeded image"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.04 USD" }
[[model.modes]]
id = "text-to-image"
output = "image"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
[[model.params.params]]
name = "seed"
label = "Seed"
control = { type = "seed" }
default = { type = "unsigned", value = "7" }
"#;

#[test]
fn variations_with_a_pinned_seed_are_flagged_on_the_card_and_in_the_quote() {
    let mut h = scene_in(SEEDED, "mock-seeded", making(image(MockModel::new(MockOutput::Image))), |_, _| {});
    assert!(h.query_by_label("same seed").is_none(), "one result can't come out alike");
    press(&mut h, Role::Button, "More variations");
    assert!(h.query_by_label("same seed").is_some(), "the card warns as soon as there are two");
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    assert!(h.query_by_label(super::generate::ALIKE).is_some(), "the quote warns before anything is spent");
}

/// The same app, drawn by wgpu in the dark theme, for a screenshot.
fn drawn(h: Harness<'static, NodeCraftApp>) -> Harness<'static, NodeCraftApp> {
    let app = h.into_state();
    let mut h = Harness::builder().wgpu().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h
}

#[test]
#[ignore = "manual public-safe capture; VARIATIONS808_RENDER_DIR"]
fn variations808_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("VARIATIONS808_RENDER_DIR").unwrap());
    let mut h = drawn(three_variations(true));
    press(&mut h, Role::Button, "Fit all");
    h.run();
    h.render().unwrap().save(dir.join("variations808-cards.png")).unwrap();
    let mut h = scene_in(SEEDED, "mock-seeded", making(image(MockModel::new(MockOutput::Image))), |_, _| {});
    press(&mut h, Role::Button, "More variations");
    press(&mut h, Role::Button, "More variations");
    let mut h = drawn(h);
    h.state_mut().selection.only(N);
    h.run();
    h.render().unwrap().save(dir.join("variations808-same-seed.png")).unwrap();
    press(&mut h, Role::Button, "Generate");
    frames_until(&mut h, |app| app.run_confirm.is_some());
    h.render().unwrap().save(dir.join("variations808-quote.png")).unwrap();
}

mod known_media795;
