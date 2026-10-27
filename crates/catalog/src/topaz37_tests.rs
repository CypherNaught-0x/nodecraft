use super::*;
use nodecraft_types::{Modality, Unsigned, Value};

#[test]
fn topaz37_one_image_only_and_every_admitted_factor_has_one_price_block() {
    let catalog = Catalog::builtin().unwrap();
    let model = catalog.get("fal.topaz-precision").unwrap();
    assert_eq!(model.provider_model, "topaz/upscale/image/precision");
    assert_eq!(model.params.params.len(), 1, "no prompt, seed, output count or unpriced enhancement controls");
    assert!(Wiring::new(Modality::Image).with(Modality::Image, 1).with_inline_prompt().fit(model).is_ok());
    for wiring in [
        Wiring::new(Modality::Image),
        Wiring::new(Modality::Image).with(Modality::Image, 2),
        Wiring::new(Modality::Image).with(Modality::Image, 1).with(Modality::Text, 1),
    ] {
        assert!(wiring.fit(model).is_err());
    }
    for factor in 1..=4 {
        let params = [("upscale_factor".into(), Value::Unsigned(Unsigned(factor)))].into();
        model.params.resolve(&params, false).unwrap();
        assert_eq!(model.quote(&params).unwrap().unwrap().micros, 80_000);
    }
    for factor in [0, 5] {
        assert!(model.params.resolve(&[("upscale_factor".into(), Value::Unsigned(Unsigned(factor)))].into(), false).is_err());
    }
}
