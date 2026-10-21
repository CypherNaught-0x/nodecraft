//! Video looks and marks (#108): grade a video, give it an effect, key a colour out of it, put a
//! watermark over it or copy it. Each runs locally on the host's video service (but Duplicate,
//! which only copies the reference); the source is never changed and its sound is kept.
use crate::action::{ActionKind, choice, color, count, kind, port, slider, text, when};
use crate::video_edit::quality;
use nodecraft_graph::Port;
use nodecraft_types::{Error, Modality};

/// Brightness, contrast, saturation and gamma.
pub const GRADE: &str = "video.grade";
/// One effect over the whole picture: black and white, sepia, negative, blur, sharpen, vignette.
pub const FILTER: &str = "video.filter";
/// A colour (a green screen) taken out, with a colour or a picture behind.
pub const KEY: &str = "video.key";
/// Text or a picture over the video.
pub const WATERMARK: &str = "video.watermark";
/// The same video, several times.
pub const DUPLICATE: &str = "video.duplicate";
/// The kinds that render on the video service.
pub const IDS: [&str; 4] = [GRADE, FILTER, KEY, WATERMARK];
/// Most copies Duplicate makes.
pub const MAX_COPIES: u64 = 16;

fn video_in() -> Port {
    port("video", Modality::Video, false)
}

fn video_out() -> Vec<Port> {
    vec![port("out", Modality::Video, false)]
}

/// Every video look and mark kind.
pub fn kinds() -> Result<Vec<ActionKind>, Error> {
    let position = [
        ("top_left", "Top left"),
        ("top", "Top"),
        ("top_right", "Top right"),
        ("left", "Left"),
        ("center", "Centre"),
        ("right", "Right"),
        ("bottom_left", "Bottom left"),
        ("bottom", "Bottom"),
        ("bottom_right", "Bottom right"),
    ];
    Ok(vec![
        kind(
            GRADE,
            "Grade video",
            vec![video_in()],
            video_out(),
            vec![
                slider("brightness", "Brightness", -1.0, 1.0, 0.01, 0.0)?,
                slider("contrast", "Contrast", 0.0, 3.0, 0.01, 1.0)?,
                slider("saturation", "Saturation", 0.0, 3.0, 0.01, 1.0)?,
                slider("gamma", "Gamma", 0.1, 4.0, 0.01, 1.0)?,
                quality(),
            ],
        ),
        kind(
            FILTER,
            "Video effect",
            vec![video_in()],
            video_out(),
            vec![
                choice(
                    "effect",
                    "Effect",
                    &[
                        ("grayscale", "Black and white"),
                        ("sepia", "Sepia"),
                        ("invert", "Negative"),
                        ("blur", "Blur"),
                        ("sharpen", "Sharpen"),
                        ("vignette", "Vignette"),
                    ],
                    "grayscale",
                ),
                when(slider("blur", "Blur (px)", 0.1, 50.0, 0.1, 4.0)?, "effect", "blur"),
                when(slider("sharpen", "Sharpen amount", 0.1, 3.0, 0.05, 1.0)?, "effect", "sharpen"),
                when(slider("vignette", "Vignette strength", 0.05, 1.0, 0.05, 0.5)?, "effect", "vignette"),
                quality(),
            ],
        ),
        kind(
            KEY,
            "Key out a colour",
            vec![video_in(), port("background", Modality::Image, false)],
            video_out(),
            vec![
                color("key", "Colour to take out", "#00ff00ff"),
                slider("similarity", "Similarity", 0.01, 1.0, 0.01, 0.15)?,
                slider("blend", "Edge softness", 0.0, 1.0, 0.01, 0.05)?,
                color("background_color", "Behind (without a picture)", "#000000ff"),
                quality(),
            ],
        )
        .optional_last(),
        kind(
            WATERMARK,
            "Watermark video",
            vec![video_in(), port("mark", Modality::Image, false)],
            video_out(),
            vec![
                choice("source", "Mark", &[("text", "Text"), ("image", "The connected picture")], "text"),
                when(text("mark_text", "Text", "NodeCraft"), "source", "text"),
                when(slider("text_size", "Text size (% of the height)", 1.0, 30.0, 0.5, 5.0)?, "source", "text"),
                when(color("text_color", "Text colour", "#ffffffff"), "source", "text"),
                when(slider("scale", "Width (% of the video's)", 2.0, 100.0, 1.0, 20.0)?, "source", "image"),
                choice("position", "Position", &position, "bottom_right"),
                slider("margin", "Margin (% of the height)", 0.0, 25.0, 0.5, 4.0)?,
                slider("opacity", "Opacity", 0.0, 1.0, 0.01, 0.7)?,
                quality(),
            ],
        )
        .optional_last(),
        kind(DUPLICATE, "Duplicate video", vec![video_in()], vec![port("out", Modality::Video, true)], vec![count("copies", "Copies", 2, MAX_COPIES, 2)]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputValue, JobSpec, PlanCtx};
    use nodecraft_types::{AssetRef, Blake3, MediaMeta, NodeId, PortType, Value};

    fn media(n: u8, mime: &str, modality: Modality, port: &str) -> InputValue {
        let value = Value::Media(AssetRef { hash: Blake3::from_bytes([n; 32]), mime: mime.parse().unwrap(), meta: MediaMeta::default() });
        InputValue { port: port.into(), port_type: PortType { modality, many: false }, value, origins: Vec::new() }
    }

    fn inputs(id: &str, given: &[InputValue]) -> Result<Vec<Value>, Error> {
        let registry = crate::Registry::builtins().unwrap();
        let kind = registry.get(id).unwrap();
        let node = kind.instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
        let jobs = kind.plan(&PlanCtx { node: &node, inputs: given, targets: &nodecraft_types::EMPTY_TARGET_SELECTION })?;
        match jobs.into_iter().next() {
            Some(JobSpec::Action { inputs, .. }) => Ok(inputs),
            other => panic!("unexpected job {other:?}"),
        }
    }

    #[test]
    fn the_picture_behind_a_key_or_a_mark_is_optional_but_the_video_is_not() {
        let video = media(1, "video/mp4", Modality::Video, "video");
        let picture = media(2, "image/png", Modality::Image, "background");
        assert_eq!(inputs(KEY, std::slice::from_ref(&video)).unwrap().len(), 1);
        assert_eq!(inputs(KEY, &[video.clone(), picture.clone()]).unwrap(), vec![video.value.clone(), picture.value]);
        assert_eq!(inputs(WATERMARK, std::slice::from_ref(&video)).unwrap().len(), 1);
        assert!(inputs(KEY, &[media(2, "image/png", Modality::Image, "background")]).is_err());
        assert!(inputs(GRADE, &[]).is_err());
    }

    #[test]
    fn every_kind_is_registered_and_takes_a_video() {
        let registry = crate::Registry::builtins().unwrap();
        let video = PortType { modality: Modality::Video, many: false };
        let image = PortType { modality: Modality::Image, many: false };
        for id in IDS.into_iter().chain([DUPLICATE]) {
            let kind = registry.get(id).unwrap();
            assert!(kind.mode(&[video]).is_some(), "{id}");
        }
        assert!(registry.get(KEY).unwrap().mode(&[video, image]).is_some());
        assert!(registry.get(WATERMARK).unwrap().mode(&[video, image]).is_some());
        assert!(registry.get(GRADE).unwrap().mode(&[video, image]).is_none());
        assert!(registry.get(KEY).unwrap().mode(&[image]).is_none(), "a picture alone isn't a video to key");
        let out = registry.get(DUPLICATE).unwrap().descriptor().outputs.first().map(|p| p.port_type);
        assert_eq!(out, Some(PortType { modality: Modality::Video, many: true }));
    }
}
