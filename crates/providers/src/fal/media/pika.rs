//! Preserve the Pika-specific local eleven-second result policy.
use super::*;
pub(in crate::fal) fn probe(b: &[u8], l: Limits) -> Result<(u32, u32), ProviderError> {
    let info = video::probe(b, l, 11)?;
    Ok((info.width, info.height))
}
