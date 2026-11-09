use super::*;
use nodecraft_types::{Modality, Value};
#[test]
fn heygen43_both_media_modes_have_unknown_cost_and_exact_documented_controls() {
    let catalog = Catalog::builtin().unwrap();
    for (id, input, remote) in [
        ("fal.heygen-avatar4", Modality::Image, "avatar4/image-to-video"),
        ("fal.heygen-lipsync-speed", Modality::Video, "v3/lipsync/speed"),
        ("fal.heygen-lipsync-precision", Modality::Video, "v3/lipsync/precision"),
    ] {
        let model = catalog.get(id).unwrap();
        assert_eq!(model.provider_model, format!("fal-ai/heygen/{remote}"));
        assert!(Wiring::new(Modality::Video).with(input, 1).with(Modality::Audio, 1).fit(model).is_ok());
        assert!(Wiring::new(Modality::Video).with(input, 1).fit(model).is_err());
        assert!(Wiring::new(Modality::Video).with(Modality::Audio, 1).fit(model).is_err());
        assert!(Wiring::new(Modality::Video).with(input, 2).with(Modality::Audio, 1).fit(model).is_err());
        assert_eq!(model.quote(&Default::default()).unwrap(), None);
        assert!(model.modes[0].audio.is_none() && model.modes[0].edit.is_none(), "no unsupported duration/sound semantics");
        if input == Modality::Image {
            assert!(model.params.params.is_empty());
        } else {
            assert_eq!(model.params.params.len(), 2);
            assert_eq!(
                model.quote(&[("disable_music_track".into(), Value::Bool(true)), ("enable_speech_enhancement".into(), Value::Bool(true))].into()).unwrap(),
                None
            );
        }
        assert!(model.quote(&[("duration_seconds".into(), Value::Bool(false))].into()).is_err());
    }
}
