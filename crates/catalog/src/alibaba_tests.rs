use super::*;
use nodecraft_types::{Currency, Modality, Money, Unsigned, Value};

#[test]
fn alibaba32_quotes_every_output_tier_and_explicit_video_duration() {
    let catalog = Catalog::builtin().unwrap();
    for id in ["fal.qwen-image3", "fal.qwen-image3-edit"] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider, "fal");
        for (preset, rate) in [("1k-square", 40_000), ("2k-square", 75_000)] {
            for outputs in 1..=6 {
                let params = [("image_preset".into(), Value::Text(preset.into())), ("num_images".into(), Value::Unsigned(Unsigned(outputs)))].into();
                assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, rate * outputs)));
            }
        }
        for count in [0, 7] {
            assert!(model.quote(&[("num_images".into(), Value::Unsigned(Unsigned(count)))].into()).is_err());
        }
    }
    for id in ["fal.wan3", "fal.wan3-i2v"] {
        let model = catalog.get(id).unwrap();
        for (resolution, rate) in [("480p", 50_000), ("720p", 100_000), ("1080p", 200_000)] {
            for duration in 2..=30 {
                let params = [("resolution".into(), Value::Text(resolution.into())), ("duration_seconds".into(), Value::Unsigned(Unsigned(duration)))].into();
                assert_eq!(model.quote(&params).unwrap(), Some(Money::new(Currency::USD, rate * duration)));
            }
        }
        for duration in [0, 1, 31] {
            assert!(model.quote(&[("duration_seconds".into(), Value::Unsigned(Unsigned(duration)))].into()).is_err());
        }
    }
}

#[test]
fn alibaba32_wiring_preserves_ordered_references_and_optional_frame_prompt() {
    let catalog = Catalog::builtin().unwrap();
    let edit = catalog.get("fal.qwen-image3-edit").unwrap();
    for count in 1..=3 {
        assert!(Wiring::new(Modality::Image).with_inline_prompt().with(Modality::Image, count).fit(edit).is_ok());
    }
    assert!(Wiring::new(Modality::Image).with_inline_prompt().with(Modality::Image, 4).fit(edit).is_err());
    let frames = catalog.get("fal.wan3-i2v").unwrap();
    assert!(Wiring::new(Modality::Video).with(Modality::Image, 1).fit(frames).is_ok());
    assert!(Wiring::new(Modality::Video).with(Modality::Image, 2).fit(frames).is_ok());
    assert!(Wiring::new(Modality::Video).with(Modality::Image, 3).fit(frames).is_err());
    assert!(Wiring::new(Modality::Video).with_inline_prompt().fit(frames).is_err());
    assert_eq!(frames.modes[0].inputs.iter().find(|input| input.role == "prompt").unwrap().min, 0);
    for id in ["fal.qwen-image3", "fal.qwen-image3-edit", "fal.wan3", "fal.wan3-i2v"] {
        let model = catalog.get(id).unwrap();
        assert!(model.random_seeds(&Default::default(), false).unwrap().is_empty(), "a fixed optional seed must not advertise random-seed contracts");
        assert!(model.params.resolve(&[("seed".into(), Value::Unsigned(Unsigned(2_147_483_648)))].into(), false).is_err());
    }
}
