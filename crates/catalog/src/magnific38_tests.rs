use super::*;
use nodecraft_types::{Currency, Modality, Unsigned, Value};

#[test]
fn magnific38_one_image_and_documented_controls_keep_the_monetary_quote_unknown() {
    let catalog = Catalog::builtin().unwrap();
    let model = catalog.get("magnific.precision-v2").unwrap();
    assert_eq!(model.provider, "magnific");
    assert_eq!(model.provider_model, "image-upscaler-precision-v2");
    assert!(matches!(model.pricing, Pricing::ByUsage { currency: Currency::USD }));
    assert_eq!(model.params.params.len(), 4);
    assert!(Wiring::new(Modality::Image).with(Modality::Image, 1).with_inline_prompt().fit(model).is_ok());
    for wiring in [
        Wiring::new(Modality::Image),
        Wiring::new(Modality::Image).with(Modality::Image, 2),
        Wiring::new(Modality::Image).with(Modality::Image, 1).with(Modality::Text, 1),
    ] {
        assert!(wiring.fit(model).is_err());
    }
    for factor in ["2", "4"] {
        for level in [0, 100] {
            let params = [
                ("scale_factor".into(), Value::Text(factor.into())),
                ("sharpen".into(), Value::Unsigned(Unsigned(level))),
                ("smart_grain".into(), Value::Unsigned(Unsigned(level))),
                ("ultra_detail".into(), Value::Unsigned(Unsigned(level))),
            ]
            .into();
            model.params.resolve(&params, false).unwrap();
            assert_eq!(model.quote(&params).unwrap(), None, "credits are not a verified currency invoice");
        }
    }
    for factor in ["02", "3", "16", "2.0"] {
        assert!(model.params.resolve(&[("scale_factor".into(), Value::Text(factor.into()))].into(), false).is_err());
    }
    for field in ["sharpen", "smart_grain", "ultra_detail"] {
        assert!(model.params.resolve(&[(field.into(), Value::Unsigned(Unsigned(101)))].into(), false).is_err());
    }
    for field in ["prompt", "seed", "n", "flavor", "filter_nsfw", "output_format"] {
        assert!(!model.params.params.iter().any(|spec| spec.name == field));
    }
}
