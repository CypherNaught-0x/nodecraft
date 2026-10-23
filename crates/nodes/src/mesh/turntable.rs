//! A bounded, repeatable orbit sampled once per encoded frame.
use super::{ActionKind, Modality, TURNTABLE, choice, count, look, port, slider, when};
use nodecraft_types::{Error, ErrorKind};

/// At most 256 million rendered pixels in one capture, independent of encoder compression.
pub const MAX_PIXELS: u64 = 268_435_456;
/// One minute, sixty frames per second, and at most 2,048 pixels per side.
pub const MAX_SECONDS: f64 = 60.0;
pub const MAX_FPS: u64 = 60;
pub const MAX_SIDE: u64 = 2048;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sequence {
    size: [u32; 2],
    fps: u32,
    frames: u32,
    start: f32,
    arc: f32,
    bounce: bool,
}
impl Sequence {
    /// Full turns omit the repeated final angle. A bounce samples a triangular wave over the
    /// same half-open period; its far endpoint is present when the frame count is even.
    pub fn new(size: [u64; 2], fps: u64, seconds: f64, start: f64, end: f64, clockwise: bool, bounce: bool) -> Result<Self, Error> {
        let invalid = || {
            Error::new(ErrorKind::InvalidInput, "turntable needs even sides from 16 to 2048, 1–60 fps, 0.1–60 seconds and finite angles within ±180 degrees")
        };
        if size.iter().any(|v| !(16..=MAX_SIDE).contains(v) || !v.is_multiple_of(2))
            || !(1..=MAX_FPS).contains(&fps)
            || !(0.1..=MAX_SECONDS).contains(&seconds)
            || !(-180.0..=180.0).contains(&start)
            || !(-180.0..=180.0).contains(&end)
        {
            return Err(invalid());
        }
        let [width, height] = size;
        let frames = (seconds * fps as f64).round();
        if !(2.0..=3600.0).contains(&frames) {
            return Err(Error::new(ErrorKind::InvalidInput, "a turntable needs at least two frames"));
        }
        let frames = frames as u32;
        let pixels = width.checked_mul(height).and_then(|n| n.checked_mul(u64::from(frames)));
        if pixels.is_none_or(|n| n > MAX_PIXELS) {
            return Err(Error::new(ErrorKind::InvalidInput, "turntable exceeds 256 million rendered pixels; lower its size, duration or frame rate"));
        }
        let sign = if clockwise { -1.0 } else { 1.0 };
        let arc = if bounce { ((end - start) * sign).rem_euclid(360.0) * sign } else { 360.0 * sign };
        if arc == 0.0 {
            return Err(Error::new(ErrorKind::InvalidInput, "the orbit's two angles must differ"));
        }
        Ok(Self { size: [width as u32, height as u32], fps: fps as u32, frames, start: start.to_radians() as f32, arc: arc.to_radians() as f32, bounce })
    }
    pub fn size(self) -> [u32; 2] {
        self.size
    }
    pub fn fps(self) -> u32 {
        self.fps
    }
    pub fn frames(self) -> u32 {
        self.frames
    }
    pub fn yaw(self, frame: u32) -> Option<f32> {
        if frame >= self.frames {
            return None;
        }
        let t = frame as f32 / self.frames as f32;
        let t = if self.bounce { 1.0 - (2.0 * t - 1.0).abs() } else { t };
        Some(self.start + self.arc * t)
    }
    pub fn duration_us(self) -> u64 {
        u64::from(self.frames) * 1_000_000 / u64::from(self.fps.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn turntable_sequence_is_repeatable_and_does_not_duplicate_its_loop_endpoint() {
        let s = Sequence::new([64, 48], 4, 1.0, 0.0, 180.0, false, false).unwrap();
        let a: Vec<_> = (0..s.frames).map(|i| s.yaw(i).unwrap()).collect();
        assert_eq!(a, (0..s.frames).map(|i| s.yaw(i).unwrap()).collect::<Vec<_>>());
        for (got, degrees) in a.iter().zip([0.0f32, 90.0, 180.0, 270.0]) {
            assert!((got.to_degrees() - degrees).abs() < 0.001);
        }
        assert_eq!(s.yaw(4), None);
        assert_eq!(s.duration_us(), 1_000_000);
    }
    #[test]
    fn turntable_bounce_reaches_the_second_angle_then_returns_in_the_chosen_direction() {
        let s = Sequence::new([16, 16], 4, 1.0, 0.0, 180.0, false, true).unwrap();
        for (i, angle) in [0.0f32, 90.0, 180.0, 90.0].into_iter().enumerate() {
            assert!((s.yaw(i as u32).unwrap().to_degrees() - angle).abs() < 0.001);
        }
        let cw = Sequence::new([16, 16], 4, 1.0, 0.0, 90.0, true, true).unwrap();
        assert!((cw.yaw(2).unwrap().to_degrees() + 270.0).abs() < 0.001);
    }
    #[test]
    fn turntable_bounds_are_checked_before_any_rendering() {
        for (size, fps, seconds, start, end) in [
            ([17, 16], 4, 1.0, 0.0, 90.0),
            ([u64::MAX, 16], 4, 1.0, 0.0, 90.0),
            ([16, 16], 61, 1.0, 0.0, 90.0),
            ([16, 16], 1, 0.1, 0.0, 90.0),
            ([16, 16], 4, f64::NAN, 0.0, 90.0),
            ([16, 16], 4, 1.0, f64::INFINITY, 90.0),
            ([2048, 2048], 60, 60.0, 0.0, 90.0),
        ] {
            assert!(Sequence::new(size, fps, seconds, start, end, false, false).is_err());
        }
        assert!(Sequence::new([16, 16], 4, 1.0, -180.0, 180.0, false, true).is_err());
        assert!(Sequence::new([512, 512], 30, 6.0, 0.0, 90.0, false, false).is_ok());
    }
}

/// A saved camera orbit rendered as a soundless WebM. Alpha is composited over black.
pub fn kind() -> Result<ActionKind, Error> {
    let mut params = vec![
        slider("start", "First angle", -180.0, 180.0, 1.0, 0.0)?,
        choice("direction", "Direction", &[("ccw", "Counter-clockwise"), ("cw", "Clockwise")], "ccw"),
        choice("orbit", "Orbit", &[("full", "Full turn"), ("bounce", "Back-and-forth")], "full"),
        when(slider("end", "Second angle", -180.0, 180.0, 1.0, 90.0)?, "orbit", "bounce"),
    ];
    params.extend(look()?);
    let mut width = count("width", "Width", 16, MAX_SIDE, 512);
    width.control = nodecraft_types::ParamControl::Unsigned {
        min: Some(nodecraft_types::Unsigned(16)),
        max: Some(nodecraft_types::Unsigned(MAX_SIDE)),
        step: nodecraft_types::Unsigned(2),
    };
    let mut height = width.clone();
    height.name = "height".into();
    height.label = "Height".into();
    params.push(width);
    params.push(height);
    params.push(slider("duration", "Duration (seconds)", 0.1, MAX_SECONDS, 0.1, 6.0)?);
    params.push(count("fps", "Frames per second", 1, MAX_FPS, 30));
    Ok(crate::action::kind(TURNTABLE, "3D turntable", vec![port("model", Modality::Mesh, false)], vec![port("out", Modality::Video, false)], params))
}
#[cfg(test)]
mod kind_tests {
    use super::*;
    use crate::{InputValue, JobSpec, NodeKind, PlanCtx};
    use nodecraft_types::{AssetRef, Blake3, NodeId, PortType, Value};

    #[test]
    fn turntable_saved_node_roundtrips_and_plans_one_mesh_to_video_action() {
        let kind = kind().unwrap();
        let node = kind.instantiate_instance(NodeId::from_bytes([9; 16]), Default::default(), &Default::default()).unwrap();
        let encoded = serde_json::to_vec(&node).unwrap();
        let node: nodecraft_graph::Node = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(serde_json::to_vec(&node).unwrap(), encoded);
        assert_eq!(node.outputs.first().unwrap().port_type.modality, Modality::Video);
        let model = Value::Media(AssetRef { hash: Blake3::from_bytes([7; 32]), mime: "model/obj".parse().unwrap(), meta: Default::default() });
        let input = InputValue { port: "model".into(), port_type: PortType { modality: Modality::Mesh, many: false }, value: model.clone(), origins: vec![] };
        let jobs = kind.plan(&PlanCtx { node: &node, inputs: &[input], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).unwrap();
        let [JobSpec::Action { action, params, inputs, output, .. }] = jobs.as_slice() else { panic!("{jobs:?}") };
        assert_eq!((action.as_str(), output.as_str()), (TURNTABLE, "out"));
        assert_eq!(inputs, &vec![model]);
        assert!(params.contains_key("start") && !params.contains_key("end"));
        assert!(kind.plan(&PlanCtx { node: &node, inputs: &[], targets: &nodecraft_types::EMPTY_TARGET_SELECTION }).is_err());
    }
}
