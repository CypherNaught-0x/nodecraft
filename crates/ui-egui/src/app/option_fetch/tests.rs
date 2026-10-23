//! The desktop's option fetch (#529), with `/object_info` served by a fixture.
use super::*;
use crate::app::NodeCraftApp;
use crate::app::tests::harness;
use crate::palette;
use egui_kittest::Harness;
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, ProviderError};

struct Fixture(Vec<u8>);

impl Http for Fixture {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let body = self.0.clone();
        Box::pin(async move { Ok(HttpResponse { status: 200, headers: Vec::new(), body }) })
    }
}

/// Fetches from a fixture server with the given checkpoints, or with the app's own settings
/// (none, headless) when `checkpoints` is `None`.
fn fetcher(checkpoints: Option<&'static [&'static str]>) -> Fetch {
    let info = json!({"CheckpointLoaderSimple": {"input": {"required": {"ckpt_name": [checkpoints.unwrap_or(&[])]}}, "output": ["MODEL"]}});
    let http: Arc<dyn Http> = Arc::new(Fixture(serde_json::to_vec(&info).unwrap()));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    Box::new(move |sources, settings| {
        let fixture = ProviderSettings::with_comfyui("http://127.0.0.1:8188", "salt");
        let settings = if checkpoints.is_some() { &fixture } else { settings };
        let keys = nodecraft_engine::secrets::MemoryStore::default();
        let env = HostEnv { keys: &keys, http: http.clone(), settings, now_ms: Arc::new(|| 0), progress: None };
        runtime.block_on(option_fetch::fetch(&env, sources))
    })
}

fn with_fetcher(fetch: Fetch) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let (fetcher, mut service) = OptionFetcher::spawn(fetch).unwrap();
    let app = h.state_mut();
    app.session.register_host_service(OPTION_SERVICE, move |request| service(request)).unwrap();
    app.option_fetch = Some(fetcher);
    h
}

/// Frames until the fetch is told.
fn answered(h: &mut Harness<'static, NodeCraftApp>) -> String {
    for _ in 0..500 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_option_fetch(&ctx);
        if !h.state().option_fetch.as_ref().unwrap().is_waiting() {
            return h.state().notice.clone().unwrap_or_default();
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the fetch never answered");
}

#[test]
fn the_palette_refreshes_the_lists_and_says_what_it_installed() {
    let mut h = with_fetcher(fetcher(Some(&["sdxl.safetensors", "juggernaut.safetensors"])));
    assert!(h.state().palette_entries().iter().any(|entry| entry.target == palette::Target::Command("options.refresh")));
    h.state_mut().dispatch_palette_command("options.refresh");
    // A second ask while the first runs is refused, not queued behind it.
    let busy = h.state_mut().session.dispatch("options.refresh", &json!({})).unwrap_err();
    assert_eq!(busy.kind, ErrorKind::Busy);
    // The FLUX and SDXL cards each list the server's checkpoints.
    assert_eq!(answered(&mut h), "Fetched 2 option lists (4 values).");
    let status = h.state_mut().session.dispatch("options.status", &json!({})).unwrap();
    let owners: Vec<&str> = status["last"]["installed"].as_array().unwrap().iter().filter_map(|i| i["owner"].as_str()).collect();
    assert_eq!(owners, ["comfyui.flux-dev", "comfyui.sdxl"]);
    // And it can be asked again once that one is done.
    h.state_mut().session.dispatch("options.refresh", &json!({})).unwrap();
    answered(&mut h);
}

#[test]
fn without_a_server_in_settings_the_notice_says_what_to_add() {
    let mut h = with_fetcher(fetcher(None));
    h.state_mut().session.dispatch("options.refresh", &json!({})).unwrap();
    let notice = answered(&mut h);
    assert!(notice.starts_with("Option lists weren't fetched: ComfyUI isn't set up"), "{notice}");
    assert!(notice.contains("[[providers.comfyui]]"), "{notice}");
}

#[test]
fn a_refused_list_is_named_in_the_notice() {
    let refused =
        |source: &str| option_fetch::Refused { owner: "comfyui.sdxl".into(), source: source.into(), reason: "the server lists more than 16384 values".into() };
    let report =
        Report { id: 1, installed: Vec::new(), refused: vec![refused("comfyui.checkpoints"), refused("comfyui.loras")], unavailable: Vec::new(), error: None };
    assert_eq!(
        notice(&report),
        "Fetched 0 option lists (0 values). comfyui.checkpoints for comfyui.sdxl wasn't installed: the server lists more than 16384 values (and 1 more)."
    );
    // A provider that isn't set up is told only when nothing else came back.
    let missing = option_fetch::Refused {
        owner: "higgsfield.genjutsu-restyle".into(),
        source: "higgsfield.restyle_presets".into(),
        reason: "Higgsfield needs an API key.".into(),
    };
    let alone = Report { id: 2, installed: Vec::new(), refused: Vec::new(), unavailable: vec![missing.clone()], error: None };
    assert_eq!(notice(&alone), "Option lists weren't fetched: Higgsfield needs an API key.");
    let installed = option_fetch::Installed { owner: "comfyui.sdxl".into(), source: "comfyui.checkpoints".into(), values: 2, skipped: 0 };
    let beside = Report { installed: vec![installed], ..alone };
    assert_eq!(notice(&beside), "Fetched 1 option list (2 values).");
}

// The card's picker over a fetched list (#676).
mod picker {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};
    use nodecraft_types::{CanvasId, Choice, NodeId, OptionKey, OptionLists, Value};

    const C: CanvasId = CanvasId::from_bytes([81; 16]);
    const N: NodeId = NodeId::from_bytes([82; 16]);

    /// One SDXL card, holding `checkpoint` when given.
    fn scene(checkpoint: Option<&str>) -> NodeCraftApp {
        let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([83; 16]), "Option picker").unwrap();
        nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
        let (kinds, _) = nodecraft_engine::generation_catalog::default_registry().unwrap();
        let mut values: nodecraft_types::ParamValues =
            [("model".into(), Value::Text("comfyui.sdxl".into())), ("prompt".into(), Value::Text("A pier".into()))].into();
        if let Some(checkpoint) = checkpoint {
            values.insert("checkpoint".into(), Value::Text(checkpoint.into()));
        }
        let node = kinds.get(nodecraft_nodes::generation::KIND).unwrap().instantiate_instance(N, Default::default(), &values).unwrap();
        nodecraft_graph::Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        NodeCraftApp::from_session(Session::new(project).unwrap(), C).unwrap()
    }

    fn harness(app: NodeCraftApp) -> Harness<'static, NodeCraftApp> {
        Harness::builder().with_size(egui::Vec2::new(1400.0, 1000.0)).with_max_steps(120).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app)
    }

    /// Installs a labelled checkpoint list, as a fetch would.
    fn install(app: &mut NodeCraftApp) {
        let mut lists = OptionLists::default();
        let choices =
            vec![Choice { value: "a.safetensors".into(), label: "Model A".into() }, Choice { value: "b.safetensors".into(), label: "Model B".into() }];
        lists.set(OptionKey { owner: "comfyui.sdxl", source: "comfyui.checkpoints", scope: None }, choices).unwrap();
        app.session.swap_resource(Some(lists));
    }

    fn checkpoint(app: &NodeCraftApp) -> Option<Value> {
        app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().params.get("checkpoint").cloned()
    }

    /// What the checkpoint picker shows.
    fn shown(h: &Harness<'static, NodeCraftApp>) -> Option<String> {
        h.get_by_role_and_label(Role::ComboBox, "Checkpoint").accesskit_node().value()
    }

    #[test]
    fn picking_a_label_stores_its_value() {
        let mut app = scene(None);
        install(&mut app);
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_label(crate::card::listed::UNFETCHED_HINT).is_none());
        h.get_by_role_and_label(Role::ComboBox, "Checkpoint").click();
        h.run();
        h.get_by_label("Model B").click();
        h.run();
        assert_eq!(checkpoint(h.state()), Some(Value::Text("b.safetensors".into())));
        // The card shows the label, not the stored file name.
        assert_eq!(shown(&h).as_deref(), Some("Model B"));
    }

    #[test]
    fn a_value_off_the_list_is_shown_as_it_is_and_kept() {
        let mut app = scene(Some("gone.safetensors"));
        install(&mut app);
        let before = app.session.document().clone();
        let mut h = harness(app);
        h.run();
        assert_eq!(shown(&h).as_deref(), Some("gone.safetensors (not an option)"));
        assert_eq!(h.state().session.document(), &before);
    }

    #[test]
    fn before_a_fetch_the_field_is_text_with_a_hint_and_a_fetch_turns_it_into_a_picker() {
        let mut h = harness(scene(None));
        let (fetcher, mut service) = OptionFetcher::spawn(super::fetcher(Some(&["sdxl.safetensors", "juggernaut.safetensors"]))).unwrap();
        let app = h.state_mut();
        app.session.register_host_service(OPTION_SERVICE, move |request| service(request)).unwrap();
        app.option_fetch = Some(fetcher);
        h.run();
        assert!(h.query_by_label(crate::card::listed::UNFETCHED_HINT).is_some());
        assert!(h.query_by_role_and_label(Role::ComboBox, "Checkpoint").is_none());
        h.state_mut().session.dispatch("options.refresh", &json!({})).unwrap();
        answered(&mut h);
        h.run();
        assert!(h.query_by_label(crate::card::listed::UNFETCHED_HINT).is_none());
        h.get_by_role_and_label(Role::ComboBox, "Checkpoint").click();
        h.run();
        h.get_by_label("juggernaut.safetensors").click();
        h.run();
        assert_eq!(checkpoint(h.state()), Some(Value::Text("juggernaut.safetensors".into())));
    }

    #[test]
    fn lists_installed_after_the_card_was_cached_show_without_a_document_edit() {
        let mut h = harness(scene(None));
        h.run();
        assert!(h.query_by_role_and_label(Role::ComboBox, "Checkpoint").is_none());
        // Installed behind the view cache's back: the card reads the lists live.
        install(h.state_mut());
        h.run();
        // The card's default checkpoint, which this server doesn't list.
        assert_eq!(shown(&h).as_deref(), Some("sd_xl_base_1.0.safetensors (not an option)"));
    }

    #[test]
    fn a_stored_value_of_another_type_keeps_the_ordinary_control() {
        let app = scene(None);
        let mut node = app.session.document().canvases().get(&C).unwrap().nodes().get(&N).unwrap().clone();
        node.params.insert("checkpoint".into(), Value::Bool(true));
        let mut project = app.session.document().clone();
        nodecraft_graph::Change::RemoveNode { canvas: C, id: N }.apply(&mut project).unwrap();
        nodecraft_graph::Change::AddNode { canvas: C, node }.apply(&mut project).unwrap();
        let mut app = NodeCraftApp::from_session(Session::new(project).unwrap(), C).unwrap();
        install(&mut app);
        let mut h = harness(app);
        h.run();
        assert!(h.query_by_role_and_label(Role::ComboBox, "Checkpoint").is_none());
        assert!(h.query_by_label(crate::card::listed::UNFETCHED_HINT).is_none());
    }

    use egui::accesskit::Role;
}
