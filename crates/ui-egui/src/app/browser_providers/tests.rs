use super::*;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use nodecraft_engine::commands::Services;
use nodecraft_engine::secrets::{OriginStorage, SecretError};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Origin(Mutex<BTreeMap<String, String>>);
impl OriginStorage for Origin {
    fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn remove(&self, key: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
    fn keys(&self) -> Result<Vec<String>, SecretError> {
        Ok(self.0.lock().unwrap().keys().cloned().collect())
    }
}

fn app(origin: Arc<Origin>) -> NodeCraftApp {
    let (store, notice) = OriginProviders::load(origin);
    let mut services = Services::default();
    store.register(&mut services).unwrap();
    let mut app = NodeCraftApp::with_services(services).unwrap();
    app.browser_providers = Some(BrowserProviders::new(store, notice));
    app
}

fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
    Harness::builder().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
}

#[test]
fn server_panel_saves_through_commands_and_a_reload_reads_the_same_origin() {
    let origin = Arc::new(Origin::default());
    let mut h = harness(app(Arc::clone(&origin)));
    h.run();
    h.get_by_label("Providers").click();
    h.run();
    assert!(h.query_by_label_contains("CORS").is_some());
    assert!(h.query_by_label_contains("LiteLLM models are not available yet").is_some());
    let providers = h.state_mut().browser_providers.as_mut().unwrap();
    providers.comfyui = "http://127.0.0.1:8188".into();
    providers.litellm = "https://proxy.example".into();
    h.run();
    h.get_by_label("Save servers").click();
    h.run();
    assert!(h.query_by_label("Provider servers saved for this site.").is_some());
    let settings = h.state().provider_settings();
    assert_eq!(nodecraft_engine::browser_providers::comfyui_url(&settings).as_deref(), Some("http://127.0.0.1:8188"));
    assert_eq!(settings.litellm_url.as_deref(), Some("https://proxy.example"));
    assert_eq!(app(origin).provider_settings(), settings);
}

#[test]
fn bad_server_input_preserves_saved_settings_and_never_enters_the_project() {
    let mut app = app(Arc::new(Origin::default()));
    app.browser_providers.as_mut().unwrap().open = true;
    let document = app.session.document().clone();
    let mut h = harness(app);
    h.run();
    h.state_mut().browser_providers.as_mut().unwrap().comfyui = "https://user:secret721@server.example".into();
    h.run();
    h.get_by_label("Save servers").click();
    h.run();
    assert_eq!(h.state().provider_settings(), ProviderSettings::default());
    assert_eq!(h.state().session.document(), &document);
    assert!(!h.state().browser_providers.as_ref().unwrap().notice.as_ref().unwrap().contains("secret721"));
}

#[test]
#[ignore = "manual public-safe #721 evidence; requires NODECRAFT_REVIEW_EVIDENCE"]
fn render_browser_provider_settings_evidence() {
    let folder = std::path::PathBuf::from(std::env::var_os("NODECRAFT_REVIEW_EVIDENCE").unwrap());
    std::fs::create_dir_all(&folder).unwrap();
    let mut app = app(Arc::new(Origin::default()));
    app.browser_providers.as_mut().unwrap().open = true;
    let mut h = Harness::builder().wgpu().with_size(egui::Vec2::new(1200.0, 800.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app);
    crate::theme::apply(&h.ctx, crate::theme::ThemeKind::Dark);
    h.run();
    h.render().unwrap().save(folder.join("before-provider-servers.png")).unwrap();
    let providers = h.state_mut().browser_providers.as_mut().unwrap();
    providers.comfyui = "http://127.0.0.1:8188".into();
    providers.litellm = "https://proxy.example".into();
    h.run();
    h.get_by_label("Save servers").click();
    h.run();
    h.render().unwrap().save(folder.join("after-provider-servers.png")).unwrap();
}
