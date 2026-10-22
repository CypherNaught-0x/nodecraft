//! Sound lengths at import (#608): an imported WAV records how long it is, so a speech-to-video
//! mode that takes at most 7.5 s of speech (fal's Wan 2.2, on the shipped catalog) refuses a
//! longer track at the quote, before anything is sent. No network.
#![cfg(not(target_arch = "wasm32"))]
use nodecraft_engine::import::{Incoming, import_files, import_media};
use nodecraft_engine::{
    Session,
    commands::{Registry, Resources, Services},
    generation_catalog::GenerationCatalog,
    jobs::JobBudget,
    provider_routing::ProviderRouting,
    run_host::RunHost,
};
use nodecraft_exec::{
    cost::{Budgets, UsageLog},
    driver::{Runtime, TaskFuture},
};
use nodecraft_graph::{Canvas, Change, Position, Project};
use nodecraft_nodes::generation::VIDEO_KIND;
use nodecraft_providers::{BoxFuture, Http, HttpRequest, HttpResponse, ProviderError, fal::Fal, secrets::Secret};
use nodecraft_types::{CanvasId, EdgeId, MediaMeta, NodeId, ProjectId, Value};
use serde_json::json;
use std::sync::{Arc, Mutex};

const C: CanvasId = CanvasId::from_bytes([68; 16]);
const CARD: NodeId = NodeId::from_bytes([1; 16]);
const PORTRAIT: NodeId = NodeId::from_bytes([2; 16]);
const SPEECH: NodeId = NodeId::from_bytes([3; 16]);

/// Nothing is spawned or slept: a refused quote never starts a run.
struct Idle;
impl Runtime for Idle {
    fn spawn(&self, _: TaskFuture<()>) {}
    fn sleep(&self, _: u64) -> TaskFuture<()> {
        Box::pin(async {})
    }
    fn now_ms(&self) -> u64 {
        0
    }
}

/// Keeps every request; there should be none.
#[derive(Default)]
struct Transport(Mutex<Vec<HttpRequest>>);
impl Http for Transport {
    fn send(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.send_no_redirect(r)
    }
    fn send_no_redirect(&self, r: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, ProviderError>> {
        self.0.lock().unwrap().push(r);
        Box::pin(async { Ok(HttpResponse { status: 500, headers: vec![], body: vec![] }) })
    }
}

/// `ms` milliseconds of 8 kHz, 8-bit mono PCM.
fn wav(ms: u32) -> Vec<u8> {
    let data = 8 * ms;
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(36 + data).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt \x10\0\0\0\x01\0\x01\0");
    file.extend_from_slice(&8000u32.to_le_bytes());
    file.extend_from_slice(&8000u32.to_le_bytes());
    file.extend_from_slice(b"\x01\0\x08\0data");
    file.extend_from_slice(&data.to_le_bytes());
    file.resize(file.len() + usize::try_from(data).unwrap(), 0x80);
    file
}

/// A Generate video card on fal's Wan 2.2 speech to video, with a portrait and a `speech_ms`
/// track imported and wired.
fn fixture(speech_ms: u32) -> (Session, Arc<Transport>) {
    let snapshot = GenerationCatalog::builtin().unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = Project::new(ProjectId::from_bytes([68; 16]), "Speech").unwrap();
    Change::AddCanvas { canvas: Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("fal.wan22-s2v".into())), ("prompt".into(), Value::Text("A paper boat talks".into()))].into();
    let card = kinds.get(VIDEO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let budget = JobBudget {
        log: Arc::new(Mutex::new(UsageLog::default())),
        budgets: Arc::new(Mutex::new(Budgets::default())),
        session: "speech".into(),
        wall_clock_ms: Arc::new(|| 0),
    };
    let host = RunHost::new(Arc::new(Idle), budget);
    let mut services = Services::default();
    host.register(&mut services).unwrap();
    let transport = Arc::new(Transport::default());
    let mut routing = ProviderRouting::new();
    let binding = routing.bind_default(Arc::new(Fal::new(Secret::new("fixture-key-not-real").unwrap(), transport.clone()))).unwrap();
    routing.select(CARD, "fal", binding.target()).unwrap();
    let mut session = Session::with_registries(project, Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources).with_services(services);
    session.set_provider_routing(routing).unwrap();
    host.install(&mut session);
    let portrait = nodecraft_media::encode_png(4, 4, &[90; 4 * 4 * 4], &Default::default()).unwrap();
    let speech = wav(speech_ms);
    let files = [Incoming { name: "portrait.png", bytes: &portrait }, Incoming { name: "speech.wav", bytes: &speech }];
    import_files(&mut session, C, Position { x: -300.0, y: 0.0 }, 200.0, &files, &[PORTRAIT, SPEECH]).unwrap();
    for (n, from, port) in [(10, PORTRAIT, "portrait"), (11, SPEECH, "speech")] {
        let edge = json!({"id": EdgeId::from_bytes([n; 16]), "from": {"node": from, "port": "out"}, "to": {"node": CARD, "port": port}, "order": 0});
        session.dispatch("edge.connect", &json!({"canvas": C, "edge": edge})).unwrap();
    }
    (session, transport)
}

fn quote(session: &mut Session) -> Result<serde_json::Value, nodecraft_types::Error> {
    session.dispatch("run.quote", &json!({"canvas":C,"mode":"selected","nodes":[CARD]}))
}

#[test]
fn an_imported_wav_records_its_length() {
    let (mut session, _) = fixture(1000);
    let asset = import_media(&mut session, &Incoming { name: "speech.wav", bytes: &wav(2_500) }).unwrap();
    assert_eq!(asset.meta, MediaMeta { duration_ms: Some(2_500), sample_rate: Some(8000), channels: Some(1), ..MediaMeta::default() });
    // A cut-off file still imports, with its length unknown.
    let mut cut = wav(2_500);
    cut.truncate(cut.len() / 2);
    let asset = import_media(&mut session, &Incoming { name: "cut.wav", bytes: &cut }).unwrap();
    assert_eq!(asset.meta.duration_ms, None);
}

#[test]
fn speech_longer_than_the_model_takes_is_refused_at_the_quote() {
    let (mut session, _) = fixture(7_000);
    let q = quote(&mut session).unwrap();
    assert_eq!(q["allowed"], true, "a track that fits is quoted: {q}");

    // The quote names the card and a fixed reason code.
    let (mut session, transport) = fixture(8_000);
    let q = quote(&mut session).unwrap();
    assert_eq!(q["allowed"], false, "{q}");
    let blocked = q["blocked"].to_string();
    assert!(blocked.contains(&CARD.to_string()) && blocked.contains("node_validation"), "{q}");
    assert!(transport.0.lock().unwrap().is_empty(), "nothing is sent");
}
