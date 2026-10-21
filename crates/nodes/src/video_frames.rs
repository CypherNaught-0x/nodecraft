//! Frame-based video actions (#108): pictures taken from a video (frames, a contact sheet, a long
//! exposure), a video played backwards (or there and back), and a still turned into a slow pan
//! and zoom. Each runs locally on the host's video service; the source is never changed. The
//! video outputs are WebM (VP9 picture, no sound), the pictures PNG.
use crate::action::{ActionKind, choice, color, count, kind, port, slider, when};
use crate::video_edit::MAX_SECONDS;
use nodecraft_types::{Error, Modality, ParamSpec};

/// Pictures taken from a video at chosen times.
pub const FRAMES: &str = "video.frames";
/// Frames from across a video laid out in a grid.
pub const CONTACT_SHEET: &str = "video.contact_sheet";
/// Frames from a stretch of a video blended into one picture.
pub const LONG_EXPOSURE: &str = "video.long_exposure";
/// A video played backwards, or forwards then backwards.
pub const REVERSE: &str = "video.reverse";
/// A still picture turned into a video that slowly zooms and pans across it.
pub const KEN_BURNS: &str = "video.ken_burns";
/// Every frame-based video action kind's id.
pub const IDS: [&str; 5] = [FRAMES, CONTACT_SHEET, LONG_EXPOSURE, REVERSE, KEN_BURNS];
/// Most frames one run takes from a video for pictures.
pub const MAX_PICTURES: u64 = 64;
/// Most frames Reverse decodes from a video (20 seconds at 30 a second).
pub const MAX_REVERSED: u64 = 600;
/// Most frames × pixels Reverse decodes (600 frames at 1280×720), which bounds the
/// render's work: about 266 frames at 1920×1080.
pub const MAX_REVERSED_PIXELS: u64 = 600 * 1280 * 720;
/// The longest side of a frame taken from a video, the video service's most.
pub const MAX_FRAME_SIDE: u64 = 2048;
/// The longest Ken Burns video, in seconds.
pub const MAX_PAN_SECONDS: f64 = 30.0;
/// The longest side of a Ken Burns video.
pub const MAX_PAN_SIDE: u64 = 1920;

fn quality() -> ParamSpec {
    choice("quality", "Quality", &[("draft", "Draft (smallest file)"), ("standard", "Standard"), ("high", "High (largest file)")], "standard")
}

fn video_in() -> Vec<nodecraft_graph::Port> {
    vec![port("video", Modality::Video, false)]
}

/// Every frame-based video action kind.
pub fn kinds() -> Result<Vec<ActionKind>, Error> {
    Ok(vec![
        kind(
            FRAMES,
            "Extract frames",
            video_in(),
            vec![port("out", Modality::Image, true)],
            vec![
                choice("pick", "Frames", &[("one", "One, at a time"), ("count", "Evenly spaced"), ("every", "Every few seconds")], "count"),
                when(slider("at", "At (s)", 0.0, MAX_SECONDS, 0.01, 0.0)?, "pick", "one"),
                when(count("frames", "How many", 2, MAX_PICTURES, 8), "pick", "count"),
                when(slider("every", "Every (s)", 0.1, 600.0, 0.1, 1.0)?, "pick", "every"),
                count("size", "Longest side", 16, MAX_FRAME_SIDE, 1024),
            ],
        ),
        kind(
            CONTACT_SHEET,
            "Contact sheet",
            video_in(),
            vec![port("out", Modality::Image, false)],
            vec![
                count("columns", "Columns", 1, 16, 4),
                count("rows", "Rows", 1, 16, 4),
                count("cell", "Frame size (longest side)", 32, 1024, 320),
                count("gap", "Gap", 0, 64, 8),
                color("background", "Background", "#000000ff"),
            ],
        ),
        kind(
            LONG_EXPOSURE,
            "Long exposure",
            video_in(),
            vec![port("out", Modality::Image, false)],
            vec![
                choice("blend", "Blend", &[("average", "Average (motion blur)"), ("lighten", "Lighten (light trails)"), ("darken", "Darken")], "average"),
                slider("start", "Start (s)", 0.0, MAX_SECONDS, 0.01, 0.0)?,
                slider("end", "End (s, 0 = the end)", 0.0, MAX_SECONDS, 0.01, 0.0)?,
                count("frames", "Frames blended", 2, MAX_PICTURES, 32),
                count("size", "Longest side", 16, MAX_FRAME_SIDE, 1280),
            ],
        ),
        kind(
            REVERSE,
            "Reverse video",
            video_in(),
            vec![port("out", Modality::Video, false)],
            vec![choice("mode", "Play", &[("reverse", "Backwards"), ("boomerang", "Forwards, then backwards")], "reverse"), quality()],
        ),
        kind(
            KEN_BURNS,
            "Pan and zoom",
            vec![port("image", Modality::Image, false)],
            vec![port("out", Modality::Video, false)],
            vec![
                slider("seconds", "Length (s)", 1.0, MAX_PAN_SECONDS, 0.5, 5.0)?,
                slider("zoom_from", "Zoom at the start (×)", 1.0, 4.0, 0.05, 1.0)?,
                slider("zoom_to", "Zoom at the end (×)", 1.0, 4.0, 0.05, 1.3)?,
                choice(
                    "pan",
                    "Pan",
                    &[
                        ("none", "None (stay centred)"),
                        ("right", "Left to right"),
                        ("left", "Right to left"),
                        ("down", "Top to bottom"),
                        ("up", "Bottom to top"),
                    ],
                    "none",
                ),
                choice("easing", "Motion", &[("smooth", "Ease in and out"), ("linear", "Steady")], "smooth"),
                choice("fps", "Frames a second", &[("24", "24"), ("30", "30"), ("60", "60")], "30"),
                count("size", "Longest side", 64, MAX_PAN_SIDE, 1280),
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
    fn every_kind_is_registered_with_its_outputs() {
        let registry = crate::Registry::builtins().unwrap();
        let out = |id: &str| registry.get(id).unwrap().descriptor().outputs.first().map(|p| p.port_type);
        assert_eq!(out(FRAMES), Some(PortType { modality: Modality::Image, many: true }));
        assert_eq!(out(CONTACT_SHEET), Some(PortType { modality: Modality::Image, many: false }));
        assert_eq!(out(LONG_EXPOSURE), Some(PortType { modality: Modality::Image, many: false }));
        assert_eq!(out(REVERSE), Some(PortType { modality: Modality::Video, many: false }));
        assert_eq!(out(KEN_BURNS), Some(PortType { modality: Modality::Video, many: false }));
        let video = PortType { modality: Modality::Video, many: false };
        let image = PortType { modality: Modality::Image, many: false };
        assert!(registry.get(KEN_BURNS).unwrap().mode(&[image]).is_some());
        assert!(registry.get(KEN_BURNS).unwrap().mode(&[video]).is_none());
        assert!(registry.get(REVERSE).unwrap().mode(&[video]).is_some());
    }
}
