use super::*;
use nodecraft_types::{Currency, Modality, Money, Unsigned, Value};

#[test]
fn rodin41_catalog_quotes_the_fixed_standard_and_fast_calls_with_optional_image_prompts() {
    let catalog = Catalog::builtin().unwrap();
    for (id, price) in [("fal.rodin25", 400_000), ("fal.rodin25-images", 400_000), ("fal.rodin25-fast", 100_000), ("fal.rodin25-fast-images", 100_000)] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "fal");
        assert_eq!(model.quote(&Default::default()).unwrap(), Some(Money::new(Currency::USD, price)));
        let text = Wiring::new(Modality::Mesh).with_inline_prompt();
        assert_eq!(text.fit(model).is_ok(), !id.ends_with("-images"));
        for count in 1..=5 {
            let photos = Wiring::new(Modality::Mesh).with(Modality::Image, count);
            assert_eq!(photos.fit(model).is_ok(), id.ends_with("-images"));
        }
        assert!(Wiring::new(Modality::Mesh).with(Modality::Image, 6).fit(model).is_err());
        assert!(model.quote(&[("seed".into(), Value::Unsigned(Unsigned(65536)))].into()).is_err());
        assert!(model.random_seeds(&Default::default(), false).unwrap().is_empty());
        assert_eq!(model.quote(&[("seed".into(), Value::Unsigned(Unsigned(65535)))].into()).unwrap(), Some(Money::new(Currency::USD, price)));
    }
}
