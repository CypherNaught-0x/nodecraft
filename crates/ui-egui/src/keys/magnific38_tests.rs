//! Key-panel evidence never reads a real keychain or sends a network request.
use super::*;

fn themed(builder: egui_kittest::HarnessBuilder<State>, theme: crate::theme::ThemeKind) -> Harness<'static, State> {
    themed_state(builder, theme, sync_state(Arc::new(MemoryStore::default()), &[]))
}

fn themed_state(builder: egui_kittest::HarnessBuilder<State>, theme: crate::theme::ThemeKind, state: State) -> Harness<'static, State> {
    let mut harness = builder.with_size(egui::vec2(1250., 760.)).build_ui_state(
        move |ui, state: &mut State| {
            if let Some(results) = &state.results {
                let mut outcomes = std::mem::take(&mut state.completed);
                outcomes.extend(results.drain());
                let (changes, _) = state.panel.receive(outcomes);
                state.changes.extend(changes);
            }
            let changes = state.panel.show(ui.ctx(), &mut state.session, &state.settings, theme.tokens());
            state.changes.extend(changes);
        },
        state,
    );
    crate::theme::apply(&harness.ctx, theme);
    harness.run();
    harness
}

#[test]
fn magnific38_key_preset_saves_only_the_named_account_and_clears_the_password() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let store = Arc::new(MemoryStore::default());
        let mut harness = themed_state(Harness::builder(), theme, sync_state(store.clone(), &[]));
        harness.get_by_label("magnific").click();
        harness.run();
        assert_eq!(harness.state().panel.account, "magnific");
        assert!(harness.state().changes.is_empty());
        type_into(&mut harness, "Key", KEY);
        harness.get_by_label("Save").click();
        harness.run();
        assert!(harness.state().panel.key.is_empty());
        assert_eq!(store.get(&Account::new("magnific").unwrap()).unwrap().unwrap().expose(), KEY);
        assert!(store.get(&Account::new("fal").unwrap()).unwrap().is_none());
        assert_eq!(harness.state().changes, [AccountChange::Remember(Account::new("magnific").unwrap())]);
        assert!(harness.query_by_label_contains("abcdefghij").is_none());
        assert!(harness.query_by_label("Stored …1234").is_some());
    }
}

struct NeverHttp(std::sync::atomic::AtomicUsize);
impl nodecraft_providers::Http for NeverHttp {
    fn send(
        &self,
        _: nodecraft_providers::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        panic!("key checks must not follow redirects")
    }
    fn send_no_redirect(
        &self,
        _: nodecraft_providers::HttpRequest,
    ) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::Network, "unexpected key probe")) })
    }
}

#[test]
fn magnific38_test_key_reports_unverifiable_without_any_http() {
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let store = Arc::new(MemoryStore::default());
        store.set(&Account::new("magnific").unwrap(), &nodecraft_engine::secrets::Secret::new(KEY).unwrap()).unwrap();
        let runtime = TestRuntime(tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap());
        let http = Arc::new(NeverHttp(std::sync::atomic::AtomicUsize::new(0)));
        let host = nodecraft_engine::key_test::KeyTestHost { runtime: Arc::new(runtime), http: http.clone() };
        let (worker, results) = KeyWorker::spawn_with_tests(store.clone(), Some(host)).unwrap();
        let mut services = Services::default();
        register_key_worker(&mut services, worker).unwrap();
        let mut state = sync_state(store, &["magnific"]);
        state.session = session(services);
        state.results = Some(results);
        let mut harness = themed_state(Harness::builder().with_step_dt(1. / 60.), theme, state);
        harness.get_by_label("Test").click();
        harness.run_steps(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while harness.state().panel.is_waiting() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
            harness.run_steps(1);
        }
        assert!(matches!(&harness.state().panel.rows()[0].1, KeyStatus::Unverifiable(masked, _) if masked == "…1234"));
        assert!(harness.query_by_label_contains("can't verify").is_some());
        assert!(harness.query_by_label_contains("abcdefghij").is_none());
        assert_eq!(http.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(harness.state().changes.is_empty());
    }
}

#[test]
#[ignore = "native GPU evidence; NODECRAFT_MAGNIFIC38_RENDER_ROOT and NODECRAFT_MAGNIFIC38_STAGE"]
fn magnific38_native_keys_capture() {
    let root = std::path::PathBuf::from(std::env::var_os("NODECRAFT_MAGNIFIC38_RENDER_ROOT").unwrap());
    let stage = std::env::var("NODECRAFT_MAGNIFIC38_STAGE").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    for theme in [crate::theme::ThemeKind::Dark, crate::theme::ThemeKind::Light] {
        let mut harness = themed(Harness::builder().wgpu(), theme);
        harness.hover_at(egui::pos2(1240., 745.));
        harness.run();
        assert_eq!(harness.query_by_label("magnific").is_some(), stage == "after");
        assert!(harness.state().panel.key.is_empty());
        assert!(harness.state().changes.is_empty());
        harness.render().unwrap().save(root.join(format!("{theme:?}-keys-{stage}.png"))).unwrap();
    }
}
