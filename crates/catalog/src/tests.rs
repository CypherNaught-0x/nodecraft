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
fn the_builtin_catalog_is_valid_and_free() {
    let catalog = Catalog::builtin().unwrap();
    assert!(catalog.len() >= 2);
    for model in catalog.models() {
        assert_eq!(model.pricing, Pricing::Free, "{}", model.id);
        assert_eq!(model.quote(&ParamValues::new()).unwrap(), Some(Money::zero(Currency::USD)));
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
    assert_eq!(order, vec![("comfyui.flux-dev", true), ("comfyui.sdxl", true), ("test.video", false)]);
    // Wiring an image narrows it to image-to-image models.
    let edit = Wiring::new(Modality::Image).with(Modality::Text, 1).with(Modality::Image, 1);
    let groups = catalog.by_provider(&edit);
    let comfy: Vec<(&str, bool)> = groups.get("comfyui").unwrap().iter().map(|c| (c.model.id.as_str(), c.fit.is_ok())).collect();
    assert_eq!(comfy, vec![("comfyui.flux-dev", true), ("comfyui.sdxl", false)]);
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
