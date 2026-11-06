//! Preserve the Pika-specific local result policy: a second past the longest clip each model
//! makes (10 s first-frame clips, 25 s of keyframe transitions).
use super::*;
pub(in crate::fal) fn probe(b: &[u8], l: Limits, model: Model) -> Result<(u32, u32), ProviderError> {
    let longest = if model == Model::PikaFrames { 26 } else { 11 };
    let info = video::probe(b, l, longest)?;
    Ok((info.width, info.height))
}
