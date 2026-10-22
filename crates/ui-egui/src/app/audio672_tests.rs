//! Generate audio cards (#672): a generated sound plays on the card, as an imported one does, and
//! the card offers no Improve prompt, since a speech model says its prompt as written.
use super::*;
use egui_kittest::{Harness, kittest::Queryable};
use nodecraft_engine::generation_catalog::GenerationCatalog;
use nodecraft_engine::run_cmds::RunCatalog;
use nodecraft_graph::Generation;
use nodecraft_types::GenerationId;

const C: nodecraft_types::CanvasId = nodecraft_types::CanvasId::from_bytes([72; 16]);
const CARD: NodeId = NodeId::from_bytes([6; 16]);

/// A speech model and a text model, so Improve prompt would have a writer if it were offered.
const CATALOG: &str = r#"
[[model]]
id = "fixture.writer"
provider = "mock"
provider_model = "mock-text"
label = "Fixture writer"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.01 USD" }
[[model.modes]]
id = "text-to-text"
output = "text"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]

[[model]]
id = "fixture.speech"
provider = "mock"
provider_model = "mock-audio"
label = "Fixture speech"
description = "Public deterministic test fixture"
quality = 1
latency_s = 0
pricing = { type = "per_call", price = "0.02 USD" }
[[model.modes]]
id = "text-to-audio"
output = "audio"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }]
"#;

/// A few bytes that sniff as WAV; enough for the card, not for ffmpeg.
fn wav_header() -> Vec<u8> {
    let mut bytes = b"RIFF\x24\x00\x00\x00WAVEfmt ".to_vec();
    bytes.resize(64, 0);
    bytes
}

/// A Generate audio card whose active result is a sound.
fn app() -> NodeCraftApp {
    let catalog = nodecraft_catalog::Catalog::from_toml(CATALOG).unwrap();
    let snapshot = GenerationCatalog::new(RunCatalog::new(catalog).unwrap()).unwrap();
    let kinds = snapshot.registry().unwrap();
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([72; 16]), "Audio").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(C, "Canvas") }.apply(&mut project).unwrap();
    let values = [("model".into(), Value::Text("fixture.speech".into())), ("prompt".into(), Value::Text("Hello there".into()))].into();
    let mut card = kinds.get(nodecraft_nodes::generation::AUDIO_KIND).unwrap().instantiate_instance(CARD, Default::default(), &values).unwrap();
    card.pos = nodecraft_graph::Position { x: 150.0, y: 100.0 };
    // Content-addressed, so a scratch session names the asset the real one will store.
    let wav = wav_header();
    let hash =
        Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([73; 16]), "Scratch").unwrap()).unwrap().import_asset(&wav).unwrap();
    let sound = nodecraft_types::AssetRef { hash, mime: "audio/wav".parse().unwrap(), meta: Default::default() };
    let generation = GenerationId::from_bytes([7; 16]);
    card.history.push(Generation::new(generation, vec![Value::Media(sound)]));
    card.active = Some(generation);
    nodecraft_graph::Change::AddNode { canvas: C, node: card }.apply(&mut project).unwrap();
    let mut resources = nodecraft_engine::commands::Resources::default();
    resources.insert(snapshot.clone());
    resources.insert(snapshot.run_catalog());
    let mut session = Session::with_registries(project, nodecraft_engine::commands::Registry::builtins().unwrap(), kinds).unwrap().with_resources(resources);
    session.import_asset(&wav).unwrap();
    NodeCraftApp::from_session(session, C).unwrap()
}

#[test]
fn a_generated_sound_plays_on_the_card_with_no_improve_prompt() {
    let mut h = Harness::builder().with_size(Vec2::new(1200.0, 850.0)).build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app());
    h.state_mut().selection.only(CARD);
    h.run();
    assert!(h.query_by_label(crate::audio::UNCONFIGURED).is_some(), "the card shows its sound's body");
    assert!(h.query_by_label("Generate").is_some(), "the card's own controls are there");
    assert!(h.query_by_label("Improve prompt").is_none(), "a spoken prompt isn't rewritten");
}
