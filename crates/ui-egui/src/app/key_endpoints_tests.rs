//! The keys panel's endpoint rows (#962), driven through the app with settings in a temporary
//! folder: the default shows, an address is set and reset in settings, a bad one is refused
//! before anything is saved, and Test checks the key at the address in use.
#![cfg(not(target_arch = "wasm32"))]

use super::super::NodeCraftApp;
use crate::app::tests::harness;
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_engine::secrets::{Account, MemoryStore, Secret, SecretStore};
use nodecraft_engine::settings::{Location, open};
use nodecraft_providers::http::{Http, HttpRequest, HttpResponse};
use nodecraft_providers::{BoxFuture, ProviderError};
use std::sync::{Arc, Mutex};

const KEY: &str = "sk-proj-abcdefghij1234";
const GATEWAY: &str = "https://gateway.example/v1";

/// The app with settings in `dir` and the keys panel open.
fn app(dir: &std::path::Path) -> Harness<'static, NodeCraftApp> {
    let mut h = closed(dir);
    open_panel(&mut h);
    h
}

/// The app with settings in `dir`, the keys panel closed.
fn closed(dir: &std::path::Path) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let (store, _) = open(Location::Dir(dir.to_path_buf()));
    h.state_mut().store = Some(store);
    h
}

fn open_panel(h: &mut Harness<'static, NodeCraftApp>) {
    h.state_mut().keys.open = true;
    h.run();
}

/// The OpenAI chat row's address field.
fn field(h: &Harness<'static, NodeCraftApp>) -> egui::Pos2 {
    h.get_by_role_and_label(Role::TextInput, "OpenAI chat").rect().center()
}

fn type_address(h: &mut Harness<'static, NodeCraftApp>, url: &str) {
    let pos = field(h);
    h.hover_at(pos);
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.run();
    h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.run();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.event(egui::Event::Text(url.into()));
    h.run();
}

/// What the settings file says for OpenAI chat, read afresh.
fn saved(dir: &std::path::Path) -> Option<String> {
    let (store, _) = open(Location::Dir(dir.to_path_buf()));
    // Blank means the provider's own API: a reset writes it, so the file never keeps the old one.
    store.get().providers.base_urls.get("openai").and_then(|entry| entry.text()).filter(|text| !text.is_empty()).map(str::to_owned)
}

/// The OpenAI chat row's button `label`: the one level with its field.
fn click(h: &mut Harness<'static, NodeCraftApp>, label: &str) {
    let row = field(h).y;
    h.get_all_by_role_and_label(Role::Button, label).find(|button| (button.rect().center().y - row).abs() < 12.0).unwrap().click();
    h.run();
}

/// How many rows show `label` (OpenAI chat and OpenAI Images share a default).
fn count(h: &Harness<'static, NodeCraftApp>, label: &str) -> usize {
    h.query_all_by_label(label).count()
}

const OPENAI: &str = "https://api.openai.com/v1 (default)";

#[test]
fn the_default_shows_until_an_address_is_set_and_reset() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    assert_eq!(count(&h, OPENAI), 2, "the address in use is the default");
    assert!(h.query_by_label("https://api.anthropic.com/v1 (default)").is_some());

    type_address(&mut h, " https://gateway.example/v1/ ");
    click(&mut h, "Save address");
    assert_eq!(saved(dir.path()).as_deref(), Some(GATEWAY), "saved normalised, and written at once");
    assert_eq!(count(&h, GATEWAY), 1, "the row shows the address in use");
    assert_eq!(count(&h, OPENAI), 1, "OpenAI Images keeps its own");

    click(&mut h, "Reset");
    assert_eq!(saved(dir.path()), None, "reset removes the entry");
    assert_eq!(count(&h, OPENAI), 2);
}

#[test]
fn a_bad_address_is_refused_before_anything_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    for bad in ["http://gateway.example/v1", "https://user:pass@gateway.example/v1", "https://gateway.example/v1?key=1", "not a url"] {
        h.state_mut().keys.endpoint_notice(None);
        type_address(&mut h, bad);
        click(&mut h, "Save address");
        assert_eq!(saved(dir.path()), None, "{bad} is not saved");
        assert!(h.query_by_label_contains("OpenAI chat: ").is_some(), "{bad}: the refusal is shown");
        assert_eq!(count(&h, OPENAI), 2);
    }
}

/// Without settings to save in, the rows aren't offered, and a change is refused.
#[test]
fn without_settings_the_rows_are_not_offered() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = app(dir.path());
    h.state_mut().store = None;
    h.run();
    // With nowhere to save, the rows aren't offered at all.
    assert_eq!(count(&h, OPENAI), 0);
    let refused = h.state_mut().save_endpoint(&crate::keys::EndpointChange { provider: "openai", url: Some(GATEWAY.into()) }).unwrap_err();
    assert!(refused.message.contains("can't be saved"), "{refused:?}");
}

/// Answers 200 at the gateway and 401 anywhere else; records each URL and whether a key went.
struct Gateway(Mutex<Vec<(String, bool)>>);
impl Http for Gateway {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("key checks never follow redirects")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let status = if request.url.starts_with("https://gateway.example/") { 200 } else { 401 };
        let keyed = request.headers.iter().any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value.ends_with(KEY));
        self.0.lock().unwrap().push((request.url, keyed));
        Box::pin(async move { Ok(HttpResponse { status, headers: vec![], body: br#"{"data": []}"#.to_vec() }) })
    }
}

#[test]
fn test_checks_the_key_at_the_address_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = closed(dir.path());
    let keychain = Arc::new(MemoryStore::default());
    let openai = Account::new("openai").unwrap();
    keychain.set(&openai, &Secret::new(KEY).unwrap()).unwrap();
    let http = Arc::new(Gateway(Mutex::default()));
    let mut services = nodecraft_engine::commands::Services::default();
    let runs = super::super::runs::Runs::with_inline_maker(
        &mut services,
        Box::new(|_, _| Err(nodecraft_engine::provider_setup::Readiness::NotSetUp("no adapters here".into()))),
    )
    .unwrap()
    .with_key_http(http.clone());
    let host = runs.key_test_host(nodecraft_engine::settings::ProviderSettings::default(), Some(dir.path().to_path_buf()));
    let (worker, results) = nodecraft_engine::secrets::KeyWorker::spawn_with_tests(keychain, host).unwrap();
    nodecraft_engine::secrets::register_key_worker(&mut services, worker).unwrap();
    let app = h.state_mut();
    app.session = nodecraft_engine::Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([3; 16]), "Keys").unwrap())
        .unwrap()
        .with_services(services);
    app.runs = Some(runs);
    app.key_results = Some(results);
    assert!(app.key_accounts.remember(&openai));
    open_panel(&mut h);

    type_address(&mut h, GATEWAY);
    click(&mut h, "Save address");
    assert_eq!(saved(dir.path()).as_deref(), Some(GATEWAY));
    h.get_by_role_and_label(Role::Button, "Test").click();
    h.run();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().keys.is_following() {
        assert!(std::time::Instant::now() < deadline, "the check never answered");
        std::thread::sleep(std::time::Duration::from_millis(1));
        // The host takes the keychain worker's answers each frame, as `update` does.
        let ctx = h.ctx.clone();
        h.state_mut().tick_keys(&ctx);
        h.run();
    }
    h.run();
    assert_eq!(*http.0.lock().unwrap(), [("https://gateway.example/v1/models".to_owned(), true)], "checked at the gateway only, with the key");
    assert!(h.query_by_label_contains("works: the provider accepted it").is_some());
    assert!(h.query_by_label_contains("abcdefghij").is_none(), "the key is never shown");
}

/// A misspelled entry refuses OpenAI chat's runs, so the row says so rather than "(default)",
/// and Reset says the file must be fixed: settings keep the keys they don't model.
#[test]
fn a_misspelled_entry_shows_as_unusable_and_reset_says_to_fix_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = closed(dir.path());
    let store = h.state_mut().store.as_mut().unwrap();
    store.update(|saved| {
        saved.providers.base_urls.insert("OpenAI".into(), GATEWAY.into());
    });
    store.flush().unwrap();
    assert!(nodecraft_engine::custom_endpoints::base_url(&h.state().provider_settings(), "openai").is_err(), "runs are refused");
    open_panel(&mut h);
    assert!(h.query_by_label_contains("can't be used").is_some());
    assert_eq!(count(&h, OPENAI), 1, "only OpenAI Images is at its default");
    click(&mut h, "Reset");
    let notice = h.state().keys.endpoint_notice_text().unwrap_or_default().to_owned();
    assert!(notice.contains("config.toml still has another entry"), "the person is told where to fix it: {notice:?}");
    assert!(h.query_by_label_contains("can't be used").is_some(), "and the row still says so");
}

/// Manual evidence: the keys panel's endpoint rows, one set, in both themes.
#[test]
#[ignore = "manual public-safe panel capture; set NODECRAFT_ENDPOINTS962_SCREENSHOT to a path prefix"]
fn capture_endpoints962_panel() {
    let prefix = std::env::var("NODECRAFT_ENDPOINTS962_SCREENSHOT").unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let dir = tempfile::tempdir().unwrap();
        let mut h = Harness::builder()
            .wgpu()
            .with_size(egui::vec2(1200.0, 760.0))
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
        let (mut store, _) = open(Location::Dir(dir.path().to_path_buf()));
        store.update(|saved| {
            saved.providers.base_urls.insert("openai".into(), "http://127.0.0.1:4000/v1".into());
        });
        h.state_mut().store = Some(store);
        crate::theme::apply(&h.ctx, theme);
        open_panel(&mut h);
        h.run();
        h.render().unwrap().save(format!("{prefix}-{theme:?}.png")).unwrap();
    }
}
