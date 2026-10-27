use super::*;
use nodecraft_types::{Currency, Money, Value};

#[test]
fn recraft35_both_models_quote_exact_single_output_prices_for_every_preset_and_color_preference() {
    let catalog = Catalog::builtin().unwrap();
    for (id, alias, micros) in
        [("fal.recraft41", "fal-ai/recraft/v4.1/text-to-image", 35_000), ("fal.recraft41-vector", "fal-ai/recraft/v4.1/text-to-vector", 80_000)]
    {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "fal");
        assert_eq!(model.provider_model, alias);
        assert!(matches!(model.pricing, Pricing::PerCall { .. }));
        assert_eq!(model.quote(&Default::default()).unwrap(), Some(Money::new(Currency::USD, micros)));
        assert!(Wiring::new(Modality::Image).with_inline_prompt().fit(model).is_ok());
        assert!(Wiring::new(Modality::Image).with_inline_prompt().with(Modality::Image, 1).fit(model).is_err());
        assert_eq!(model.modes.len(), 1);
        for preset in ["square_hd", "square", "portrait_4_3", "portrait_16_9", "landscape_4_3", "landscape_16_9"] {
            let params = [
                ("image_size".into(), Value::Text(preset.into())),
                ("colors".into(), Value::Text("#123456,#abcdef".into())),
                ("background_color".into(), Value::Text("#ffffff".into())),
            ]
            .into();
            assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, micros)));
        }
        for absent in ["seed", "num_images", "n", "enable_safety_checker", "style"] {
            assert!(!model.params.params.iter().any(|param| param.name == absent));
        }
    }
}
