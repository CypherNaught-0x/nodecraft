//! How a generation mode treats a wired audio track (#559): whether the result's length follows
//! it, the longest track the model takes, and which soundtrack the result carries. Catalogs
//! declare it per mode; planning and the model picker read it.
use serde::{Deserialize, Serialize};

/// Longest audio limit a catalog may declare: one hour.
pub const MAX_AUDIO_MS: u64 = 3_600_000;

/// Whether a result's length comes from its audio or from its own settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioDuration {
    /// The result runs as long as the audio; the mode takes no duration setting.
    FollowsAudio,
    /// The result's own duration setting decides; the audio only conditions it.
    Fixed,
}

/// What happens to audio longer than the model takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioOverrun {
    /// The run is refused before anything is sent, so the user trims it first.
    #[default]
    Refuse,
    /// The provider keeps the start and drops the rest.
    ProviderTrims,
}

/// Which soundtrack the result carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioSound {
    /// The wired audio, unchanged.
    Input,
    /// Sound the model makes itself.
    Generated,
    /// No soundtrack.
    Silent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AudioUse {
    /// The mode's audio input role this describes.
    pub role: String,
    pub duration: AudioDuration,
    /// The longest track the model takes, in milliseconds, when it has a limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ms: Option<u64>,
    #[serde(default)]
    pub longer: AudioOverrun,
    pub sound: AudioSound,
}

impl AudioUse {
    /// One line a person reads in the model picker.
    pub fn summary(&self) -> String {
        let length = match self.duration {
            AudioDuration::FollowsAudio => "length follows the audio",
            AudioDuration::Fixed => "length set by its own setting",
        };
        let limit = match (self.max_ms, self.longer) {
            (Some(ms), AudioOverrun::Refuse) => format!("; audio up to {}", seconds(ms)),
            (Some(ms), AudioOverrun::ProviderTrims) => format!("; audio past {} is cut", seconds(ms)),
            (None, _) => String::new(),
        };
        let sound = match self.sound {
            AudioSound::Input => "keeps the input audio",
            AudioSound::Generated => "makes its own sound",
            AudioSound::Silent => "silent",
        };
        format!("{}: {length}{limit}; {sound}", self.role)
    }
}

/// `ms` as seconds for messages, e.g. `30 s` or `12.5 s`.
pub fn seconds(ms: u64) -> String {
    let (whole, tenths) = (ms / 1000, (ms % 1000) / 100);
    if tenths == 0 { format!("{whole} s") } else { format!("{whole}.{tenths} s") }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn summaries_and_seconds_read_plainly() {
        assert_eq!(seconds(30_000), "30 s");
        assert_eq!(seconds(12_540), "12.5 s");
        assert_eq!(seconds(u64::MAX), "18446744073709551.6 s", "the largest length prints without overflow");
        let audio = AudioUse {
            role: "speech".into(),
            duration: AudioDuration::FollowsAudio,
            max_ms: Some(30_000),
            longer: AudioOverrun::Refuse,
            sound: AudioSound::Input,
        };
        assert_eq!(audio.summary(), "speech: length follows the audio; audio up to 30 s; keeps the input audio");
    }
}
