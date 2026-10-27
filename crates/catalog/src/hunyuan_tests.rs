use super::*;
use nodecraft_types::{Currency, Modality, Money, Value};

#[test]
fn hunyuan42_shipped_presets_quote_one_call_with_every_material_surcharge() {
    let catalog = Catalog::builtin().unwrap();
    for id in ["fal.hunyuan3d-v3", "fal.hunyuan3d-v3-image", "fal.hunyuan3d-v3-sketch"] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "fal");
        assert_eq!(model.quote(&Default::default()).unwrap(), Some(Money::new(Currency::USD, 375_000)));
        assert_eq!(model.modes[0].output, Modality::Mesh);
        for (preset, micros) in [
            ("textured", 375_000),
            ("pbr", 525_000),
            ("low-poly-triangles", 450_000),
            ("low-poly-quads", 450_000),
            ("low-poly-triangles-pbr", 600_000),
            ("low-poly-quads-pbr", 600_000),
            ("geometry", 225_000),
        ] {
            let params = [("mesh_preset".into(), Value::Text(preset.into()))].into();
            let quote = model.quote(&params);
            if id.ends_with("-sketch") && !matches!(preset, "textured" | "pbr") {
                assert!(quote.is_err());
            } else {
                assert_eq!(quote.unwrap(), Some(Money::new(Currency::USD, micros)));
            }
        }
    }
}

#[test]
fn hunyuan42_modes_match_their_named_prompt_photo_and_sketch_contracts() {
    let catalog = Catalog::builtin().unwrap();
    let text = catalog.get("fal.hunyuan3d-v3").unwrap();
    let photo = catalog.get("fal.hunyuan3d-v3-image").unwrap();
    let sketch = catalog.get("fal.hunyuan3d-v3-sketch").unwrap();
    let prompt = Wiring::new(Modality::Mesh).with_inline_prompt();
    assert!(prompt.fit(text).is_ok());
    assert!(prompt.fit(photo).is_err());
    assert!(prompt.fit(sketch).is_err());
    let image = Wiring::new(Modality::Mesh).with(Modality::Image, 1).with_inline_prompt();
    assert!(image.fit(text).is_err());
    assert!(image.fit(photo).is_ok());
    assert!(image.fit(sketch).is_ok());
    let extra = Wiring::new(Modality::Mesh).with(Modality::Image, 2).with_inline_prompt();
    assert!(extra.fit(photo).is_err());
    assert!(extra.fit(sketch).is_err());
    assert_eq!(photo.modes[0].inputs[0].role, "image");
    assert_eq!(sketch.modes[0].inputs[0].role, "image");
    assert_eq!(sketch.modes[0].inputs[1].role, "prompt");
}
