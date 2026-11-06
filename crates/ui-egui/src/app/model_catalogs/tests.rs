//! Reading provider model lists in the desktop, with OpenRouter and a LiteLLM proxy served by a
//! fixture, then finding the listed models in the catalog.
use super::*;
use crate::app::NodeCraftApp;
use crate::app::tests::harness;
use egui_kittest::Harness;
use nodecraft_nodes::generation::{KIND, TEXT_KIND};
use nodecraft_providers::{BoxFuture, HttpRequest, HttpResponse, ProviderError};
use std::sync::Mutex;

const IMAGES: &str = r#"{"data": [{"id": "tencent/hy-image-v3.5-preview", "name": "Tencent: Hy Image 3.5 Preview",
  "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["image"]},
  "supported_parameters": {"resolution": {"type": "enum", "values": ["1K", "2K"]}}}]}"#;
const CHAT: &str = r#"{"data": [{"id": "meta-llama/llama-3.3-70b-instruct:free", "name": "Llama (free)",
  "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]}, "pricing": {"prompt": "0", "completion": "0"}}]}"#;
const LITELLM: &str = r#"{"data": [{"id": "local-free-model"}, {"id": "local-free-model-ml"}]}"#;

/// Answers each listing by its address, and remembers what was asked and with which headers.
#[derive(Default)]
struct Fixture {
    asked: Mutex<Vec<(String, bool)>>,
}

impl Http for Fixture {
    fn send(&self, _: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        panic!("listings are never read through redirects")
    }
    fn send_no_redirect(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        let keyed = request.headers.iter().any(|(name, _)| name.eq_ignore_ascii_case("authorization"));
        self.asked.lock().unwrap().push((request.url.clone(), keyed));
        let body = match request.url.as_str() {
            "https://openrouter.ai/api/v1/models" => CHAT,
            "https://openrouter.ai/api/v1/images/models" => IMAGES,
            "http://127.0.0.1:4000/v1/models" => LITELLM,
            "https://openrouter.ai/api/v1/models?output_modalities=speech" | "https://openrouter.ai/api/v1/videos/models" => r#"{"data": []}"#,
            _ => "",
        };
        let status = if body.is_empty() { 404 } else { 200 };
        Box::pin(async move { Ok(HttpResponse { status, headers: Vec::new(), body: body.as_bytes().to_vec() }) })
    }
}

/// Reads from the fixtures, as if OpenRouter's key account and a LiteLLM address were set up.
fn fetcher(http: Arc<Fixture>) -> Fetch {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    Box::new(move |_, _| {
        let settings = ProviderSettings { litellm_url: Some("http://127.0.0.1:4000".into()), ..ProviderSettings::default() };
        let keys = nodecraft_engine::secrets::MemoryStore::default();
        let env = HostEnv { keys: &keys, http: http.clone(), settings: &settings, now_ms: Arc::new(|| 0), progress: None };
        runtime.block_on(model_catalogs::fetch(&env, true))
    })
}

fn with_fetcher(fetch: Fetch) -> Harness<'static, NodeCraftApp> {
    let mut h = harness();
    let (fetcher, mut service) = CatalogFetcher::spawn(fetch).unwrap();
    let app = h.state_mut();
    app.session.register_host_service(SERVICE, move |request| service(request)).unwrap();
    app.model_catalogs = Some(fetcher);
    h
}

/// Frames until the refresh is installed.
fn settled(h: &mut Harness<'static, NodeCraftApp>) {
    for _ in 0..500 {
        let ctx = h.ctx.clone();
        h.state_mut().tick_model_catalogs(&ctx);
        if !h.state().model_catalogs.as_ref().unwrap().is_waiting() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the refresh never finished");
}

#[test]
fn a_refresh_reads_the_lists_and_offers_their_models() {
    let http = Arc::new(Fixture::default());
    let mut h = with_fetcher(fetcher(http.clone()));
    // The harness has no settings file: nothing is set up, so the start-up refresh isn't asked.
    settled(&mut h);
    assert!(http.asked.lock().unwrap().is_empty());

    h.state_mut().session.dispatch("models.catalog_refresh", &json!({})).unwrap();
    settled(&mut h);

    let notice = h.state().notice.clone().unwrap_or_default();
    assert!(notice.contains("OpenRouter: 2 models") && notice.contains("LiteLLM: 2 models") && !notice.contains("couldn't"), "{notice}");
    let catalog = h.state().session.generation_catalog().unwrap();
    assert!(catalog.model(KIND, "openrouter.tencent/hy-image-v3.5-preview").is_some());
    assert!(catalog.model(TEXT_KIND, "litellm.local-free-model-ml").is_some());
    let asked = http.asked.lock().unwrap().clone();
    assert!(asked.iter().filter(|(url, _)| url.starts_with("https://openrouter.ai")).all(|(_, keyed)| !keyed), "OpenRouter's lists are read without its key");
}

#[test]
fn a_quiet_refresh_tells_only_failures() {
    let mut h = with_fetcher(Box::new(|_, _| {
        Fetched(
            [(model_catalogs::Source::Openrouter, model_catalogs::Listing::Failed(Error::new(ErrorKind::Timeout, "OpenRouter's model list: slow")))]
                .into_iter()
                .collect(),
        )
    }));
    let app = h.state_mut();
    app.notice = None;
    app.model_catalogs.as_mut().unwrap().refresh_quietly(&mut app.session);
    settled(&mut h);
    assert!(h.state().notice.clone().unwrap_or_default().contains("couldn't read its models"));

    let mut h =
        with_fetcher(Box::new(|_, _| Fetched([(model_catalogs::Source::Openrouter, model_catalogs::Listing::Listed(Vec::new()))].into_iter().collect())));
    h.state_mut().notice = None;
    let app = h.state_mut();
    app.model_catalogs.as_mut().unwrap().refresh_quietly(&mut app.session);
    settled(&mut h);
    assert_eq!(h.state().notice, None);
}

#[test]
fn a_panicking_read_fails_the_refresh_not_the_app() {
    let mut h = with_fetcher(Box::new(|_, _| panic!("hostile backend")));
    h.state_mut().session.dispatch("models.catalog_refresh", &json!({})).unwrap();
    settled(&mut h);
    assert!(h.state().notice.clone().unwrap_or_default().contains("failed unexpectedly"));
}

#[test]
fn refreshes_asked_while_one_waits_share_it() {
    let mut h = with_fetcher(Box::new(|_, _| Fetched::default()));
    let first = h.state_mut().session.dispatch("models.catalog_refresh", &json!({})).unwrap();
    let second = h.state_mut().session.dispatch("models.catalog_refresh", &json!({})).unwrap();
    assert_eq!(first, second);
    settled(&mut h);
}
