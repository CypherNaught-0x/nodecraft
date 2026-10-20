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
        let fixture = ProviderSettings { comfyui_url: Some("http://127.0.0.1:8188".into()), install_salt: "salt".into() };
        let settings = if checkpoints.is_some() { &fixture } else { settings };
        let keys = nodecraft_engine::secrets::MemoryStore::default();
        let env = HostEnv { keys: &keys, http: http.clone(), settings, now_ms: Arc::new(|| 0) };
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
    assert_eq!(answered(&mut h), "Fetched 1 option list (2 values).");
    let status = h.state_mut().session.dispatch("options.status", &json!({})).unwrap();
    assert_eq!(status["last"]["installed"][0]["owner"], "comfyui.sdxl");
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
    assert!(notice.contains("comfyui_url"), "{notice}");
}

#[test]
fn a_refused_list_is_named_in_the_notice() {
    let refused =
        |source: &str| option_fetch::Refused { owner: "comfyui.sdxl".into(), source: source.into(), reason: "the server lists more than 16384 values".into() };
    let report = Report { id: 1, installed: Vec::new(), refused: vec![refused("comfyui.checkpoints"), refused("comfyui.loras")], error: None };
    assert_eq!(
        notice(&report),
        "Fetched 0 option lists (0 values). comfyui.checkpoints for comfyui.sdxl wasn't installed: the server lists more than 16384 values (and 1 more)."
    );
}
