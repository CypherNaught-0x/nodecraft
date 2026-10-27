//! Video actions (#108): trim, speed, stitch and replace a video's sound. Each runs locally on the
//! host's video service and makes a new WebM (VP9 picture, Opus sound); the source videos are never
//! changed. The output keeps the first video's upright shape (its rotation and pixel aspect applied)
//! and its frame rate.
use crate::action::{ActionKind, choice, kind, port, slider, when};
use nodecraft_types::{Error, Modality, ParamSpec};

/// Keep one stretch of a video.
pub const TRIM: &str = "video.trim";
/// Play a video faster or slower.
pub const SPEED: &str = "video.speed";
/// Play videos one after another.
pub const STITCH: &str = "video.stitch";
/// Put a sound under a video, in place of its own or mixed with it.
pub const REPLACE_AUDIO: &str = "video.replace_audio";
/// Every video action kind's id.
pub const IDS: [&str; 4] = [TRIM, SPEED, STITCH, REPLACE_AUDIO];
/// The longest stretch, in seconds, a time setting names: the video service renders at most an hour.
pub const MAX_SECONDS: f64 = 3600.0;
/// Most videos one Stitch plays, the video service's most clips.
pub const MAX_STITCHED: usize = 32;

/// The render quality every video action offers.
pub(crate) fn quality() -> ParamSpec {
    choice("quality", "Quality", &[("draft", "Draft (smallest file)"), ("standard", "Standard"), ("high", "High (largest file)")], "standard")
}

fn video(id: &str, label: &str, mut params: Vec<ParamSpec>) -> ActionKind {
    params.push(quality());
    kind(id, label, vec![port("video", Modality::Video, false)], vec![port("out", Modality::Video, false)], params)
}

/// Every video action kind.
pub fn kinds() -> Result<Vec<ActionKind>, Error> {
    Ok(vec![
        video(
            TRIM,
            "Trim video",
            vec![slider("start", "Start (s)", 0.0, MAX_SECONDS, 0.01, 0.0)?, slider("end", "End (s, 0 = the end)", 0.0, MAX_SECONDS, 0.01, 0.0)?],
        ),
        video(
            SPEED,
            "Change speed",
            vec![
                slider("speed", "Speed (×)", 0.25, 4.0, 0.05, 2.0)?,
                choice("sound", "Sound", &[("keep", "Keep, at the new speed (same pitch)"), ("mute", "Silence")], "keep"),
            ],
        ),
        kind(
            STITCH,
            "Stitch videos",
            vec![port("videos", Modality::Video, true)],
            vec![port("out", Modality::Video, false)],
            vec![
                choice("transition", "Between videos", &[("cut", "Cut"), ("crossfade", "Crossfade")], "cut"),
                when(slider("transition_length", "Crossfade (s)", 0.1, 5.0, 0.1, 0.5)?, "transition", "crossfade"),
                quality(),
            ],
        ),
        kind(
            REPLACE_AUDIO,
            "Replace sound",
            vec![port("video", Modality::Video, false), port("audio", Modality::Audio, false)],
            vec![port("out", Modality::Video, false)],
            vec![
                choice("mode", "The video's own sound", &[("replace", "Replace it"), ("mix", "Mix the new sound in")], "replace"),
                slider("volume", "New sound's volume", 0.0, 4.0, 0.05, 1.0)?,
                slider("offset", "New sound starts at (s)", 0.0, MAX_SECONDS, 0.01, 0.0)?,
                quality(),
            ],
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::PortType;

    #[test]
    fn every_kind_is_registered_with_a_video_out_and_a_quality() {
        let registry = crate::Registry::builtins().unwrap();
        for id in IDS {
            let kind = registry.get(id).unwrap();
            let descriptor = kind.descriptor();
            assert_eq!(descriptor.outputs.first().map(|p| p.port_type.modality), Some(Modality::Video), "{id}");
            assert!(descriptor.params.params.iter().any(|p| p.name == "quality"), "{id}");
        }
    }

    #[test]
    fn replace_sound_takes_a_video_and_a_sound() {
        let registry = crate::Registry::builtins().unwrap();
        let kind = registry.get(REPLACE_AUDIO).unwrap();
        let video = PortType { modality: Modality::Video, many: false };
        let audio = PortType { modality: Modality::Audio, many: false };
        assert!(kind.mode(&[video, audio]).is_some());
        assert!(kind.mode(&[video]).is_some());
        assert!(kind.mode(&[PortType { modality: Modality::Image, many: false }]).is_none());
        // Stitch takes any number of videos on its one port.
        let stitch = registry.get(STITCH).unwrap();
        assert!(stitch.mode(&[video, video, video]).is_some());
        assert!(stitch.mode(&[audio]).is_none());
    }

    #[test]
    fn templates952_stitch_plans_collection_items_in_order_and_keeps_scalar_feeds() {
        use crate::{InputValue, JobSpec, PlanCtx};
        use nodecraft_types::{AssetRef, Blake3, NodeId, Value};
        let registry = crate::Registry::builtins().unwrap();
        let kind = registry.get(STITCH).unwrap();
        let node = kind.instantiate_instance(NodeId::from_bytes([95; 16]), Default::default(), &Default::default()).unwrap();
        let value = |n| Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: "video/webm".parse().unwrap(), meta: Default::default() });
        let input = |value, many| InputValue { port: "videos".into(), port_type: PortType { modality: Modality::Video, many }, value, origins: vec![] };
        for (inputs, expected) in [
            (vec![input(Value::List(vec![value(1), value(2)]), true)], vec![value(1), value(2)]),
            (vec![input(value(1), false), input(value(2), false)], vec![value(1), value(2)]),
            (vec![input(Value::List(vec![value(2), value(1)]), true), input(value(3), false)], vec![value(2), value(1), value(3)]),
        ] {
            let jobs = kind.plan(&PlanCtx { node: &node, inputs: &inputs, targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
            let [JobSpec::Action { inputs, .. }] = jobs.as_slice() else { panic!("one local stitch job") };
            assert_eq!(inputs, &expected);
        }
        assert!(kind.plan(&PlanCtx { node: &node, inputs: &[input(Value::List(vec![]), true)], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err());
    }
}
