use super::*;
use nodecraft_types::{Modality, Unsigned, Value};
#[test]
fn pika29_fixed720p_one_first_frame_and_every_duration_have_the_published_rate() {
    let catalog = Catalog::builtin().unwrap();
    let model = catalog.get("fal.pika25-i2v").unwrap();
    assert_eq!(model.provider_model, "fal-ai/pika/v2.5/image-to-video");
    assert_eq!(model.params.params.len(), 1);
    assert!(Wiring::new(Modality::Video).with(Modality::Image, 1).with_inline_prompt().fit(model).is_ok());
    for wiring in [
        Wiring::new(Modality::Video).with_inline_prompt(),
        Wiring::new(Modality::Video).with(Modality::Image, 2).with_inline_prompt(),
        Wiring::new(Modality::Image).with(Modality::Image, 1).with_inline_prompt(),
    ] {
        assert!(wiring.fit(model).is_err());
    }
    for seconds in [5, 10] {
        let params = [("duration_seconds".into(), Value::Unsigned(Unsigned(seconds)))].into();
        assert_eq!(model.quote(&params).unwrap().unwrap().micros, seconds * 50_000);
    }
    for seconds in [0, 4, 6, 9, 11] {
        assert!(model.quote(&[("duration_seconds".into(), Value::Unsigned(Unsigned(seconds)))].into()).is_err());
    }
    assert!(model.quote(&[("seed".into(), Value::Unsigned(Unsigned(1)))].into()).is_err());
}
