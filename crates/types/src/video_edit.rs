//! How a generation mode edits a wired source video (#179): which operation it is, the source
//! lengths and frame sizes it takes, how much of a longer source it uses, and what happens to the
//! source's sound. Catalogs declare it per mode; planning, quoting and the model picker read it.
use crate::{Error, ErrorKind, seconds};
use serde::{Deserialize, Serialize};

/// Longest source a catalog may declare a limit for: one hour.
pub const MAX_EDIT_MS: u64 = 3_600_000;

/// A provider-neutral source video edit. Models from different providers that do the same edit
/// declare the same operation, whatever their own endpoint is called.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditOperation {
    /// The source's motion and framing, performed by the subjects of the reference images.
    MotionTransfer,
    /// The source with an object replaced by the one in the reference images.
    ObjectSwap,
    /// The source redrawn in a preset style.
    Restyle,
}

impl EditOperation {
    /// The stable id agents and project files use.
    pub fn id(self) -> &'static str {
        match self {
            Self::MotionTransfer => "video.motion_transfer",
            Self::ObjectSwap => "video.object_swap",
            Self::Restyle => "video.restyle",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::MotionTransfer => "Motion transfer",
            Self::ObjectSwap => "Object swap",
            Self::Restyle => "Restyle",
        }
    }
}

/// What the result's soundtrack is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditSound {
    /// The source's sound, over the part of the source that was used.
    Source,
    /// No soundtrack.
    Silent,
    /// The provider doesn't say; check the result.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoEdit {
    pub operation: EditOperation,
    /// The mode's source video input role.
    pub role: String,
    /// The shortest source the model takes, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_ms: Option<u64>,
    /// How much of a longer source the model edits, from its start, in milliseconds; the
    /// provider drops the rest, so the result is no longer than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uses_ms: Option<u64>,
    /// The fewest pixels (width × height) a source frame may have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_pixels: Option<u64>,
    pub sound: EditSound,
}

impl VideoEdit {
    /// Checks the limits make sense; `what` names the mode in the message.
    pub fn validate(&self) -> Result<(), String> {
        let ms = |v: Option<u64>| v.is_none_or(|ms| (1..=MAX_EDIT_MS).contains(&ms));
        if !ms(self.min_ms) || !ms(self.uses_ms) {
            return Err(format!("source lengths are 1 to {MAX_EDIT_MS} ms"));
        }
        if let (Some(min), Some(uses)) = (self.min_ms, self.uses_ms)
            && min > uses
        {
            return Err("`min_ms` can't be above `uses_ms`".into());
        }
        if self.min_pixels.is_some_and(|px| px == 0 || px > 1 << 30) {
            return Err("`min_pixels` is 1 to 2^30".into());
        }
        Ok(())
    }

    /// One line a person reads in the model picker.
    pub fn summary(&self) -> String {
        let length = match (self.min_ms, self.uses_ms) {
            (Some(min), Some(uses)) => format!("; sources of at least {}, only the first {} is used", seconds(min), seconds(uses)),
            (Some(min), None) => format!("; sources of at least {}", seconds(min)),
            (None, Some(uses)) => format!("; only the first {} of the source is used", seconds(uses)),
            (None, None) => String::new(),
        };
        let sound = match self.sound {
            EditSound::Source => "keeps the source's sound",
            EditSound::Silent => "silent",
            EditSound::Unknown => "sound not documented",
        };
        format!("{} of {}{length}; {sound}", self.operation.label(), self.role)
    }

    /// Refuses a source the model won't take, before anything is sent or charged. A length or
    /// size that isn't known goes through; the provider still checks it.
    pub fn fits(&self, duration_ms: Option<u64>, size: Option<(u32, u32)>) -> Result<(), Error> {
        if let (Some(min), Some(ms)) = (self.min_ms, duration_ms)
            && ms < min
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                // Rounded down, so a clip just under the limit never reads as the limit.
                format!(
                    "the {} video is {} long and {} needs at least {}; use a longer clip",
                    self.role,
                    seconds(ms / 100 * 100),
                    self.operation.label().to_lowercase(),
                    seconds(min)
                ),
            ));
        }
        if let (Some(min), Some((w, h))) = (self.min_pixels, size)
            && u64::from(w).saturating_mul(u64::from(h)) < min
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "the {} video is {w}×{h} and {} needs frames of at least {min} pixels; use a larger clip",
                    self.role,
                    self.operation.label().to_lowercase()
                ),
            ));
        }
        Ok(())
    }

    /// The part of a source of `duration_ms` the model edits, in milliseconds.
    pub fn used_ms(&self, duration_ms: u64) -> u64 {
        self.uses_ms.map_or(duration_ms, |uses| duration_ms.min(uses))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swap() -> VideoEdit {
        VideoEdit {
            operation: EditOperation::ObjectSwap,
            role: "source".into(),
            min_ms: Some(4000),
            uses_ms: Some(30_000),
            min_pixels: Some(409_600),
            sound: EditSound::Unknown,
        }
    }

    #[test]
    fn short_or_small_sources_are_refused_and_unknown_ones_pass() {
        let edit = swap();
        assert!(edit.fits(Some(3_990), None).unwrap_err().message.contains("is 3.9 s long and object swap needs at least 4 s"));
        assert!(edit.fits(None, Some((640, 480))).unwrap_err().message.contains("640×480"));
        edit.fits(Some(4000), Some((854, 480))).unwrap();
        edit.fits(None, None).unwrap();
        assert_eq!((edit.used_ms(42_000), edit.used_ms(7_200)), (30_000, 7_200));
        assert_eq!(edit.summary(), "Object swap of source; sources of at least 4 s, only the first 30 s is used; sound not documented");
    }

    #[test]
    fn limits_are_checked() {
        assert!(VideoEdit { min_ms: Some(40_000), ..swap() }.validate().is_err());
        assert!(VideoEdit { min_pixels: Some(0), ..swap() }.validate().is_err());
        assert!(VideoEdit { uses_ms: Some(0), ..swap() }.validate().is_err());
        swap().validate().unwrap();
        assert_eq!(EditOperation::Restyle.id(), "video.restyle");
    }
}
