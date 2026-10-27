use super::*;
use nodecraft_types::{Unsigned, Value};
#[test]
fn minimax27_all_three_families_have_exact_scoped_settings_and_conservative_quotes() {
    let c = Catalog::builtin().unwrap();
    for id in ["fal.minimax-h3", "fal.minimax-h3-i2v"] {
        let m = c.get(id).unwrap();
        for (res, rate) in [("480P", 50_000), ("768P", 60_000)] {
            for duration in [5, 10, 15] {
                let p = [("resolution".into(), Value::Text(res.into())), ("duration".into(), Value::Unsigned(Unsigned(duration)))].into();
                assert_eq!(m.quote(&p).unwrap().unwrap().micros, duration * rate);
            }
        }
        assert!(m.params.params.iter().all(|p| !matches!(p.name.as_str(), "sync_mode" | "target_audio_url" | "num_outputs")));
    }
    let m = c.get("fal.minimax-music3").unwrap();
    for seconds in [1, 60, 120, 300] {
        assert_eq!(m.quote(&[("duration".into(), Value::Unsigned(Unsigned(seconds)))].into()).unwrap().unwrap().micros, seconds * 2000);
    }
    let speech = c.get("fal.minimax-speech28-hd").unwrap();
    assert!(speech.quote(&Default::default()).unwrap().is_none());
    let Pricing::PerUnit { unit: Unit::ThousandCharacters, price, count: None, default_count: None } = &speech.pricing else {
        panic!("speech character price")
    };
    assert_eq!(price.0.micros, 100_000);
}
