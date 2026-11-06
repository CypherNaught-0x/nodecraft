use super::*;
use nodecraft_catalog::Unit;
use nodecraft_nodes::generation::{KIND, TEXT_KIND};
use serde_json::json;

fn bytes(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).unwrap()
}

fn chat_listing() -> Vec<u8> {
    bytes(json!({"data": [
        {"id": "meta-llama/llama-3.3-70b-instruct:free", "name": "Meta: Llama 3.3 70B Instruct (free)",
         "description": "A multilingual model. It is tuned for dialogue.",
         "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
         "pricing": {"prompt": "0", "completion": "0"}, "top_provider": {"max_completion_tokens": 4096}},
        {"id": "openai/gpt-5-mini", "name": "OpenAI: GPT-5 Mini",
         "architecture": {"input_modalities": ["text", "image", "file"], "output_modalities": ["text"]},
         "pricing": {"prompt": "0.00000025", "completion": "0.000002", "request": "0"}, "top_provider": {"max_completion_tokens": null}},
        {"id": "x/with-request-fee", "name": "Fee", "architecture": {"output_modalities": ["text"]},
         "pricing": {"prompt": "0.000001", "completion": "0.000001", "request": "0.01"}},
        {"id": "x/image-fee", "name": "Image fee", "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
         "pricing": {"prompt": "0", "completion": "0", "image": "0.001"}},
        {"id": "x/reasoning", "name": "Reasoning", "architecture": {"output_modalities": ["text"]},
         "pricing": {"prompt": "0.000001", "completion": "0.000002", "internal_reasoning": "0.000003"}},
        {"id": "openrouter/auto", "name": "Auto", "architecture": {"output_modalities": ["text"]}, "pricing": {"prompt": "-1", "completion": "-1"}},
        {"id": "google/gemini-image", "name": "Image chat", "architecture": {"output_modalities": ["image", "text"]}, "pricing": {"prompt": "0", "completion": "0"}},
        {"id": "x/negative", "name": "Negative", "architecture": {"output_modalities": ["text"]}, "pricing": {"prompt": "-1", "completion": "0.1"}},
        {"id": "x/no-pricing", "name": "None", "architecture": {"output_modalities": ["text"]}},
        {"id": "", "name": "Empty"},
        {"id": 7},
        "not an object",
        {"id": "x/\u{0007}bell", "name": "\u{0000}", "architecture": {"output_modalities": ["text"]}, "pricing": {"prompt": "0", "completion": "0"}}
    ]}))
}

fn image_listing() -> Vec<u8> {
    bytes(json!({"data": [
        {"id": "tencent/hy-image-v3.5-preview", "name": "Tencent: Hy Image 3.5 Preview",
         "description": "Hy Image 3.5 Preview is a unified image generation and editing model from Tencent. It handles more.",
         "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["image"]},
         "supported_parameters": {
            "resolution": {"type": "enum", "values": ["1K", "1.5K", "2K", "4K"]},
            "aspect_ratio": {"type": "enum", "values": ["1:1", "16:9", "9:16", "bad value!"]},
            "n": {"type": "range", "min": 1, "max": 1},
            "input_references": {"type": "range", "min": 0, "max": 20},
            "seed": {"type": "boolean"}}},
        {"id": "recraft/recraft-v4.1-flash", "name": "Recraft V4.1 Flash",
         "architecture": {"input_modalities": ["text"], "output_modalities": ["image"]},
         "supported_parameters": {"aspect_ratio": {"type": "enum", "values": ["16:9"]}}},
        {"id": "a/text-only", "architecture": {"output_modalities": ["text"]}}
    ]}))
}

fn image_prices() -> Vec<u8> {
    bytes(json!({"data": [{"id": "tencent/hy-image-v3.5-preview", "pricing": {"image_output": "0.0000016"}}]}))
}

#[test]
fn openrouter_chat_models_become_text_models_with_token_prices() {
    let models = listing::openrouter_chat(&chat_listing()).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["openrouter.meta-llama/llama-3.3-70b-instruct.free", "openrouter.openai/gpt-5-mini", "openrouter.x/image-fee", "openrouter.x/reasoning"],
        "a per-request fee can't be quoted: skipped"
    );

    let free = &models[0];
    assert_eq!((free.provider.as_str(), free.provider_model.as_str()), ("openrouter", "meta-llama/llama-3.3-70b-instruct:free"));
    assert_eq!(free.pricing, Pricing::Free);
    assert_eq!(free.modes.len(), 1, "text only");
    assert_eq!(
        free.params.params[0].default,
        Some(nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(4096))),
        "capped at the provider's longest reply"
    );

    let mini = &models[1];
    assert_eq!(mini.modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["text-to-text", "image-to-text"]);
    let Pricing::PerUnit { unit, price, count, .. } = &mini.pricing else { panic!("token quote") };
    assert_eq!((*unit, price.0.micros, count.as_deref()), (Unit::ThousandTokens, 2000, Some("max_tokens")));
    let usage = mini.usage.as_ref().unwrap();
    assert_eq!((usage.input.0.micros, usage.output.0.micros), (250, 2000));
    assert!(mini.description.contains("$0.25 / M input, $2.00 / M output"), "{}", mini.description);

    assert_eq!(models[2].pricing, Pricing::Free, "free tokens");
    assert_eq!(models[2].modes.len(), 1, "a per-image fee drops reading images, so nothing paid is sent");
    let Pricing::PerUnit { price, .. } = &models[3].pricing else { panic!("token quote") };
    assert_eq!(price.0.micros, 3000, "reasoning tokens are quoted at the higher rate");
}

#[test]
fn openrouter_image_models_take_their_listed_settings_and_are_priced_by_the_run() {
    let models = listing::openrouter_images(&image_listing(), Some(&image_prices())).unwrap();
    assert_eq!(models.len(), 2);
    let hy = &models[0];
    assert_eq!(hy.id, "openrouter.tencent/hy-image-v3.5-preview");
    assert_eq!(hy.provider, "openrouter-images");
    assert_eq!(hy.provider_model, "tencent/hy-image-v3.5-preview");
    assert!(matches!(hy.pricing, Pricing::ByUsage { .. }));
    assert!(hy.description.contains("$1.60 per million image tokens"), "{}", hy.description);
    let names: Vec<&str> = hy.params.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["resolution", "aspect_ratio", "seed"]);
    let nodecraft_types::ParamControl::Choice { options } = &hy.params.params[1].control else { panic!("choice") };
    assert_eq!(options.len(), 3, "a value outside the adapter's alphabet is dropped");
    let edit = hy.modes.iter().find(|m| m.id == "image-to-image").unwrap();
    assert_eq!(edit.inputs[1].max, 16, "capped at the adapter's reference limit");

    let recraft = &models[1];
    assert_eq!(recraft.modes.len(), 1, "no image input listed: no edit mode");
    assert_eq!(recraft.params.params[0].default, Some(nodecraft_types::Value::Text("16:9".into())), "the only value is the default");
    assert!(listing::openrouter_images(&image_listing(), None).unwrap()[0].description.contains("Priced per image"));
}

#[test]
fn litellm_models_are_chat_models_priced_by_the_proxy() {
    let models = bytes(json!({"data": [{"id": "local-free-model"}, {"id": "local-free-model-ml"}, {"id": "embedder"}, {"id": "priced"}]}));
    let info = bytes(json!({"data": [
        {"model_name": "local-free-model", "model_info": {"mode": null, "input_cost_per_token": null, "output_cost_per_token": null, "supports_vision": true, "max_output_tokens": 32768}},
        {"model_name": "embedder", "model_info": {"mode": "embedding"}},
        {"model_name": "priced", "model_info": {"mode": "chat", "input_cost_per_token": 1e-6, "output_cost_per_token": 2e-6}}
    ]}));
    let listed = listing::litellm(&models, Some(&info)).unwrap();
    let ids: Vec<&str> = listed.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["litellm.local-free-model", "litellm.local-free-model-ml", "litellm.priced"]);
    assert_eq!(listed[0].provider, "litellm");
    assert!(matches!(listed[0].pricing, Pricing::ByUsage { .. }), "no price is never free");
    assert_eq!(listed[0].modes.len(), 2, "reads images");
    assert_eq!(listed[1].modes.len(), 1, "no details: text only");
    assert!(matches!(listed[2].pricing, Pricing::PerUnit { .. }));

    let without_info = listing::litellm(&models, None).unwrap();
    assert_eq!(without_info.len(), 4, "every model is offered when details can't be read");
}

#[test]
fn ids_map_into_the_catalog_alphabet_or_are_skipped() {
    assert_eq!(listing::catalog_id("openrouter", "Qwen/Qwen3:Free").as_deref(), Some("openrouter.qwen/qwen3.free"));
    assert_eq!(listing::catalog_id("litellm", "gpt 4o").as_deref(), Some("litellm.gpt-4o"));
    assert_eq!(listing::catalog_id("x", "a//b"), None);
    assert_eq!(listing::catalog_id("x", "a/../etc"), None);
    assert_eq!(listing::catalog_id("x", ""), None);
    assert_eq!(listing::catalog_id("x", &"a".repeat(500)), None);
}

#[test]
fn hostile_listings_are_errors_or_skipped_never_panics() {
    for body in [&b"not json"[..], b"[]", b"{}", b"{\"data\": 3}", b"null", b"{\"data\": [null, 1, [], {\"id\": null}]}"] {
        let _ = listing::openrouter_chat(body);
        let _ = listing::openrouter_images(body, Some(body));
        let _ = listing::litellm(body, Some(body));
    }
    let deep = format!("{{\"data\": [{}]}}", "[".repeat(10_000));
    assert!(listing::openrouter_chat(deep.as_bytes()).is_err());
    let huge = bytes(
        json!({"data": (0..5000).map(|i| json!({"id": format!("m/{i}"), "architecture": {"output_modalities": ["text"]}, "pricing": {"prompt": "0", "completion": "0"}})).collect::<Vec<_>>()}),
    );
    assert_eq!(listing::openrouter_chat(&huge).unwrap().len(), listing::MAX_ENTRIES, "a listing is read only so far");
}

fn session() -> Session {
    Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([7; 16]), "P").unwrap()).unwrap()
}

fn fetched(listings: impl IntoIterator<Item = (Source, Listing)>) -> Fetched {
    Fetched(listings.into_iter().collect())
}

#[test]
fn finish_publishes_listed_models_next_to_the_curated_ones() {
    let mut session = session();
    let curated = session.generation_catalog().unwrap().models(KIND).count();
    let mut models = listing::openrouter_chat(&chat_listing()).unwrap();
    models.extend(listing::openrouter_images(&image_listing(), None).unwrap());
    let litellm = listing::litellm(&bytes(json!({"data": [{"id": "local-free-model"}]})), None).unwrap();
    let report = finish(&mut session, 1, fetched([(Source::Openrouter, Listing::Listed(models)), (Source::Litellm, Listing::Listed(litellm))]));
    assert_eq!(report.error, None);
    assert_eq!((report.offered, report.left_out), (7, 0));
    let catalog = session.generation_catalog().unwrap();
    assert_eq!(catalog.models(KIND).count(), curated + 2);
    assert!(catalog.model(KIND, "openrouter.tencent/hy-image-v3.5-preview").is_some());
    assert!(catalog.model(TEXT_KIND, "litellm.local-free-model").is_some());
    assert!(catalog.model(TEXT_KIND, "openrouter.meta-llama/llama-3.3-70b-instruct.free").is_some());
    assert!(report.summary().contains("OpenRouter: 6 models"), "{}", report.summary());

    // A failed read keeps what was read before; a source no longer set up loses its models.
    let report = finish(
        &mut session,
        2,
        fetched([(Source::Openrouter, Listing::Failed(Error::new(ErrorKind::Timeout, "slow"))), (Source::Litellm, Listing::Off("no address".into()))]),
    );
    let catalog = session.generation_catalog().unwrap();
    assert!(catalog.model(KIND, "openrouter.tencent/hy-image-v3.5-preview").is_some());
    assert!(catalog.model(TEXT_KIND, "litellm.local-free-model").is_none());
    assert_eq!(report.offered, 6);
    assert_eq!(last(&session).map(|r| r.id), Some(2));
}

#[test]
fn a_full_catalog_keeps_the_highest_priority_models_and_says_how_many_it_left_out() {
    let mut session = session();
    let many: Vec<Value> = (0..MAX_LISTED + 100)
        .map(|i| json!({"id": format!("vendor/model-{i}"), "name": format!("Model {i}"), "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}, "pricing": {"prompt": "0.000001", "completion": "0.000002"}}))
        .collect();
    let models = listing::openrouter_chat(&bytes(json!({"data": many}))).unwrap();
    let images = listing::openrouter_images(&image_listing(), None).unwrap();
    let all = models.into_iter().chain(images).collect();
    let report = finish(&mut session, 1, fetched([(Source::Openrouter, Listing::Listed(all))]));
    assert_eq!(report.error, None);
    assert!(report.offered > 0 && report.offered <= MAX_LISTED, "{report:?}");
    let catalog = session.generation_catalog().unwrap();
    assert!(catalog.model(KIND, "openrouter.tencent/hy-image-v3.5-preview").is_some(), "image models come first");
}

#[test]
fn listed_ids_never_shadow_curated_models() {
    let mut session = session();
    let curated = session.generation_catalog().unwrap().models(TEXT_KIND).next().unwrap().id.clone();
    let (prefix, rest) = curated.split_once('.').unwrap();
    let mut clash = listing::litellm(&bytes(json!({"data": [{"id": rest}]})), None).unwrap();
    clash[0].id = format!("{prefix}.{rest}");
    let report = finish(&mut session, 1, fetched([(Source::Litellm, Listing::Listed(clash))]));
    assert_eq!((report.offered, report.error), (0, None));
    assert_ne!(session.generation_catalog().unwrap().model(TEXT_KIND, &curated).unwrap().provider, "litellm", "the curated model stays");
}

#[test]
fn refresh_needs_the_hosts_fetch_service() {
    let mut session = session();
    let error = session.dispatch("models.catalog_refresh", &json!({})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported);
    let status = session.dispatch("models.catalog_status", &json!({})).unwrap();
    assert_eq!(status["last"], Value::Null);
    assert!(session.dispatch("models.catalog_refresh", &json!({"extra": 1})).is_err());
}

#[test]
fn openrouter_speech_models_take_their_voices_and_character_prices() {
    let body = bytes(json!({"data": [
        {"id": "hexgrad/kokoro-82m", "name": "Kokoro 82M", "architecture": {"input_modalities": ["text"], "output_modalities": ["speech"]},
         "pricing": {"prompt": "0.00000062", "completion": "0"}, "supported_voices": ["af_bella", "am_adam", "bad voice!", "af_bella"]},
        {"id": "fish-audio/s2.1-pro-free:free", "name": "Fish (free)", "architecture": {"output_modalities": ["speech"]}, "pricing": {"prompt": "0", "completion": "0"}, "supported_voices": []},
        {"id": "bytedance-seed/seed-audio-1-0", "name": "Seed Audio", "architecture": {"output_modalities": ["speech"]}, "pricing": {"prompt": "0", "completion": "0.0025"}},
        {"id": "x/unpriced", "name": "No pricing", "architecture": {"output_modalities": ["speech"]}},
        {"id": "x/empty-pricing", "name": "Empty pricing", "architecture": {"output_modalities": ["speech"]}, "pricing": {}},
        {"id": "x/two-fees", "name": "Two fees", "architecture": {"output_modalities": ["speech"]}, "pricing": {"prompt": "0.00001", "request": "0.01"}},
        {"id": "heygen/heygen-voice-1", "name": "PCM only", "architecture": {"output_modalities": ["speech"]}, "pricing": {"prompt": "0.000015"}},
        {"id": "x/text", "architecture": {"output_modalities": ["text"]}}
    ]}));
    let models = listing::openrouter_speech(&body).unwrap();
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["openrouter.hexgrad/kokoro-82m", "openrouter.fish-audio/s2.1-pro-free.free"],
        "PCM-only models, and prices that can't be quoted or read back, are left out"
    );
    let kokoro = &models[0];
    assert_eq!(kokoro.provider, "openrouter-speech");
    let Pricing::PerUnit { unit, price, .. } = &kokoro.pricing else { panic!("per character") };
    assert_eq!((*unit, price.0.micros), (Unit::ThousandCharacters, 620));
    let nodecraft_types::ParamControl::Choice { options } = &kokoro.params.params[0].control else { panic!("voices") };
    assert_eq!(options.len(), 2, "invalid and repeated voices are dropped");
    assert_eq!(models[1].pricing, Pricing::Free);
    assert!(models[1].params.params.is_empty(), "no voices listed: the model's default");
}

#[test]
fn openrouter_video_models_take_their_listed_lengths_frames_and_prices() {
    let body = bytes(json!({"data": [
        {"id": "minimax/hailuo-2.3", "name": "MiniMax: Hailuo 2.3", "supported_resolutions": ["1080p"], "supported_aspect_ratios": ["16:9"],
         "supported_durations": [10, 6], "supported_frame_images": ["first_frame"], "generate_audio": false, "seed": null, "pricing_skus": {"duration_seconds": "0.0817"}},
        {"id": "google/veo-3.1-lite", "name": "Google: Veo 3.1 Lite", "supported_resolutions": ["720p", "1080p"], "supported_aspect_ratios": ["16:9", "9:16"],
         "supported_durations": [8, 4, 6], "supported_frame_images": ["first_frame", "last_frame"], "generate_audio": true, "seed": true,
         "pricing_skus": {"duration_seconds_with_audio": "0.08", "duration_seconds_without_audio_720p": "0.03"}},
        {"id": "x/no-durations", "name": "Odd", "pricing_skus": {"duration_seconds": "0.1"}},
        {"id": "x/tokens", "name": "Tokens", "supported_durations": [5], "pricing_skus": {"duration_seconds": "0.1", "video_tokens": "0.000001"}},
        {"id": 7}
    ]}));
    let models = listing::openrouter_video(&body).unwrap();
    assert_eq!(models.len(), 4);
    assert!(matches!(models[3].pricing, Pricing::ByUsage { .. }), "a token price isn't bounded by seconds");
    let hailuo = &models[0];
    assert_eq!((hailuo.id.as_str(), hailuo.provider.as_str()), ("openrouter.minimax/hailuo-2.3", "openrouter-video"));
    let Pricing::PerUnit { unit, price, count, .. } = &hailuo.pricing else { panic!("per second") };
    assert_eq!((*unit, price.0.micros, count.as_deref()), (Unit::Second, 81_700, Some("duration")));
    let duration = &hailuo.params.params[0];
    assert_eq!(duration.default, Some(nodecraft_types::Value::Unsigned(nodecraft_types::Unsigned(6))));
    assert_eq!(hailuo.modes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["text-to-video", "image-to-video"]);

    let veo = &models[1];
    let Pricing::PerUnit { price, .. } = &veo.pricing else { panic!("an upper-bound per-second quote") };
    assert_eq!(price.0.micros, 80_000, "the highest listed rate");
    assert!(veo.description.contains("highest rate"), "{}", veo.description);
    let names: Vec<&str> = veo.params.params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["duration", "resolution", "aspect_ratio", "generate_audio", "seed"]);
    assert_eq!(veo.modes.len(), 3, "first and last frames");

    assert!(matches!(models[2].pricing, Pricing::ByUsage { .. }), "no lengths listed: no per-second quote");
}

#[test]
fn plugins_are_never_crowded_out_by_listed_models_whichever_is_published_first() {
    let mut session = session();
    let many: Vec<Value> = (0..MAX_LISTED + 100)
        .map(|i| json!({"id": format!("vendor/model-{i}"), "name": format!("Model {i}"), "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]}, "pricing": {"prompt": "0.000001", "completion": "0.000002"}}))
        .collect();
    let listed = listing::openrouter_chat(&bytes(json!({"data": many}))).unwrap();
    let report = finish(&mut session, 1, fetched([(Source::Openrouter, Listing::Listed(listed))]));
    assert!(report.left_out > 0, "the budget is full of listed models");
    let mut plugin = session
        .generation_catalog()
        .unwrap()
        .models_except(&LISTED_PROVIDERS)
        .find(|m| m.modes.iter().any(|mode| mode.output == nodecraft_types::Modality::Image))
        .unwrap()
        .clone();
    plugin.id = "plugin.fixture/model".into();
    plugin.provider = format!("{}fixture", nodecraft_plugin::id::PLUGIN_PREFIX);
    let with_plugin = session.generation_catalog().unwrap().with_plugin_models(vec![vec![plugin]]).unwrap();
    assert!(with_plugin.model(KIND, "plugin.fixture/model").is_some(), "the plugin's model is admitted first");
    assert!(with_plugin.models(TEXT_KIND).any(|m| m.provider == "openrouter"), "listed models fill what is left");
}

#[test]
fn a_list_that_fails_keeps_its_kinds_models_from_before() {
    let mut session = session();
    let mut first = listing::openrouter_chat(&chat_listing()).unwrap();
    first.extend(listing::openrouter_images(&image_listing(), None).unwrap());
    finish(&mut session, 1, fetched([(Source::Openrouter, Listing::Listed(first))]));
    // The next read gets chat models only: the image list failed.
    let chat_only = listing::openrouter_chat(&chat_listing()).unwrap();
    let report = finish(
        &mut session,
        2,
        fetched([(Source::Openrouter, Listing::Partial(chat_only, vec![nodecraft_providers::openrouter_images::ID], Error::new(ErrorKind::Timeout, "slow")))]),
    );
    assert!(session.generation_catalog().unwrap().model(KIND, "openrouter.tencent/hy-image-v3.5-preview").is_some(), "image models from before stay");
    assert!(report.summary().contains("models from before stay"), "{}", report.summary());
}

#[test]
fn listing_errors_never_carry_the_key() {
    assert_eq!(
        masked("HTTP 500: echo sk-secret-123 back", Some(&nodecraft_providers::secrets::Secret::new("sk-secret-123").unwrap())),
        "HTTP 500: echo … back"
    );
    assert_eq!(masked("no key here", None), "no key here");
}
