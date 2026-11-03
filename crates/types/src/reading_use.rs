//! What a text mode promises about how it reads its inputs (#84): its context window and longest
//! answer, how it samples a wired video into frames, and whether a transcript carries timestamps
//! and speaker labels. None of these is universal: each is a provider capability a catalog
//! declares per mode, and what a mode doesn't declare it doesn't offer. Catalogs check it; the
//! model picker shows it.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};

/// The largest context window or answer a catalog may declare, in tokens.
pub const MAX_DECLARED_TOKENS: u32 = 100_000_000;
/// Most frames a catalog may let one video send.
pub const MAX_FRAMES: u32 = 100_000;
/// The longest gap between sampled frames a catalog may declare: an hour.
pub const MAX_FRAME_EVERY_MS: u32 = 3_600_000;

/// How a mode turns a wired video into the frames the model reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameSampling {
    /// One frame every this many milliseconds of video.
    pub every_ms: u32,
    /// Most frames one video sends; a longer video is sampled no further.
    pub max: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadingUse {
    /// The model's context window: prompt, inputs and answer together, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// The longest answer it writes, in tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// How it samples a wired video. A text mode that reads video must say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<FrameSampling>,
    /// Its transcript or description says when things happen.
    #[serde(default)]
    pub timestamps: bool,
    /// Its transcript says who speaks.
    #[serde(default)]
    pub speakers: bool,
}

fn invalid(message: String) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

impl ReadingUse {
    pub fn validate(&self) -> Result<(), Error> {
        for (name, tokens) in [("context_tokens", self.context_tokens), ("max_output_tokens", self.max_output_tokens)] {
            if tokens.is_some_and(|tokens| tokens == 0 || tokens > MAX_DECLARED_TOKENS) {
                return Err(invalid(format!("`{name}` is 1 to {MAX_DECLARED_TOKENS}")));
            }
        }
        if let (Some(context), Some(answer)) = (self.context_tokens, self.max_output_tokens)
            && answer > context
        {
            return Err(invalid("`max_output_tokens` can't be more than `context_tokens`".into()));
        }
        if let Some(frames) = self.frames {
            if frames.every_ms == 0 || frames.every_ms > MAX_FRAME_EVERY_MS {
                return Err(invalid(format!("`frames.every_ms` is 1 to {MAX_FRAME_EVERY_MS}")));
            }
            if frames.max == 0 || frames.max > MAX_FRAMES {
                return Err(invalid(format!("`frames.max` is 1 to {MAX_FRAMES}")));
            }
        }
        Ok(())
    }

    /// One line a person reads in the model picker; empty when nothing is declared.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(tokens) = self.context_tokens {
            parts.push(format!("reads up to {tokens} tokens"));
        }
        if let Some(tokens) = self.max_output_tokens {
            parts.push(format!("answers up to {tokens} tokens"));
        }
        if let Some(frames) = self.frames {
            parts.push(format!("video: a frame every {}, at most {} frames", every(frames.every_ms), frames.max));
        }
        match (self.timestamps, self.speakers) {
            (true, true) => parts.push("timestamps and speaker labels".into()),
            (true, false) => parts.push("timestamps".into()),
            (false, true) => parts.push("speaker labels".into()),
            (false, false) => {}
        }
        let mut line = parts.join("; ");
        if let Some(first) = line.get(..1) {
            let upper = first.to_ascii_uppercase();
            line.replace_range(..1, &upper);
        }
        line
    }
}

/// A sampling gap in plain words: whole seconds as seconds, anything else in milliseconds.
fn every(ms: u32) -> String {
    match ms {
        1000 => "second".into(),
        ms if ms % 1000 == 0 => format!("{} seconds", ms / 1000),
        ms => format!("{ms} ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> ReadingUse {
        ReadingUse {
            context_tokens: Some(128_000),
            max_output_tokens: Some(8_000),
            frames: Some(FrameSampling { every_ms: 1000, max: 600 }),
            timestamps: true,
            speakers: true,
        }
    }

    #[test]
    fn declarations_are_checked() {
        assert!(full().validate().is_ok());
        assert!(ReadingUse::default().validate().is_ok(), "declaring nothing promises nothing");
        assert!(ReadingUse { context_tokens: Some(0), ..full() }.validate().is_err());
        assert!(ReadingUse { context_tokens: Some(MAX_DECLARED_TOKENS + 1), ..full() }.validate().is_err());
        assert!(ReadingUse { max_output_tokens: Some(200_000), ..full() }.validate().is_err(), "an answer longer than the window");
        assert!(ReadingUse { frames: Some(FrameSampling { every_ms: 0, max: 1 }), ..full() }.validate().is_err());
        assert!(ReadingUse { frames: Some(FrameSampling { every_ms: 500, max: MAX_FRAMES + 1 }), ..full() }.validate().is_err());
    }

    #[test]
    fn the_picker_line_names_only_what_is_declared() {
        assert_eq!(
            full().summary(),
            "Reads up to 128000 tokens; answers up to 8000 tokens; video: a frame every second, at most 600 frames; timestamps and speaker labels"
        );
        assert_eq!(ReadingUse::default().summary(), "");
        let sampled = ReadingUse { frames: Some(FrameSampling { every_ms: 2500, max: 16 }), ..Default::default() };
        assert_eq!(sampled.summary(), "Video: a frame every 2500 ms, at most 16 frames");
        assert_eq!(ReadingUse { speakers: true, ..Default::default() }.summary(), "Speaker labels");
        assert_eq!(every(4000), "4 seconds");
    }

    #[test]
    fn a_catalog_entry_reads_from_toml_shaped_json() {
        let read: ReadingUse = serde_json::from_value(serde_json::json!({"context_tokens": 1000, "frames": {"every_ms": 1000, "max": 8}})).unwrap();
        assert_eq!(read.frames, Some(FrameSampling { every_ms: 1000, max: 8 }));
        assert!(!read.timestamps && !read.speakers);
        assert!(serde_json::from_value::<ReadingUse>(serde_json::json!({"fps": 1})).is_err(), "unknown fields are refused");
    }
}
