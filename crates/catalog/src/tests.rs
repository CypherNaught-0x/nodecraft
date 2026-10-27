use super::*;
use nodecraft_types::{Currency, Money, Unsigned, Value};
use proptest::prelude::*;

const PRICED: &str = r#"
[[model]]
id = "test.video"
provider = "test"
provider_model = "video-v1"
label = "Test Video"
description = "First and last frame video."
latency_s = 90
quality = 5
pricing = { type = "per_unit", unit = "second", price = "0.05 USD", count = "duration", default_count = 5 }

[[model.modes]]
id = "text-to-video"
output = "video"
inputs = [{ role = "prompt", modality = "text", min = 1, max = 1 }]

[[model.modes]]
id = "frames-to-video"
output = "video"
inputs = [
  { role = "prompt", modality = "text", min = 0, max = 1 },
  { role = "first_frame", modality = "image", min = 1, max = 1 },
  { role = "last_frame", modality = "image", min = 0, max = 1 },
]

[[model.params.params]]
name = "duration"
label = "Duration"
control = { type = "unsigned", min = "1", max = "10" }
default = { type = "unsigned", value = "5" }

[[model.params.params]]
name = "seed"
label = "Seed"
control = { type = "seed" }
"#;

fn catalog() -> Catalog {
    let mut catalog = Catalog::builtin().unwrap();
    catalog.merge(Catalog::from_toml(PRICED).unwrap()).unwrap();
    catalog
}

#[test]
fn builtin_prices_preserve_free_unknown_and_documented_estimates() {
    let catalog = Catalog::builtin().unwrap();
    assert!(catalog.len() >= 2);
    for model in catalog.models() {
        if model.provider == "comfyui" {
            assert_eq!(model.pricing, Pricing::Free, "{}", model.id);
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::zero(Currency::USD)));
        } else if model.id == "fal.trellis2" {
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 250_000)));
        } else if matches!(model.id.as_str(), "fal.luma-ray32" | "fal.luma-ray32-i2v") {
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 500_000)));
        } else if model.id.starts_with("fal.hunyuan3d-v3") {
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 375_000)));
        } else if model.id.starts_with("fal.rodin25") {
            let micros = if model.id.contains("-fast") { 100_000 } else { 400_000 };
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, micros)));
        } else if model.id.starts_with("fal.qwen-image3") {
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 40_000)));
        } else if model.id.starts_with("fal.wan3") {
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 500_000)));
        } else if model.id.starts_with("fal.grok-") {
            let micros = match model.id.as_str() {
                "fal.grok-image2" => Some(60_000),
                "fal.grok-image2-edit" => None,
                "fal.grok-video15" => Some(840_000),
                "fal.grok-video15-i2v" => Some(850_000),
                other => panic!("{other}: no verified Grok price"),
            };
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), micros.map(|n| Money::new(Currency::USD, n)));
        } else if model.id == "fal.topaz-precision" {
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 80_000)));
        } else if model.id == "fal.bria-rmbg" {
            assert!(matches!(&model.pricing, Pricing::PerCall { .. }));
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 18_000)));
        } else if model.id == "fal.flux-lora-trainer" {
            // 2 USD per 1000 training steps, checked 2026-10-21; 1000 steps by default.
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 2_000_000)));
            let mut long = ParamValues::new();
            long.insert("steps".into(), Value::Unsigned(Unsigned(2500)));
            assert_eq!(model.quote(&long).unwrap(), Some(Money::new(Currency::USD, 5_000_000)));
        } else if model.provider == "tripo" {
            let micros = if model.id == "tripo.text-to-3d" { 200_000 } else { 300_000 };
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, micros)), "{}", model.id);
        } else if model.provider == "meshy" {
            let micros = match model.id.as_str() {
                "meshy.text-to-3d" => 400_000,
                "meshy.image-to-3d" | "meshy.multi-image-to-3d" => 600_000,
                "meshy.remesh" => 100_000,
                _ => 200_000,
            };
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, micros)), "{}", model.id);
        } else if model.provider == "openai" || model.provider == "anthropic" {
            // Chat models (#503): the output price per 1000 tokens, counted on max_tokens (16000 by default).
            let Pricing::PerUnit { unit: Unit::ThousandTokens, price, count: Some(count), default_count: None } = &model.pricing else {
                panic!("{}: {:?}", model.id, model.pricing)
            };
            assert_eq!(count, "max_tokens", "{}", model.id);
            // Output USD per 1000 tokens from the providers' pricing pages, checked 2026-10-21.
            let per_thousand = match model.id.as_str() {
                "anthropic.claude-opus-5-5" => 20_000,
                "anthropic.claude-sonnet-5-5" => 10_000,
                // GPT-6.1 Sol and GPT-6 Luna at their rate for prompts over 272K tokens (1.5x).
                "openai.gpt-6.1-sol" => 15_000,
                "anthropic.claude-haiku-5-5" => 2_500,
                "openai.gpt-6-luna" => 750,
                "openai.gpt-4.1-mini" => 1_600,
                other => panic!("{other}: no verified price in this test"),
            };
            assert_eq!(price.0, Money::new(Currency::USD, per_thousand), "{}", model.id);
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, per_thousand * 16)), "{}", model.id);
            let mut short = ParamValues::new();
            short.insert("max_tokens".into(), Value::Unsigned(Unsigned(1000)));
            assert_eq!(model.quote(&short).unwrap(), Some(Money::new(Currency::USD, per_thousand)), "{}", model.id);
        } else if model.provider == "elevenlabs" {
            // ElevenLabs pricing page, checked 2026-10-21.
            match model.id.as_str() {
                // Speech per 1000 characters, known only once the text is: no quote before the run.
                "elevenlabs.v3" | "elevenlabs.multilingual-v2" | "elevenlabs.flash-v2-5" => {
                    let Pricing::PerUnit { unit: Unit::ThousandCharacters, price, count: None, default_count: None } = &model.pricing else {
                        panic!("{}: {:?}", model.id, model.pricing)
                    };
                    let micros = if model.id == "elevenlabs.flash-v2-5" { 40_000 } else { 80_000 };
                    assert_eq!(price.0, Money::new(Currency::USD, micros), "{}", model.id);
                    assert_eq!(model.quote(&ParamValues::new()).unwrap(), None, "{}", model.id);
                }
                // 0.12 USD a minute of sound, 5 seconds by default.
                "elevenlabs.sound-effects" => {
                    assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 10_000)));
                    let mut long = ParamValues::new();
                    long.insert("duration_seconds".into(), Value::Number(nodecraft_types::Number::new(30.0).unwrap()));
                    assert_eq!(model.quote(&long).unwrap(), Some(Money::new(Currency::USD, 60_000)));
                }
                // 0.15 USD a minute, 30 seconds by default.
                "elevenlabs.music" => {
                    assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 75_000)));
                    let mut long = ParamValues::new();
                    long.insert("duration".into(), Value::Unsigned(Unsigned(600)));
                    assert_eq!(model.quote(&long).unwrap(), Some(Money::new(Currency::USD, 1_500_000)));
                }
                "elevenlabs.voice-changer" => assert_eq!(model.quote(&ParamValues::new()).unwrap(), None),
                other => panic!("{other}: no verified price in this test"),
            }
        } else if model.provider == "stability" {
            let micros = match model.id.as_str() {
                "stability.image-core" => 30_000,
                "stability.audio3" => 80_000,
                "stability.fast-3d" => 100_000,
                other => panic!("{other}: no verified Stability price"),
            };
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, micros)));
        } else if model.provider == "seedream" {
            assert!(matches!(&model.pricing, Pricing::PerCall { .. }));
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 40_000)));
        } else if model.id.starts_with("higgsfield.genjutsu-") {
            // Priced per source second (#179): no quote until the source's length is known.
            assert!(matches!(&model.pricing, Pricing::PerSourceSecond { .. }));
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), None);
        } else if model.provider == "google-tts" {
            // Cloud Text-to-Speech pricing page, checked 2026-10-23.
            if model.provider_model.starts_with("gemini-") {
                // Text tokens in, audio tokens out, and the reply reports neither.
                assert!(matches!(&model.pricing, Pricing::ByUsage { .. }), "{}", model.id);
            } else {
                // Per character sent, known only once the text is: no quote before the run.
                let Pricing::PerUnit { unit: Unit::ThousandCharacters, price, count: None, default_count: None } = &model.pricing else {
                    panic!("{}: {:?}", model.id, model.pricing)
                };
                let micros = match model.id.as_str() {
                    "google-tts.chirp3-hd" => 30_000,
                    "google-tts.neural2" => 16_000,
                    "google-tts.wavenet" | "google-tts.standard" => 4_000,
                    "google-tts.studio" => 160_000,
                    other => panic!("{other}: no verified price in this test"),
                };
                assert_eq!(price.0, Money::new(Currency::USD, micros), "{}", model.id);
            }
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), None, "{}", model.id);
        } else if model.provider == "runway" {
            let rate = match model.id.as_str() {
                "runway.gen4-5" => 120_000,
                "runway.gen4-turbo" => 50_000,
                other => panic!("{other}: no verified Runway price"),
            };
            assert!(matches!(&model.pricing, Pricing::PerUnit { unit: Unit::Second, .. }));
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, rate * 5)));
        } else if model.provider == "seedance" {
            // Per second by resolution (#18); quoted in seedance_is_quoted_per_second_at_its_largest_size.
            assert!(matches!(&model.pricing, Pricing::PerUnitByChoice { unit: Unit::Second, .. }), "{}", model.id);
            assert!(model.quote(&ParamValues::new()).unwrap().is_some(), "{}", model.id);
        } else if model.provider == "google-gemini-images" {
            // Per image by size (#47); quoted in gemini_images_on_google_cloud_are_quoted_per_image_by_size.
            assert!(matches!(&model.pricing, Pricing::PerUnitByChoice { unit: Unit::Image, .. } | Pricing::PerUnit { unit: Unit::Image, .. }), "{}", model.id);
            assert!(model.quote(&ParamValues::new()).unwrap().is_some() && model.usage.is_some(), "{}", model.id);
        } else if model.provider == "google-veo" {
            // Per second by resolution (#742); quoted in veo_on_google_cloud_is_quoted_by_length_and_resolution.
            assert!(matches!(&model.pricing, Pricing::PerUnitByChoice { unit: Unit::Second, .. }), "{}", model.id);
            assert!(model.quote(&ParamValues::new()).unwrap().is_some(), "{}", model.id);
        } else if model.id == "fal.ideogram-v3" {
            // Fixed Balanced rendering and one output (#34), checked 2026-10-27.
            assert!(matches!(&model.pricing, Pricing::PerCall { .. }));
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 60_000)));
        } else if model.id == "higgsfield.soul-id-trainer" {
            // Soul ID training's list price per request (#680), checked 2026-10-22.
            assert!(matches!(&model.pricing, Pricing::PerCall { .. }));
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 2_500_000)));
        } else {
            assert_eq!(model.pricing, Pricing::ByUsage { currency: Currency::USD });
            assert_eq!(model.quote(&ParamValues::new()).unwrap(), None);
        }
    }
}

#[test]
fn the_picker_enables_models_that_take_what_is_wired_and_explains_the_rest() {
    let catalog = catalog();
    let video = catalog.get("test.video").unwrap();
    // A prompt alone: text-to-video.
    let prompt = Wiring::new(Modality::Video).with(Modality::Text, 1);
    assert_eq!(prompt.fit(video).map(|m| m.id.as_str()), Ok("text-to-video"));
    // Two frames: the frames mode, prompt optional.
    let frames = Wiring::new(Modality::Video).with(Modality::Image, 2);
    assert_eq!(frames.fit(video).map(|m| m.id.as_str()), Ok("frames-to-video"));
    let three = Wiring::new(Modality::Video).with(Modality::Image, 3);
    assert_eq!(three.fit(video).unwrap_err().message(), "Takes at most 2 image inputs");
    let audio = Wiring::new(Modality::Video).with(Modality::Text, 1).with(Modality::Audio, 1);
    assert_eq!(audio.fit(video).unwrap_err().message(), "Doesn't take audio inputs");
    let nothing = Wiring::new(Modality::Video);
    assert_eq!(nothing.fit(video).unwrap_err().message(), "Needs at least 1 text input");
    let flux = catalog.get("comfyui.flux-dev").unwrap();
    assert_eq!(prompt.fit(flux).unwrap_err().message(), "Doesn't make video output");
    // A fresh image node with its own prompt filled in and nothing wired can use text-to-image.
    let fresh = Wiring::new(Modality::Image).with_inline_prompt();
    assert_eq!(fresh.fit(flux).map(|m| m.id.as_str()), Ok("text-to-image"));
    assert_eq!(Wiring::new(Modality::Image).fit(flux).unwrap_err().message(), "Needs at least 1 text input");
    // A wired prompt replaces the inline one rather than adding a second.
    assert!(fresh.clone().with(Modality::Text, 1).fit(flux).is_ok());
    assert_eq!(fresh.with(Modality::Text, 2).fit(flux).unwrap_err().message(), "Takes at most 1 text input");
    // With no output asked for, any mode that takes the inputs fits.
    let any = Wiring { output: None, inputs: vec![(Modality::Image, 2)], inline_prompt: false };
    assert_eq!(any.fit(video).map(|m| m.id.as_str()), Ok("frames-to-video"));

    // Candidates: compatible first; then provider, best quality first, label.
    let image = Wiring::new(Modality::Image).with(Modality::Text, 1);
    let candidates = catalog.candidates(&image);
    let order: Vec<(&str, bool)> = candidates.iter().map(|c| (c.model.id.as_str(), c.fit.is_ok())).collect();
    assert_eq!(
        order,
        vec![
            ("bfl.flux-2-klein-4b", true),
            ("comfyui.flux-dev", true),
            ("comfyui.sdxl", true),
            ("fal.flux-schnell", true),
            ("fal.grok-image2", true),
            ("fal.ideogram-v3", true),
            ("fal.luma-photon", true),
            ("fal.qwen-image3", true),
            ("gemini-images.pro", true),
            ("gemini-images.nano-banana-2.1", true),
            ("gemini-images.flash-lite", true),
            ("google-gemini-images.3-pro", true),
            ("google-gemini-images.3.1-flash", true),
            ("google-gemini-images.nano-banana-2.1", true),
            ("google-gemini-images.2.5-flash", true),
            ("google-gemini-images.3.1-flash-lite", true),
            ("higgsfield.soul-2", true),
            ("kling.image_v2_1", true),
            ("openai-images.flare", true),
            ("openai-images.sunburst", true),
            ("replicate.sdxl", true),
            ("seedream.4_5", true),
            ("stability.image-core", true),
            ("anthropic.claude-opus-5-5", false),
            ("anthropic.claude-sonnet-5-5", false),
            ("anthropic.claude-haiku-5-5", false),
            ("azure-openai.deployment", false),
            ("deepgram.nova-3", false),
            ("elevenlabs.v3", false),
            ("elevenlabs.multilingual-v2", false),
            ("elevenlabs.music", false),
            ("elevenlabs.sound-effects", false),
            ("elevenlabs.voice-changer", false),
            ("elevenlabs.flash-v2-5", false),
            ("fal.flux-lora", false),
            ("fal.ltx23", false),
            ("fal.ltx23-i2v", false),
            ("fal.topaz-precision", false),
            ("fal.bria-rmbg", false),
            ("fal.esrgan", false),
            ("fal.flux-lora-trainer", false),
            ("fal.grok-image2-edit", false),
            ("fal.grok-video15", false),
            ("fal.grok-video15-i2v", false),
            ("fal.hunyuan3d-v3-image", false),
            ("fal.hunyuan3d-v3-sketch", false),
            ("fal.hunyuan3d-v3", false),
            ("fal.kokoro", false),
            ("fal.luma-ray32", false),
            ("fal.luma-ray32-i2v", false),
            ("fal.qwen-image3-edit", false),
            ("fal.rodin25-fast-images", false),
            ("fal.rodin25-fast", false),
            ("fal.rodin25-images", false),
            ("fal.rodin25", false),
            ("fal.trellis2", false),
            ("fal.wan22", false),
            ("fal.wan22-i2v", false),
            ("fal.wan22-s2v", false),
            ("fal.wan3", false),
            ("fal.wan3-i2v", false),
            ("gemini-llm.3.8-flash", false),
            ("gemini-video.omni-flash", false),
            ("google-tts.gemini-2.5-pro", false),
            ("google-tts.chirp3-hd", false),
            ("google-tts.gemini-2.5-flash", false),
            ("google-tts.gemini-3.1-flash", false),
            ("google-tts.neural2", false),
            ("google-tts.studio", false),
            ("google-tts.gemini-2.5-flash-lite", false),
            ("google-tts.wavenet", false),
            ("google-tts.standard", false),
            ("google-veo.veo-3.1", false),
            ("google-veo.veo-3.1-fast", false),
            ("google-veo.veo-3.1-lite", false),
            ("higgsfield.genjutsu-motion-transfer", false),
            ("higgsfield.genjutsu-object-swap", false),
            ("higgsfield.genjutsu-restyle", false),
            ("higgsfield.soul-id-trainer", false),
            ("higgsfield.kling-2.5-turbo-i2v", false),
            ("huggingface.gpt-oss-120b-groq", false),
            ("kling.v2_1_master", false),
            ("kling.v2_5_turbo", false),
            ("kling.v1_6", false),
            ("meshy.image-to-3d", false),
            ("meshy.multi-image-to-3d", false),
            ("meshy.remesh", false),
            ("meshy.retexture", false),
            ("meshy.retexture-image", false),
            ("meshy.text-to-3d", false),
            ("openai.gpt-6.1-sol", false),
            ("openai.gpt-6-luna", false),
            ("openai.gpt-4.1-mini", false),
            ("openai-audio.transcribe", false),
            ("openai-audio.mini-tts", false),
            ("replicate.hello", false),
            ("runway.gen4-5", false),
            ("runway.gen4-turbo", false),
            ("seedance.2-0", false),
            ("seedance.2-5", false),
            ("seedance.2-0-fast", false),
            ("seedance.2-0-mini", false),
            ("stability.audio3", false),
            ("stability.fast-3d", false),
            ("test.video", false),
            ("tripo.image-to-3d", false),
            ("tripo.multiview-to-3d", false),
            ("tripo.text-to-3d", false)
        ]
    );
    // Wiring an image narrows it to image-to-image models.
    let edit = Wiring::new(Modality::Image).with(Modality::Text, 1).with(Modality::Image, 1);
    let bria = catalog.get("fal.bria-rmbg").unwrap();
    assert_eq!(Wiring::new(Modality::Image).with(Modality::Image, 1).fit(bria).map(|mode| mode.id.as_str()), Ok("remove-background"));
    assert!(Wiring::new(Modality::Image).fit(bria).is_err(), "one image is required");
    assert!(Wiring::new(Modality::Image).with(Modality::Image, 2).fit(bria).is_err(), "extra images are refused");
    assert!(edit.fit(bria).is_err(), "a wired prompt cannot select the differently priced guided operation");
    let groups = catalog.by_provider(&edit);
    let comfy: Vec<(&str, bool)> = groups.get("comfyui").unwrap().iter().map(|c| (c.model.id.as_str(), c.fit.is_ok())).collect();
    assert_eq!(comfy, vec![("comfyui.flux-dev", true), ("comfyui.sdxl", false)]);
    // A wired 3D model picks the Meshy models that take one; the prompt picks retexture.
    let retexture = catalog.get("meshy.retexture").unwrap();
    let remesh = catalog.get("meshy.remesh").unwrap();
    let styled = catalog.get("meshy.retexture-image").unwrap();
    let mesh_in = Wiring::new(Modality::Mesh).with(Modality::Mesh, 1);
    assert!(mesh_in.clone().fit(remesh).is_ok());
    assert!(mesh_in.clone().with(Modality::Image, 1).fit(styled).is_ok());
    assert!(mesh_in.clone().fit(styled).is_err(), "the style image is required");
    assert!(Wiring::new(Modality::Mesh).fit(remesh).is_err(), "remesh needs a model");
    assert!(mesh_in.clone().with_inline_prompt().fit(retexture).is_ok());
    assert!(mesh_in.clone().fit(retexture).is_err(), "retexture needs a prompt");
    let seedream = catalog.get("seedream.4_5").unwrap();
    assert!(Wiring::new(Modality::Image).with(Modality::Text, 1).with(Modality::Image, 14).fit(seedream).is_ok());
    assert!(Wiring::new(Modality::Image).with(Modality::Text, 1).with(Modality::Image, 15).fit(seedream).is_err());
}

#[test]
fn quotes_follow_the_counted_parameter() {
    let catalog = catalog();
    let video = catalog.get("test.video").unwrap();
    let mut params = ParamValues::new();
    assert_eq!(video.quote(&params).unwrap(), Some(Money::new(Currency::USD, 250_000)));
    params.insert("duration".into(), Value::Unsigned(Unsigned(8)));
    assert_eq!(video.quote(&params).unwrap(), Some(Money::new(Currency::USD, 400_000)));
    // The schema default (not `default_count`) is what runs, so it's what is quoted.
    let defaulted = Catalog::from_toml(&PRICED.replace("default_count = 5", "default_count = 1")).unwrap();
    assert_eq!(defaulted.get("test.video").unwrap().quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 250_000)));
}

#[test]
fn swapping_models_keeps_what_fits_and_lists_what_was_dropped() {
    let catalog = catalog();
    let (sdxl, flux) = (catalog.get("comfyui.sdxl").unwrap(), catalog.get("comfyui.flux-dev").unwrap());
    let mut params = ParamValues::new();
    params.insert("steps".into(), Value::Unsigned(Unsigned(90)));
    params.insert("cfg".into(), Value::Number(nodecraft_types::Number::new(4.5).unwrap()));
    params.insert("seed".into(), Value::Unsigned(Unsigned(7)));
    // SDXL → FLUX: seed and steps (within FLUX's 1-100) carry over; FLUX has no cfg.
    let migration = migrate(&params, flux).unwrap();
    assert_eq!(migration.params.get("seed"), Some(&Value::Unsigned(Unsigned(7))));
    assert_eq!(migration.params.get("steps"), Some(&Value::Unsigned(Unsigned(90))));
    assert_eq!(migration.params.get("aspect_ratio"), Some(&Value::Text("1:1".into())));
    assert_eq!((migration.removed.clone(), migration.reset.clone()), (vec!["cfg".to_owned()], vec![]));
    assert_eq!(migration.notice(sdxl, flux).as_deref(), Some("Switched to FLUX.1 [dev] (local): removed Guidance (not available there)."));
    // FLUX → SDXL: steps 90 is out of SDXL's 1-80 range, so it's reset to SDXL's default.
    let back = migrate(&migration.params, sdxl).unwrap();
    assert_eq!(back.params.get("steps"), Some(&Value::Unsigned(Unsigned(30))));
    assert_eq!((back.removed.clone(), back.reset.clone()), (vec!["aspect_ratio".to_owned()], vec!["steps".to_owned()]));
    assert_eq!(
        back.notice(flux, sdxl).as_deref(),
        Some("Switched to SDXL (local): removed Aspect ratio (not available there); reset Steps (out of range there).")
    );
    assert!(migrate(&ParamValues::new(), sdxl).unwrap().notice(flux, sdxl).is_none());
    // Long lists are capped in the notice.
    let many = Migration { params: ParamValues::new(), removed: (0..8).map(|i| format!("p{i}")).collect(), reset: vec![] };
    assert_eq!(many.notice(sdxl, flux).as_deref(), Some("Switched to FLUX.1 [dev] (local): removed p0, p1, p2, p3, p4 and 3 more (not available there)."));
}

#[test]
fn bad_catalog_files_are_rejected_with_a_reason() {
    let model = |edit: &str| PRICED.replacen("quality = 5", edit, 1);
    for (text, needle) in [
        (model("quality = 9"), "quality"),
        (model("quality = 5\nsurprise = true"), "unknown field"),
        (PRICED.replace("count = \"duration\"", "count = \"length\""), "length"),
        (PRICED.replace("count = \"duration\"", "count = \"seed\""), "at least 1"),
        (PRICED.replace("count = \"duration\", default_count = 5", "default_count = 0"), "at least 1"),
        (PRICED.replace("control = { type = \"unsigned\", min = \"1\", max = \"10\" }", "control = { type = \"unsigned\", max = \"10\" }"), "at least 1"),
        (
            PRICED
                .replace(
                    "control = { type = \"unsigned\", min = \"1\", max = \"10\" }\ndefault = { type = \"unsigned\", value = \"5\" }",
                    "control = { type = \"unsigned\", min = \"1\", max = \"10\" }",
                )
                .replace(", default_count = 5", ""),
            "needs a default",
        ),
        (PRICED.replace("count = \"duration\", default_count = 5", ""), "explicit `default_count`"),
        (
            PRICED
                .replace("control = { type = \"unsigned\", min = \"1\", max = \"10\" }", "control = { type = \"integer\", min = \"-1\", max = \"10\" }")
                .replace("{ type = \"unsigned\", value = \"5\" }", "{ type = \"integer\", value = \"5\" }"),
            "at least 1",
        ),
        (PRICED.replace("id = \"test.video\"", "id = \"test/../video\""), "ids"),
        (
            PRICED.replace(
                "role = \"first_frame\", modality = \"image\", min = 1, max = 1",
                "role = \"first_frame\", modality = \"image\", min = 1, max = 1, inline = true",
            ),
            "only text inputs",
        ),
        (PRICED.replace("id = \"test.video\"", "id = \"Test Video\""), "ids"),
        (PRICED.replace("0.05 USD", "0.05"), "price"),
        (PRICED.replace("max = 1 }]", "max = 0 }]"), "min <= max"),
        (PRICED.replace("label = \"Test Video\"", "label = \"\""), "label"),
        (format!("{PRICED}{PRICED}"), "defined twice"),
        ("[[model]".to_owned(), "invalid catalog file"),
    ] {
        let error = Catalog::from_toml(&text).unwrap_err();
        assert!(error.message.contains(needle), "{needle}: {}", error.message);
    }
    assert!(Catalog::from_toml(&" ".repeat(MAX_FILE_BYTES + 1)).is_err());
    assert!(Catalog::from_toml("").unwrap().is_empty());
    // Merging a builtin id again is a conflict, not an override.
    let mut catalog = Catalog::builtin().unwrap();
    assert_eq!(catalog.merge(Catalog::builtin().unwrap()).unwrap_err().kind, ErrorKind::Conflict);
}

fn migration_text_model(default_bytes: usize) -> ModelDescriptor {
    let mut model = catalog().get("comfyui.flux-dev").unwrap().clone();
    model.params.params = ["defaulted", "supplied"]
        .into_iter()
        .map(|name| nodecraft_types::ParamSpec {
            name: name.into(),
            label: name.into(),
            description: None,
            control: nodecraft_types::ParamControl::Text { multiline: true, max_bytes: MAX_MIGRATED_BYTES },
            required: false,
            default: (name == "defaulted").then(|| Value::Text("d".repeat(default_bytes))),
            visible_if: None,
        })
        .collect();
    model
}

#[test]
fn migration_admits_effective_defaults_before_copying_retained_values() {
    let model = migration_text_model(600 * 1024);
    model.params.validate().unwrap();
    for reset in [false, true] {
        let mut params = ParamValues::from([("supplied".into(), Value::Text("s".repeat(600 * 1024)))]);
        if reset {
            params.insert("defaulted".into(), Value::Bool(false));
        }
        // The catalog admission error distinguishes this from the resolver's later rejection,
        // which used to happen only after cloning the supplied value into the retained map.
        let error = migrate(&params, &model).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert_eq!(error.message, "parameters are too large to carry over");
    }
}

#[test]
fn migration_effective_default_budget_is_inclusive_and_preserves_notices() {
    let default_bytes = 600 * 1024;
    let model = migration_text_model(default_bytes);
    for reset in [false, true] {
        let mut params =
            ParamValues::from([("supplied".into(), Value::Text("s".repeat(MAX_MIGRATED_BYTES - default_bytes))), ("obsolete".into(), Value::Bool(true))]);
        if reset {
            params.insert("defaulted".into(), Value::Bool(false));
        }
        let migration = migrate(&params, &model).unwrap();
        assert_eq!(migration.params.get("defaulted"), Some(&Value::Text("d".repeat(default_bytes))));
        assert_eq!(migration.params.get("supplied"), params.get("supplied"));
        assert_eq!(migration.removed, ["obsolete"]);
        assert_eq!(migration.reset, if reset { vec!["defaulted"] } else { vec![] });
        if let Some(Value::Text(text)) = params.get_mut("supplied") {
            text.push('s');
        }
        assert_eq!(migrate(&params, &model).unwrap_err().message, "parameters are too large to carry over");
    }
}

#[test]
fn migration_counts_valid_overrides_instead_of_their_defaults() {
    let model = migration_text_model(600 * 1024);
    let params = ParamValues::from([
        ("defaulted".into(), Value::Text("override".into())),
        ("supplied".into(), Value::Text("s".repeat(MAX_MIGRATED_BYTES - "override".len()))),
    ]);
    let migration = migrate(&params, &model).unwrap();
    assert_eq!(migration.params, params);
    assert!(migration.removed.is_empty());
    assert!(migration.reset.is_empty());
}

#[test]
fn migration_includes_default_asset_mime_in_the_effective_budget() {
    let mut model = migration_text_model(0);
    let asset = Value::Media(nodecraft_types::AssetRef {
        hash: nodecraft_types::Blake3::from_bytes([0; 32]),
        mime: "image/png".parse().unwrap(),
        meta: Default::default(),
    });
    let spec = model.params.params.first_mut().unwrap();
    spec.control = nodecraft_types::ParamControl::Asset { modality: Modality::Image };
    spec.default = Some(asset.clone());
    model.params.validate().unwrap();
    for reset in [false, true] {
        let mut params = ParamValues::from([("supplied".into(), Value::Text("s".repeat(MAX_MIGRATED_BYTES - "image/png".len())))]);
        if reset {
            params.insert("defaulted".into(), Value::Bool(false));
        }
        assert_eq!(migrate(&params, &model).unwrap().params.get("defaulted"), Some(&asset));
        if let Some(Value::Text(text)) = params.get_mut("supplied") {
            text.push('s');
        }
        assert_eq!(migrate(&params, &model).unwrap_err().message, "parameters are too large to carry over");
    }
}

#[test]
fn migration_still_validates_the_destination_schema_before_defaults() {
    let mut model = migration_text_model(0);
    model.params.params.first_mut().unwrap().default = Some(Value::Bool(false));
    assert_eq!(migrate(&ParamValues::new(), &model).unwrap_err().message, "parameter value does not match its control type or bounds");
}

proptest! {
    #[test]
    fn arbitrary_text_never_panics(text in ".{0,2048}") {
        let _ = Catalog::from_toml(&text);
    }

    #[test]
    fn mangled_catalogs_never_panic(cut in 0usize..PRICED.len(), insert in ".{0,16}") {
        let mut text = PRICED.to_owned();
        let at = (0..=cut).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
        text.insert_str(at, &insert);
        if let Ok(catalog) = Catalog::from_toml(&text) {
            for model in catalog.models() {
                prop_assert!(model.validate().is_ok());
                let _ = model.pricing.quote(&ParamValues::new(), Currency::USD);
            }
        }
    }
}

#[test]
fn quotes_and_swaps_refuse_bad_programmatic_input() {
    let catalog = catalog();
    let mut video = catalog.get("test.video").unwrap().clone();
    // Changed in code to something the loader would reject: no free quote.
    video.pricing = Pricing::PerUnit { unit: Unit::Second, price: "1 USD".parse().unwrap(), count: None, default_count: Some(0) };
    assert!(video.quote(&ParamValues::new()).is_err());

    // Too many or too large parameters are refused before anything is copied.
    let flux = catalog.get("comfyui.flux-dev").unwrap();
    let many: ParamValues = (0..300).map(|i| (format!("p{i}"), Value::Bool(true))).collect();
    assert!(migrate(&many, flux).is_err());
    let mut prompt_model = flux.clone();
    prompt_model.params.params.push(nodecraft_types::ParamSpec {
        name: "note".into(),
        label: "Note".into(),
        description: None,
        control: nodecraft_types::ParamControl::Text { multiline: true, max_bytes: 1024 * 1024 },
        required: false,
        default: None,
        visible_if: None,
    });
    let mut big = ParamValues::new();
    big.insert("note".into(), Value::Text("x".repeat(1024 * 1024 - 64)));
    big.insert("steps".into(), Value::Unsigned(Unsigned(2)));
    assert!(migrate(&big, &prompt_model).is_ok());
    // Two values each within their control, together over the 1 MiB budget: refused up front.
    let second = prompt_model.params.params.last().cloned().map(|spec| nodecraft_types::ParamSpec { name: "note2".into(), ..spec });
    prompt_model.params.params.extend(second);
    let mut bigger = ParamValues::new();
    bigger.insert("note".into(), Value::Text("x".repeat(600 * 1024)));
    bigger.insert("note2".into(), Value::Text("y".repeat(600 * 1024)));
    assert_eq!(migrate(&bigger, &prompt_model).unwrap_err().message, "parameters are too large to carry over");
    // Long unknown names are cut for the notice.
    let mut long = ParamValues::new();
    long.insert("é".repeat(100), Value::Bool(true));
    let migration = migrate(&long, flux).unwrap();
    assert!(migration.removed.first().is_some_and(|name| name.len() <= 128 && name.chars().all(|c| c == 'é')));
}

#[test]
fn measured_usage_pricing_is_never_free_and_keeps_schema_admission() {
    let pricing: Pricing = toml::from_str("type = \"by_usage\"\ncurrency = \"USD\"").unwrap();
    assert_eq!(pricing.currency(), Some(Currency::USD));
    for n in [1, 10] {
        let mut params = ParamValues::new();
        params.insert("n".into(), Value::Unsigned(Unsigned(n)));
        for model in Catalog::builtin().unwrap().models().filter(|m| m.provider == "openai-images") {
            assert_eq!(model.quote(&params).unwrap(), None);
            assert_eq!(model.mode("edit").unwrap().inputs.iter().find(|r| r.role == "image").unwrap().max, 16);
            assert!(model.mode("inpaint").unwrap().inputs.iter().any(|r| r.role == "mask"));
            let mut bad = model.clone();
            bad.params.params[0].name.clear();
            assert!(bad.quote(&params).is_err(), "ByUsage must not bypass descriptor validation");
        }
    }
    for bad in ["type = \"by_usage\"", "type = \"by_usage\"\ncurrency = \"usd\"", "type = \"by_usage\"\ncurrency = \"USD\"\nreason = \"free\""] {
        assert!(toml::from_str::<Pricing>(bad).is_err());
    }
}

#[test]
fn gemini_models_have_exact_wire_options_and_unknown_cost() {
    let c = Catalog::builtin().unwrap();
    let models: Vec<_> = c.models().filter(|m| m.provider == "gemini-images").collect();
    assert_eq!(models.len(), 3);
    for m in models {
        assert_eq!(m.quote(&ParamValues::new()).unwrap(), None);
        assert_eq!(m.mode("edit").unwrap().inputs.iter().find(|i| i.role == "image").unwrap().max, 14);
        assert_eq!(m.params.params.len(), 2);
        let mut p = ParamValues::new();
        p.insert("image_size".into(), Value::Text("1024px".into()));
        assert!(m.quote(&p).is_err());
        if m.provider_model == "gemini-3.1-flash-lite-image" {
            p.insert("image_size".into(), Value::Text("2K".into()));
            assert!(m.quote(&p).is_err());
        }
    }
}

#[test]
fn gemini_omni_flash_is_charged_by_measured_tokens() {
    // Gemini pricing page, checked 2026-10-22: 1.50 USD per million input tokens, 17.50 USD per
    // million video output tokens. The clip's length is the model's choice: no quote before the run.
    let c = Catalog::builtin().unwrap();
    let m = c.get("gemini-video.omni-flash").unwrap();
    assert_eq!(m.quote(&ParamValues::new()).unwrap(), None);
    let rates = m.usage.as_ref().unwrap();
    assert_eq!((rates.input.0, rates.output.0), (Money::new(Currency::USD, 1_500), Money::new(Currency::USD, 17_500)));
    assert_eq!(m.mode("reference-to-video").unwrap().inputs.iter().find(|i| i.role == "reference").unwrap().max, 3);
    let mut p = ParamValues::new();
    p.insert("resolution".into(), Value::Text("4k".into()));
    assert!(m.quote(&p).is_err(), "4K's token rate isn't published");
}

#[test]
fn veo_on_google_cloud_is_quoted_by_length_and_resolution() {
    // Vertex AI pricing page, checked 2026-10-22: per second of video with audio.
    let c = Catalog::builtin().unwrap();
    let quote = |id: &str, seconds: u64, resolution: &str| {
        let mut p = ParamValues::new();
        p.insert("duration_seconds".into(), Value::Unsigned(nodecraft_types::Unsigned(seconds)));
        p.insert("resolution".into(), Value::Text(resolution.into()));
        c.get(id).unwrap().quote(&p)
    };
    assert_eq!(c.get("google-veo.veo-3.1").unwrap().quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 3_200_000)), "8 s at 720p");
    assert_eq!(quote("google-veo.veo-3.1", 4, "4k").unwrap(), Some(Money::new(Currency::USD, 2_400_000)));
    assert_eq!(quote("google-veo.veo-3.1-fast", 6, "1080p").unwrap(), Some(Money::new(Currency::USD, 720_000)));
    assert_eq!(quote("google-veo.veo-3.1-lite", 8, "1080p").unwrap(), Some(Money::new(Currency::USD, 640_000)));
    assert!(quote("google-veo.veo-3.1-fast", 8, "4k").is_err(), "4K is Veo 3.1's alone");
    assert!(quote("google-veo.veo-3.1", 5, "720p").is_err(), "4, 6 or 8 seconds");
    let lite = c.get("google-veo.veo-3.1-lite").unwrap();
    assert!(lite.mode("image-to-video").is_none() && lite.mode("reference-to-video").is_none(), "Lite is text only");
    assert_eq!(c.get("google-veo.veo-3.1-fast").unwrap().mode("reference-to-video").unwrap().inputs.iter().find(|i| i.role == "reference").unwrap().max, 3);
}

#[test]
fn gemini_images_on_google_cloud_are_quoted_per_image_by_size() {
    // Vertex AI pricing page, checked 2026-10-23: image output tokens per image by size, at the
    // model's image output rate per million tokens (Standard PayGo, global).
    let c = Catalog::builtin().unwrap();
    let usd = |micros| Some(Money::new(Currency::USD, micros));
    let quote = |id: &str, size: Option<&str>| {
        let mut p = ParamValues::new();
        if let Some(size) = size {
            p.insert("image_size".into(), Value::Text(size.into()));
        }
        c.get(id).unwrap().quote(&p).unwrap()
    };
    assert_eq!(quote("google-gemini-images.3.1-flash", None), usd(67_200), "1K by default: 1120 tokens at 60 USD/M");
    assert_eq!(quote("google-gemini-images.3.1-flash", Some("2K")), usd(100_800));
    assert_eq!(quote("google-gemini-images.3.1-flash", Some("4K")), usd(151_200));
    assert_eq!(quote("google-gemini-images.3-pro", Some("2K")), usd(134_400));
    assert_eq!(quote("google-gemini-images.3-pro", Some("4K")), usd(240_000));
    assert_eq!(quote("google-gemini-images.nano-banana-2.1", Some("4K")), usd(113_400));
    assert_eq!(quote("google-gemini-images.3.1-flash-lite", None), usd(33_600));
    assert_eq!(quote("google-gemini-images.2.5-flash", None), usd(38_700));
    let pro = c.get("google-gemini-images.3-pro").unwrap();
    let rates = pro.usage.as_ref().unwrap();
    assert_eq!((rates.input.0, rates.output.0), (Money::new(Currency::USD, 2_000), Money::new(Currency::USD, 120_000)));
    assert_eq!(pro.mode("edit").unwrap().inputs.iter().find(|i| i.role == "image").unwrap().max, 14);
    assert_eq!(c.get("google-gemini-images.2.5-flash").unwrap().mode("edit").unwrap().inputs.iter().find(|i| i.role == "image").unwrap().max, 3);
}

#[test]
fn seedance_is_quoted_per_second_at_its_largest_size() {
    // ModelArk pricing page, checked 2026-10-23: USD per million tokens, tokens being
    // width x height x 24 x seconds / 1024, at each resolution's largest listed size.
    let c = Catalog::builtin().unwrap();
    let quote = |id: &str, seconds: u64, resolution: &str| {
        let mut p = ParamValues::new();
        p.insert("duration_seconds".into(), Value::Unsigned(nodecraft_types::Unsigned(seconds)));
        p.insert("resolution".into(), Value::Text(resolution.into()));
        c.get(id).unwrap().quote(&p)
    };
    assert_eq!(c.get("seedance.2-0").unwrap().quote(&ParamValues::new()).unwrap(), Some(Money::new(Currency::USD, 760_765)), "5 s at 720p");
    assert_eq!(quote("seedance.2-5", 30, "1080p").unwrap(), Some(Money::new(Currency::USD, 17_167_830)));
    assert_eq!(quote("seedance.2-0", 15, "4k").unwrap(), Some(Money::new(Currency::USD, 11_664_915)));
    assert_eq!(quote("seedance.2-0-mini", 4, "480p").unwrap(), Some(Money::new(Currency::USD, 140_616)));
    assert!(quote("seedance.2-0-fast", 5, "1080p").is_err(), "Fast and Mini stop at 720p");
    assert!(quote("seedance.2-5", 5, "4k").is_err(), "4K is 2.0's alone");
    assert!(quote("seedance.2-0", 16, "720p").is_err() && quote("seedance.2-5", 31, "720p").is_err() && quote("seedance.2-5", 3, "720p").is_err());
    let references = |id: &str| c.get(id).unwrap().mode("reference-to-video").unwrap().inputs.iter().find(|i| i.role == "reference").unwrap().max;
    assert_eq!((references("seedance.2-5"), references("seedance.2-0-mini")), (30, 9));
}

#[test]
fn fal_catalog_covers_all_five_modalities_without_false_free_quotes() {
    let c = Catalog::builtin().unwrap();
    for (id, output, input) in [
        ("fal.flux-schnell", Modality::Image, Modality::Text),
        ("fal.wan22", Modality::Video, Modality::Text),
        ("fal.kokoro", Modality::Audio, Modality::Text),
        ("fal.trellis2", Modality::Mesh, Modality::Image),
        ("fal.esrgan", Modality::Image, Modality::Image),
    ] {
        let m = c.get(id).unwrap();
        assert_eq!(m.provider, "fal");
        assert!(Wiring::new(output).with(input, 1).fit(m).is_ok());
        let excess = if id == "fal.wan22" { 3 } else { 2 };
        assert!(Wiring::new(output).with(input, excess).fit(m).is_err());
        assert_eq!(m.quote(&ParamValues::new()).unwrap(), if id == "fal.trellis2" { Some(Money::new(Currency::USD, 250_000)) } else { None });
    }
}

#[test]
fn token_rates_must_match_a_paid_price_and_name_a_real_threshold() {
    let pricing = r#"pricing = { type = "per_unit", unit = "second", price = "0.05 USD", count = "duration", default_count = 5 }"#;
    let with = |line: &str| PRICED.replace(pricing, line);
    let rated = |usage: &str| with(&format!("{pricing}\nusage = {usage}"));
    let ok =
        Catalog::from_toml(&rated(r#"{ input = "0.002 USD", output = "0.01 USD", long = { above = 1, input = "0.003 USD", output = "0.015 USD" } }"#)).unwrap();
    assert_eq!(ok.get("test.video").and_then(|m| m.usage.as_ref()).map(|r| r.input.0), Some(Money::new(Currency::USD, 2_000)));
    assert!(Catalog::from_toml(&rated(r#"{ input = "0.002 EUR", output = "0.01 EUR" }"#)).is_err(), "another currency");
    assert!(Catalog::from_toml(&rated(r#"{ input = "0.002 USD", output = "0.01 USD", long = { above = 0, input = "1 USD", output = "1 USD" } }"#)).is_err());
    let free = with("pricing = { type = \"free\" }\nusage = { input = \"0.002 USD\", output = \"0.01 USD\" }");
    assert!(Catalog::from_toml(&free).is_err(), "a free model has nothing to settle");
    assert!(Catalog::from_toml(&with(&format!("{pricing}\nusage = {{ input = \"0.002 USD\" }}"))).is_err(), "both rates");
    assert!(Catalog::from_toml(&rated(r#"{ input = "0.002 USD", output = "0 USD" }"#)).is_err(), "a paid reply costs something");
}

/// PRICED with a speech mode appended, and `extra` spliced into that mode.
fn with_speech_mode(extra: &str) -> Result<Catalog, nodecraft_types::Error> {
    let speech = format!(
        r#"
[[model.modes]]
id = "speech-to-video"
output = "video"
inputs = [{{ role = "prompt", modality = "text", min = 0, max = 1 }}, {{ role = "portrait", modality = "image", min = 1, max = 1 }}, {{ role = "speech", modality = "audio", min = 1, max = 1 }}]
{extra}
"#
    );
    // The mode goes before the params so TOML keeps it in the same model.
    let at = PRICED.find("[[model.params.params]]").unwrap();
    Catalog::from_toml(&format!("{}{speech}{}", &PRICED[..at], &PRICED[at..]))
}

#[test]
fn a_mode_may_send_only_some_settings_and_describe_its_audio() {
    let catalog = with_speech_mode(
        r#"params = ["duration", "seed"]
audio = { role = "speech", duration = "fixed", max_ms = 30000, sound = "input" }"#,
    )
    .unwrap();
    let model = catalog.get("test.video").unwrap();
    let mode = model.modes.iter().find(|mode| mode.id == "speech-to-video").unwrap();
    assert_eq!(mode.params.as_deref(), Some(&["duration".to_owned(), "seed".to_owned()][..]));
    let audio = mode.audio.as_ref().unwrap();
    assert_eq!((audio.max_ms, audio.longer), (Some(30_000), nodecraft_types::AudioOverrun::Refuse), "refusing is the default");
    assert!(model.modes.iter().filter(|mode| mode.id != "speech-to-video").all(|mode| mode.params.is_none() && mode.audio.is_none()));
}

#[test]
fn bad_mode_settings_and_audio_are_rejected_with_a_reason() {
    let speech = r#"audio = { role = "speech", duration = "fixed", sound = "input" }"#;
    for (extra, reason) in [
        (r#"params = ["duration", "fps"]"#, "doesn't have"),
        (r#"params = ["duration", "duration"]"#, "twice"),
        // Pricing counts `duration`: a mode that never sends it can't be quoted.
        (r#"params = ["seed"]"#, "which pricing counts"),
        (r#"audio = { role = "portrait", duration = "fixed", sound = "silent" }"#, "audio inputs"),
        (r#"audio = { role = "speech", duration = "fixed", max_ms = 0, sound = "input" }"#, "max_ms"),
        (r#"audio = { role = "speech", duration = "fixed", max_ms = 3600001, sound = "input" }"#, "max_ms"),
        (r#"audio = { role = "speech", duration = "sometimes", sound = "input" }"#, ""),
        // Priced per second of `duration`, but the clip runs as long as the speech.
        (r#"audio = { role = "speech", duration = "follows_audio", sound = "input" }"#, "follows its audio"),
        (r#"audio = { role = "speech", duration = "fixed", sound = "input", loud = true }"#, ""),
    ] {
        let error = with_speech_mode(extra).unwrap_err();
        assert!(error.message.contains(reason), "{extra}: {}", error.message);
    }
    assert!(with_speech_mode(speech).is_ok());
}

#[test]
fn fal_wan_frames_and_speech_take_what_their_adapter_maps() {
    let c = Catalog::builtin().unwrap();
    let frames = c.get("fal.wan22-i2v").unwrap();
    let one = Wiring::new(Modality::Video).with_inline_prompt().with(Modality::Image, 1);
    assert_eq!(one.fit(frames).map(|m| m.id.as_str()), Ok("image-to-video"));
    assert!(Wiring::new(Modality::Video).with_inline_prompt().with(Modality::Image, 2).fit(frames).is_ok());
    assert!(Wiring::new(Modality::Video).with_inline_prompt().with(Modality::Image, 3).fit(frames).is_err());
    assert!(Wiring::new(Modality::Video).with_inline_prompt().fit(frames).is_err(), "a first frame is required");
    let speech = c.get("fal.wan22-s2v").unwrap();
    let both = Wiring::new(Modality::Video).with_inline_prompt().with(Modality::Image, 1).with(Modality::Audio, 1);
    let mode = both.fit(speech).unwrap();
    assert_eq!(mode.id, "speech-to-video");
    let audio = mode.audio.as_ref().unwrap();
    assert_eq!((audio.role.as_str(), audio.duration, audio.sound), ("speech", nodecraft_types::AudioDuration::Fixed, nodecraft_types::AudioSound::Input));
    assert_eq!(audio.max_ms, Some(7_500), "the longest clip: 120 frames at 16 fps");
    assert!(Wiring::new(Modality::Video).with_inline_prompt().with(Modality::Image, 1).fit(speech).is_err(), "speech is required");
    for m in [frames, speech] {
        assert_eq!(m.quote(&ParamValues::new()).unwrap(), None, "fal bills by usage");
    }
}

const TRAINER: &str = r#"
[[model]]
id = "test.trainer"
provider = "test"
provider_model = "trainer-v1"
label = "Test Trainer"
description = "Trains a style from images."
latency_s = 600
quality = 3
style_family = "flux-1"
pricing = { type = "per_unit", unit = "thousand_steps", price = "2 USD", count = "steps" }

[[model.modes]]
id = "images-to-style"
output = "style"
inputs = [{ role = "images", modality = "image", min = 4, max = 50 }]

[[model.params.params]]
name = "steps"
label = "Steps"
control = { type = "unsigned", min = "1", max = "10000" }
default = { type = "unsigned", value = "1000" }
"#;

#[test]
fn trainers_quote_their_steps_and_name_the_family_they_train() {
    let catalog = Catalog::from_toml(TRAINER).unwrap();
    let trainer = catalog.get("test.trainer").unwrap();
    assert_eq!(trainer.style_family.as_deref(), Some("flux-1"));
    let mut params = ParamValues::new();
    assert_eq!(trainer.quote(&params).unwrap(), Some(Money::new(Currency::USD, 2_000_000)));
    params.insert("steps".into(), Value::Unsigned(Unsigned(2500)));
    assert_eq!(trainer.quote(&params).unwrap(), Some(Money::new(Currency::USD, 5_000_000)));
    assert!(Wiring::new(Modality::Style).with(Modality::Image, 10).fit(trainer).is_ok());
    assert!(Wiring::new(Modality::Style).with(Modality::Image, 2).fit(trainer).is_err(), "too few images");

    let unnamed = TRAINER.replace("style_family = \"flux-1\"\n", "");
    assert!(Catalog::from_toml(&unnamed).unwrap_err().message.contains("style_family"));
    assert!(Catalog::from_toml(&TRAINER.replace("\"flux-1\"", "\"Flux 1\"")).is_err());
    // Steps are known before the run: a count from zero would quote a paid run as free.
    assert!(Catalog::from_toml(&TRAINER.replace("min = \"1\"", "min = \"0\"")).is_err());
    // Only a model that trains or takes styles names a family.
    let video = PRICED.replace("quality = 5", "quality = 5\nstyle_family = \"flux-1\"");
    assert!(Catalog::from_toml(&video).unwrap_err().message.contains("style_family"));
    let styled = PRICED.replace(
        "{ role = \"last_frame\", modality = \"image\", min = 0, max = 1 },",
        "{ role = \"last_frame\", modality = \"image\", min = 0, max = 1 },\n  { role = \"style\", modality = \"style\", min = 0, max = 1 },",
    );
    assert!(Catalog::from_toml(&styled).is_err(), "takes a style but names no family");
    assert!(Catalog::from_toml(&styled.replace("quality = 5", "quality = 5\nstyle_family = \"wan-2\"")).is_ok());
}

#[test]
fn source_edits_quote_the_used_seconds_of_their_source_at_the_chosen_resolution() {
    let catalog = Catalog::builtin().unwrap();
    let swap = catalog.get("higgsfield.genjutsu-object-swap").unwrap();
    let edit = swap.edit().unwrap();
    assert_eq!((edit.role.as_str(), edit.min_ms, edit.uses_ms, edit.min_pixels), ("source", Some(4000), Some(30_000), Some(409_600)));
    let usd = |micros| Some(Money::new(Currency::USD, micros));
    // 720p by default; a started second counts in full.
    assert_eq!(swap.quote_source(&ParamValues::new(), Some(4_200)).unwrap(), usd(5 * 681_000));
    let mut fine = ParamValues::new();
    fine.insert("resolution".into(), Value::Text("1080p".into()));
    assert_eq!(swap.quote_source(&fine, Some(10_000)).unwrap(), usd(10 * 1_632_000));
    // Only the first 30 s is used, so only it is charged.
    assert_eq!(swap.quote_source(&fine, Some(95_000)).unwrap(), usd(30 * 1_632_000));
    // Without the source's length there is no price yet, and a plain quote has none either.
    assert_eq!(swap.quote_source(&fine, None).unwrap(), None);
    assert_eq!(swap.quote(&ParamValues::new()).unwrap(), None);
    let mut bad = ParamValues::new();
    bad.insert("resolution".into(), Value::Text("4K".into()));
    assert!(swap.quote_source(&bad, Some(5_000)).is_err());
    // Other pricing ignores the source.
    let kling = catalog.get("higgsfield.kling-2.5-turbo-i2v").unwrap();
    assert_eq!(kling.quote_source(&ParamValues::new(), Some(5_000)).unwrap(), kling.quote_selected(&ParamValues::new()).unwrap());
}

#[test]
fn per_source_second_pricing_is_checked() {
    const EDIT: &str = r#"
[[model]]
id = "test.edit"
provider = "test"
provider_model = "edit-v1"
label = "Edit"
description = "Edits a source."
latency_s = 90
quality = 3
pricing = PRICING

[[model.modes]]
id = "edit"
output = "video"
inputs = [{ role = "source", modality = "video", min = 1, max = 1 }]
EDIT_LINE

[[model.params.params]]
name = "resolution"
label = "Resolution"
control = { type = "choice", options = [{ value = "480p", label = "480p" }, { value = "720p", label = "720p" }] }
default = { type = "text", value = "720p" }
"#;
    let edit = r#"edit = { operation = "restyle", role = "source", min_ms = 4000, sound = "source" }"#;
    let good = r#"{ type = "per_source_second", param = "resolution", prices = { "480p" = "0.1 USD", "720p" = "0.2 USD" } }"#;
    let parse = |pricing: &str, edit: &str| Catalog::from_toml(&EDIT.replace("PRICING", pricing).replace("EDIT_LINE", edit));
    parse(good, edit).unwrap();
    for (pricing, edit, why) in [
        (good, "", "every mode to edit the same source"),
        (r#"{ type = "per_source_second", param = "resolution", prices = { "480p" = "0.1 USD" } }"#, edit, "each `resolution` choice"),
        (r#"{ type = "per_source_second", param = "resolution", prices = { "480p" = "0.1 USD", "720p" = "0.2 EUR" } }"#, edit, "one currency"),
        (r#"{ type = "per_source_second", param = "resolution", prices = { "480p" = "0 USD", "720p" = "0.2 USD" } }"#, edit, "above zero"),
        (r#"{ type = "per_source_second", param = "seed", prices = { "480p" = "0.1 USD", "720p" = "0.2 USD" } }"#, edit, "choice setting with a default"),
        (good, r#"edit = { operation = "restyle", role = "prompt", sound = "source" }"#, "one required video input"),
        (good, r#"edit = { operation = "restyle", role = "source", min_ms = 0, sound = "source" }"#, "source lengths"),
    ] {
        let error = parse(pricing, edit).unwrap_err();
        assert!(error.message.contains(why), "{why}: {}", error.message);
    }
}

/// A text model reading PDFs, with `document` spliced into its document mode.
fn reader(document: &str) -> Result<Catalog, nodecraft_types::Error> {
    Catalog::from_toml(&format!(
        r#"
[[model]]
id = "test.reader"
provider = "test"
provider_model = "reader-v1"
label = "Test Reader"
description = "Reads PDFs."
latency_s = 10
quality = 3
pricing = {{ type = "per_unit", unit = "thousand_tokens", price = "0.02 USD", count = "max_tokens" }}
usage = {{ input = "0.004 USD", output = "0.02 USD", long = {{ above = 100000, input = "0.008 USD", output = "0.04 USD" }} }}

[[model.modes]]
id = "document-to-text"
output = "text"
inputs = [{{ role = "prompt", modality = "text", min = 1, max = 1, inline = true }}, {{ role = "document", modality = "document", min = 1, max = 5 }}]
{document}

[[model.params.params]]
name = "max_tokens"
label = "Max tokens"
control = {{ type = "unsigned", min = "1", max = "64000" }}
default = {{ type = "unsigned", value = "1000" }}
"#
    ))
}

#[test]
fn pdf_pages_add_their_input_tokens_to_the_quote() {
    let catalog = reader(r#"document = { role = "document", max_pages = 100, tokens_per_page = 3000 }"#).unwrap();
    let model = catalog.get("test.reader").unwrap();
    let document = model.mode("document-to-text").and_then(|mode| mode.document.as_ref()).unwrap();
    let params = ParamValues::new();
    // 1000 output tokens at 0.02 USD per 1000.
    let base = Money::new(Currency::USD, 20_000);
    assert_eq!(model.quote_inputs(&params, None, 0).unwrap(), Some(base), "no PDF, the output-only quote");
    // 10 pages × 3000 tokens = 30000 input tokens at 0.004 USD per 1000 = 0.12 USD.
    assert_eq!(model.quote_inputs(&params, None, document.tokens(10)).unwrap(), Some(Money::new(Currency::USD, 140_000)));
    // 100 pages = 300000 tokens: over the long-prompt threshold, so the long rate applies.
    assert_eq!(model.quote_inputs(&params, None, document.tokens(100)).unwrap(), Some(Money::new(Currency::USD, 20_000 + 2_400_000)));
    assert!(model.quote_inputs(&params, None, u64::MAX).is_err(), "a hostile token count is refused, not wrapped");
}

#[test]
fn a_mode_taking_pdfs_must_say_how_it_reads_them() {
    for (document, reason) in [
        ("", "says how it reads them"),
        (r#"document = { role = "prompt", max_pages = 100, tokens_per_page = 3000 }"#, "isn't its one document input"),
        (r#"document = { role = "document", max_pages = 0, tokens_per_page = 3000 }"#, "max_pages"),
        (r#"document = { role = "document", max_pages = 100, tokens_per_page = 0 }"#, "tokens_per_page"),
        (r#"document = { role = "document", max_pages = 100, tokens_per_page = 3000, pages = 2 }"#, ""),
    ] {
        let error = reader(document).unwrap_err();
        assert!(error.message.contains(reason), "{document}: {}", error.message);
    }
}

#[test]
fn builtin_chat_models_read_pdfs_and_quote_their_pages() {
    let catalog = Catalog::builtin().unwrap();
    let chat: Vec<_> = catalog.models().filter(|model| model.provider == "anthropic" || model.provider == "openai").collect();
    assert!(!chat.is_empty());
    for model in chat {
        let id = &model.id;
        let document = model.mode("document-to-text").and_then(|mode| mode.document.as_ref()).unwrap_or_else(|| panic!("{id} reads PDFs"));
        assert_eq!((document.role.as_str(), document.max_pages), ("document", 100), "{id}");
        let params = ParamValues::new();
        let base = model.quote_inputs(&params, None, 0).unwrap().unwrap();
        let read = model.quote_inputs(&params, None, document.tokens(10)).unwrap().unwrap();
        assert_eq!(base.checked_cmp(read).unwrap(), std::cmp::Ordering::Less, "{id}: pages cost input tokens");
    }
}

#[test]
fn a_schema_only_seed_is_pinned_only_when_it_is_sent() {
    let catalog = Catalog::from_toml(PRICED).unwrap();
    let model = catalog.get("test.video").unwrap();
    let mut params = ParamValues::new();
    params.insert("prompt".into(), Value::Text("a fox".into()));
    assert!(model.pinned_seeds(&params, false).unwrap().is_empty(), "no seed sent, the provider picks one");
    params.insert("seed".into(), Value::Unsigned(Unsigned(7)));
    assert_eq!(model.pinned_seeds(&params, false).unwrap(), vec!["seed".to_owned()]);
}

#[test]
fn a_model_cards_dont_offer_is_kept_but_never_picked() {
    let catalog = catalog();
    let workflow = catalog.get("comfyui.workflow").unwrap();
    assert!(!workflow.cards);
    assert!(catalog.get("comfyui.flux-dev").unwrap().cards, "cards offer models by default");
    assert!(catalog.models().any(|m| m.id == workflow.id), "it is still priced and admitted");
    assert!(catalog.card_models().all(|m| m.id != workflow.id));
    for output in [Modality::Image, Modality::Video, Modality::Audio, Modality::Mesh] {
        let wiring = Wiring::new(output);
        assert!(workflow.modes.iter().any(|mode| mode.output == output));
        assert!(catalog.candidates(&wiring).iter().all(|c| c.model.id != workflow.id));
        let request = select::Request { wiring, effort: select::Effort::Balanced, budget_per_job: None, providers: None, params: None };
        let selection = catalog.select(&request);
        assert!(selection.ranked.iter().all(|c| c.model.id != workflow.id) && selection.rejected.iter().all(|(m, _)| m.id != workflow.id));
    }
    let unlisted = PRICED.replace("quality = 5", "quality = 5\ncards = false");
    assert!(!Catalog::from_toml(&unlisted).unwrap().models().next().unwrap().cards);
}
