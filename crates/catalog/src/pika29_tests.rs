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

/// Pika 2.5 keyframes (#1061): a prompt and 2 to 5 keyframes, fixed 720p, every length from 5 to
/// 25 seconds at the published $0.05 a second.
#[test]
fn pika1061_keyframes_take_two_to_five_frames_at_the_published_rate() {
    let catalog = Catalog::builtin().unwrap();
    let model = catalog.get("fal.pika25-keyframes").unwrap();
    assert_eq!(model.provider_model, "fal-ai/pika/v2.5/pikaframes");
    assert_eq!(model.params.params.len(), 1);
    for frames in 2..=5 {
        assert!(Wiring::new(Modality::Video).with(Modality::Image, frames).with_inline_prompt().fit(model).is_ok(), "{frames} frames");
    }
    for wiring in [
        Wiring::new(Modality::Video).with(Modality::Image, 1).with_inline_prompt(),
        Wiring::new(Modality::Video).with(Modality::Image, 6).with_inline_prompt(),
        Wiring::new(Modality::Video).with_inline_prompt(),
    ] {
        assert!(wiring.fit(model).is_err());
    }
    for seconds in [5, 6, 13, 25] {
        let params = [("duration_seconds".into(), Value::Unsigned(Unsigned(seconds)))].into();
        assert_eq!(model.quote(&params).unwrap().unwrap().micros, seconds * 50_000);
    }
    for seconds in [0, 4, 26] {
        assert!(model.quote(&[("duration_seconds".into(), Value::Unsigned(Unsigned(seconds)))].into()).is_err());
    }
}
