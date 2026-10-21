//! Cutting a video up and taking its sound (#108): split a video into parts (equal parts, parts
//! of a set length, or at its scene changes) and write its sound out on its own. Each runs locally
//! on the host's video service; the source is never changed. The parts are WebM (VP9 picture, Opus
//! sound), the sound Ogg (Opus or Vorbis).
use crate::action::{ActionKind, choice, count, kind, port, slider, when};
use crate::video_edit::{MAX_SECONDS, quality};
use nodecraft_types::{Error, Modality};

/// A video cut into parts, in time order.
pub const SPLIT: &str = "video.split";
/// A video's sound, as a sound of its own.
pub const EXTRACT_AUDIO: &str = "video.extract_audio";
/// Every kind here.
pub const IDS: [&str; 2] = [SPLIT, EXTRACT_AUDIO];
/// Most parts one split makes, the most videos one Stitch plays back together.
pub const MAX_PARTS: u64 = crate::video_edit::MAX_STITCHED as u64;

/// Every split and extract kind.
pub fn kinds() -> Result<Vec<ActionKind>, Error> {
    Ok(vec![
        kind(
            SPLIT,
            "Split video",
            vec![port("video", Modality::Video, false)],
            vec![port("out", Modality::Video, true)],
            vec![
                choice("by", "Split", &[("parts", "Into equal parts"), ("length", "Every few seconds"), ("scenes", "At scene changes")], "parts"),
                when(count("parts", "Parts", 2, MAX_PARTS, 2), "by", "parts"),
                when(slider("length", "Part length (s)", 0.1, MAX_SECONDS, 0.1, 10.0)?, "by", "length"),
                when(slider("threshold", "Scene change threshold (lower finds more)", 0.05, 1.0, 0.01, 0.3)?, "by", "scenes"),
                when(slider("min_length", "Shortest part (s)", 0.0, 600.0, 0.1, 1.0)?, "by", "scenes"),
                quality(),
            ],
        ),
        kind(EXTRACT_AUDIO, "Extract sound", vec![port("video", Modality::Video, false)], vec![port("out", Modality::Audio, false)], Vec::new()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::PortType;

    #[test]
    fn both_kinds_are_registered_with_their_outputs() {
        let registry = crate::Registry::builtins().unwrap();
        let out = |id: &str| registry.get(id).unwrap().descriptor().outputs.first().map(|p| p.port_type);
        assert_eq!(out(SPLIT), Some(PortType { modality: Modality::Video, many: true }));
        assert_eq!(out(EXTRACT_AUDIO), Some(PortType { modality: Modality::Audio, many: false }));
        let video = PortType { modality: Modality::Video, many: false };
        let audio = PortType { modality: Modality::Audio, many: false };
        assert!(registry.get(SPLIT).unwrap().mode(&[video]).is_some());
        assert!(registry.get(EXTRACT_AUDIO).unwrap().mode(&[audio]).is_none());
    }
}
