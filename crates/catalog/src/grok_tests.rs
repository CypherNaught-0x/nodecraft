use super::*;
use nodecraft_types::{Currency, Money, Unsigned, Value};

#[test]
fn grok31_quotes_every_preset_output_and_reference_without_dropping_fees() {
    let catalog = Catalog::builtin().unwrap();
    for id in ["fal.grok-image2", "fal.grok-image2-edit"] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "fal");
        for (preset, rate) in [("low-1k", 40_000), ("low-2k", 60_000), ("medium-1k", 60_000), ("medium-2k", 80_000)] {
            for outputs in 1..=10 {
                let params = [("image_preset".into(), Value::Text(preset.into())), ("num_images".into(), Value::Unsigned(Unsigned(outputs)))].into();
                if id.ends_with("-edit") {
                    assert_eq!(model.quote(&params).unwrap(), None, "variable references must be counted");
                    for references in 1..=5 {
                        assert_eq!(
                            model.quote_inputs_count(&params, None, 0, Some(references)).unwrap(),
                            Some(Money::new(Currency::USD, outputs * rate + references * 10_000))
                        );
                    }
                    assert!(model.quote_inputs_count(&params, None, 0, Some(6)).is_err());
                    assert!(model.quote_inputs_count(&params, None, 0, Some(0)).is_err());
                } else {
                    assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, outputs * rate)));
                }
            }
        }
    }
    for id in ["fal.grok-video15", "fal.grok-video15-i2v"] {
        let model = catalog.get(id).unwrap();
        for (resolution, rate) in [("480p", 80_000), ("720p", 140_000), ("1080p", 250_000)] {
            for duration in 1..=15 {
                let params = [("resolution".into(), Value::Text(resolution.into())), ("duration_seconds".into(), Value::Unsigned(Unsigned(duration)))].into();
                assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, duration * rate + if id.ends_with("-i2v") { 10_000 } else { 0 })));
            }
        }
    }
}

#[test]
fn grok31_untrusted_input_fees_validate_role_currency_and_nonzero_price() {
    let source = BUILTIN.iter().find(|(name, _)| *name == "grok.toml").unwrap().1;
    for changed in [
        source.replace("role=\"images\",price=\"0.01 USD\"", "role=\"prompt\",price=\"0.01 USD\""),
        source.replace("price=\"0.01 USD\"", "price=\"0.01 EUR\""),
        source.replace("price=\"0.01 USD\"", "price=\"0 USD\""),
    ] {
        assert!(Catalog::from_toml(&changed).is_err());
    }
}
