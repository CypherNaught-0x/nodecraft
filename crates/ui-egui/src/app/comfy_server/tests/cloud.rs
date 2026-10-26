use super::*;
use nodecraft_engine::secrets::{Account, MemoryStore, Secret, SecretStore};

struct Cloud(Arc<Mutex<Vec<HttpRequest>>>);
impl Http for Cloud {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("a hosted key must never use redirect-capable transport")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        assert_eq!(request.url, "https://cloud.comfy.org/api/object_info");
        self.0.lock().unwrap().push(request);
        Box::pin(async { Ok(HttpResponse { status: 200, headers: vec![], body: OBJECT_INFO.as_bytes().to_vec() }) })
    }
}

fn hosted(h: &mut Harness<'static, NodeCraftApp>, dir: &std::path::Path, key: bool) -> Arc<Mutex<Vec<HttpRequest>>> {
    let (mut store, _) = open(Location::Dir(dir.to_path_buf()));
    store.update(|settings| settings.providers.install_salt = "salt".into());
    h.state_mut().store = Some(store);
    let requests = Arc::new(Mutex::new(vec![]));
    let http: Arc<dyn Http> = Arc::new(Cloud(requests.clone()));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let fetch: Fetch = Box::new(move |settings| {
        let keys = MemoryStore::default();
        if key {
            keys.set(&Account::new("comfy-cloud").unwrap(), &Secret::new("public-cloud-fixture").unwrap()).unwrap();
        }
        let env = HostEnv { keys: &keys, http: http.clone(), settings, now_ms: Arc::new(|| 0), progress: None };
        runtime.block_on(comfy_workflow_cmds::fetch(&env))
    });
    let (fetcher, mut service) = ComfyNodesFetcher::spawn(fetch).unwrap();
    h.state_mut().session.register_host_service(NODES_SERVICE, move |request| service(request)).unwrap();
    h.state_mut().comfy_nodes = Some(fetcher);
    requests
}

#[test]
fn pointer_adds_hosted_profile_checks_with_key_and_preserves_fixed_address_on_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness();
    let requests = hosted(&mut h, dir.path(), true);
    h.state_mut().comfy_server.open = true;
    h.run();
    h.get_by_label("Add Comfy Cloud").click();
    h.run();
    answered(&mut h);
    assert_eq!(default_name(&h).as_deref(), Some("Comfy Cloud"));
    assert!(h.state().provider_settings().default_comfy().unwrap().is_cloud());
    assert_eq!(nodes(&h), Some(1));
    h.get_by_label("Comfy Cloud · hosted GPU");
    h.get_by_label("GPU usage is billed. The amount is unknown before a run; finite budgets block it.");
    h.get_by_label("Add your Comfy API key in API keys, comfy-cloud.");
    assert!(h.query_by_role_and_label(Role::TextInput, "Address").is_none());
    let request = requests.lock().unwrap()[0].clone();
    assert!(request.secret_headers.contains(&"x-api-key".to_owned()));
    assert!(request.headers.iter().any(|(name, value)| name == "X-API-Key" && value == "public-cloud-fixture"));
    h.get_by_role_and_label(Role::TextInput, "Jobs at once").click();
    h.run();
    h.event(egui::Event::Text("2".into()));
    h.run();
    h.get_by_label("Save and check").click();
    h.run();
    answered(&mut h);
    let saved = h.state().provider_settings();
    assert!(saved.default_comfy().unwrap().is_cloud());
    assert_eq!(saved.default_comfy().unwrap().max_jobs(), Some(2));
    assert!(h.state_mut().save_comfy_server("Comfy Cloud", "https://example.invalid", "2").is_err());
    h.state_mut().store.as_mut().unwrap().flush().unwrap();
    let (saved, _) = open(Location::Dir(dir.path().to_path_buf()));
    assert!(saved.get().providers.default_comfy().unwrap().is_cloud());
    let config = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(config.contains("backend = \"cloud\""));
    assert!(!config.contains("public-cloud-fixture"));
}

#[test]
fn missing_cloud_key_is_actionable_and_sends_no_request() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness();
    let requests = hosted(&mut h, dir.path(), false);
    h.state_mut().add_comfy_cloud("Hosted").unwrap();
    answered(&mut h);
    assert!(requests.lock().unwrap().is_empty());
    let report = comfy_server_cmds::report(&h.state().session, "Hosted").unwrap();
    let message = report.error.as_ref().unwrap();
    assert!(message.contains("comfy-cloud") && message.contains("API key"), "{message}");
}

#[test]
#[ignore = "manual public-safe hosted panel evidence; set NODECRAFT_COMFY16_SCREENSHOT"]
fn capture_comfy16_hosted_panel() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = Harness::builder()
        .wgpu()
        .with_size(egui::vec2(1100.0, 760.0))
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), NodeCraftApp::headless().unwrap());
    let requests = hosted(&mut h, dir.path(), true);
    h.state_mut().comfy_server.open = true;
    h.run();
    h.get_by_label("Add Comfy Cloud").click();
    h.run();
    answered(&mut h);
    assert_eq!(requests.lock().unwrap().len(), 1);
    h.render().unwrap().save(std::env::var("NODECRAFT_COMFY16_SCREENSHOT").unwrap()).unwrap();
}
