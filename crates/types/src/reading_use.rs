//! What a text mode promises about how it reads its inputs (#84): its context window and longest
//! answer, how it samples a wired video into frames, and whether a transcript carries timestamps
//! and speaker labels. None of these is universal: each is a provider capability a catalog
//! declares per mode, and what a mode doesn't declare it doesn't offer. Catalogs check it; the
//! model picker shows it.
use crate::{Error, ErrorKind};
use serde::{Deserialize, Serialize};

/// The largest context window or answer a catalog may declare, in tokens.
pub const MAX_DECLARED_TOKENS: u32 = 100_000_000;
/// What a context-window refusal's message carries (#84), whether the provider refused the
/// request or planning did before sending it, so hosts that keep only the text can tell it from
/// other unsupported inputs.
pub const CONTEXT_LIMIT: &str = "the prompt and its inputs are longer than the model's context window";
/// What to do about a context-window refusal.
pub const CONTEXT_LIMIT_FIX: &str = "shorten the prompt, connect fewer or smaller inputs, or pick a model with a larger context";
/// The most UTF-8 bytes one token is assumed to cover when planning estimates a prompt's tokens
/// (#84). Tokenizers cover well under this on ordinary prose and code (Anthropic documents about
/// 2.5 characters a token for its current tokenizer), so for ordinary text the estimate
/// undercounts and a prompt it puts over the window is clearly over. Unusual text (long runs of
/// one character) can pack more bytes into a token; the provider's own refusal still applies.
pub const BYTES_PER_TOKEN_AT_MOST: usize = 6;
/// Most frames a catalog may let one video send.
pub const MAX_FRAMES: u32 = 100_000;
/// The longest gap between sampled frames a catalog may declare: an hour.
pub const MAX_FRAME_EVERY_MS: u32 = 3_600_000;
/// Most video types a mode may list.
pub const MAX_VIDEO_TYPES: usize = 32;

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
    /// The video types it reads, by MIME essence; empty means every video type. Planning refuses
    /// a wired video of another type before quoting it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub video_types: Vec<String>,
    /// The most stored bytes of video one request sends, all its videos together. The quote
    /// refuses more before anything is reserved or sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_bytes: Option<u64>,
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
        if self.video_types.len() > MAX_VIDEO_TYPES
            || self.video_types.iter().enumerate().any(|(i, t)| {
                t.len() > 64
                    || !t.starts_with("video/")
                    || !t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'/' | b'-' | b'.' | b'+'))
                    || self.video_types.iter().take(i).any(|p| p == t)
            })
        {
            return Err(invalid(format!("`video_types` lists at most {MAX_VIDEO_TYPES} distinct lowercase `video/` types")));
        }
        if self.video_bytes == Some(0) {
            return Err(invalid("`video_bytes` is at least 1".into()));
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

    /// Refuses `text_bytes` of prompt text that clearly can't fit the declared context window,
    /// before anything is quoted or sent (#84). The estimate is a low one
    /// ([`BYTES_PER_TOKEN_AT_MOST`]) and counts text only, so media never makes it refuse; an
    /// undeclared window refuses nothing. `model` names the model in the message.
    pub fn context_fits(&self, model: &str, text_bytes: usize) -> Result<(), Error> {
        let Some(window) = self.context_tokens else { return Ok(()) };
        let tokens = text_bytes.div_ceil(BYTES_PER_TOKEN_AT_MOST);
        if u64::try_from(tokens).unwrap_or(u64::MAX) <= u64::from(window) {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{CONTEXT_LIMIT}: the prompt alone is {} of text, at least {tokens} tokens, and {model} reads at most {window}; {CONTEXT_LIMIT_FIX}",
                size(text_bytes)
            ),
        ))
    }

    /// Refuses a video whose known length samples more frames than the mode reads, before
    /// anything is sent (#84). A video of unknown length goes through; the provider still checks
    /// it. `role` names the input in the message.
    pub fn frames_fit(&self, role: &str, duration_ms: Option<u64>) -> Result<(), Error> {
        let (Some(frames), Some(ms)) = (self.frames, duration_ms) else { return Ok(()) };
        let longest = u64::from(frames.every_ms).saturating_mul(u64::from(frames.max));
        if ms <= longest {
            return Ok(());
        }
        Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "a video on {role} is {} long and this model reads at most {} of it ({} frames, one every {}); trim it first",
                crate::seconds(ms.div_ceil(100).saturating_mul(100)),
                crate::seconds(longest),
                frames.max,
                every(frames.every_ms)
            ),
        ))
    }

    /// Refuses a wired video of a type the mode doesn't list, before anything is quoted (#84).
    pub fn video_type_fits(&self, role: &str, essence: &str) -> Result<(), Error> {
        if self.video_types.is_empty() || self.video_types.iter().any(|t| t == essence) {
            return Ok(());
        }
        let shown: String = essence.chars().filter(|c| !c.is_control()).take(64).collect();
        Err(Error::new(
            ErrorKind::Unsupported,
            format!("this model doesn't read {shown} video on {role}; it reads {}. Convert the video first", self.video_types.join(", ")),
        ))
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
        if let Some(bytes) = self.video_bytes {
            // Rounded down to a tenth of a megabyte (10^6 bytes, as providers state their limits),
            // so the line never promises more than is declared.
            let tenths = bytes / 100_000;
            let mb = if tenths % 10 == 0 { format!("{}", tenths / 10) } else { format!("{}.{}", tenths / 10, tenths % 10) };
            parts.push(format!("videos up to {mb} MB together"));
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

/// A byte count as a person reads it, rounded down so it never overstates.
fn size(bytes: usize) -> String {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB {
        format!("{}.{} MiB", bytes / MIB, (bytes % MIB) * 10 / MIB)
    } else if bytes >= KIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
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
            video_types: Vec::new(),
            video_bytes: None,
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
    fn only_a_prompt_clearly_past_the_window_is_refused() {
        let small = ReadingUse { context_tokens: Some(10), ..Default::default() };
        assert!(small.context_fits("fixture.model", 60).is_ok(), "60 bytes is at least 10 tokens: at the window, not past it");
        let refused = small.context_fits("fixture.model", 61).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::InvalidInput);
        assert!(refused.message.starts_with(CONTEXT_LIMIT), "{refused}");
        assert!(refused.message.contains("at least 11 tokens") && refused.message.contains("fixture.model reads at most 10"), "{refused}");
        assert!(refused.message.contains("the prompt alone is 61 bytes of text"), "says how large the input is (#1074): {refused}");
        assert_eq!((size(2047), size(3 * 1024 * 1024 + 600 * 1024), size(usize::MAX).is_empty()), ("1 KiB".to_owned(), "3.5 MiB".to_owned(), false));
        assert!(refused.message.ends_with(CONTEXT_LIMIT_FIX), "says what to do: {refused}");
        assert!(ReadingUse::default().context_fits("fixture.model", usize::MAX).is_ok(), "an undeclared window refuses nothing");
        assert!(full().context_fits("m", usize::MAX).is_err(), "no overflow on hostile sizes");
    }

    #[test]
    fn a_known_video_longer_than_the_frames_read_is_refused() {
        let reads = ReadingUse { frames: Some(FrameSampling { every_ms: 1000, max: 60 }), ..Default::default() };
        assert!(reads.frames_fit("video", Some(60_000)).is_ok());
        assert!(reads.frames_fit("video", None).is_ok(), "an unknown length goes through");
        let long = reads.frames_fit("video", Some(60_001)).unwrap_err();
        assert!(long.message.starts_with("a video on video is 60.1 s long"), "{long}");
        assert!(long.message.contains("60 frames, one every second") && long.message.contains("trim it first"), "{long}");
        assert!(ReadingUse::default().frames_fit("video", Some(u64::MAX)).is_ok());
        let wide = ReadingUse { frames: Some(FrameSampling { every_ms: MAX_FRAME_EVERY_MS, max: MAX_FRAMES }), ..Default::default() };
        assert!(wide.frames_fit("video", Some(u64::MAX)).is_err(), "no overflow on hostile lengths");
    }

    #[test]
    fn declared_video_types_and_bytes_are_checked_and_enforced() {
        let reads = ReadingUse { video_types: vec!["video/mp4".into(), "video/webm".into()], video_bytes: Some(15_000_000), ..Default::default() };
        assert!(reads.validate().is_ok());
        assert!(reads.video_type_fits("video", "video/mp4").is_ok());
        let mkv = reads.video_type_fits("video", "video/x-matroska").unwrap_err();
        assert_eq!(mkv.kind, ErrorKind::Unsupported);
        assert!(mkv.message.contains("video/x-matroska") && mkv.message.contains("video/mp4, video/webm"), "{mkv}");
        assert!(ReadingUse::default().video_type_fits("video", "video/x-matroska").is_ok(), "no list means any video");
        assert_eq!(reads.summary(), "Videos up to 15 MB together");
        for bad in [
            ReadingUse { video_types: vec!["image/png".into()], ..Default::default() },
            ReadingUse { video_types: vec!["video/MP4".into()], ..Default::default() },
            ReadingUse { video_types: vec!["video/mp4".into(), "video/mp4".into()], ..Default::default() },
            ReadingUse { video_types: vec!["video/mp4".into(); MAX_VIDEO_TYPES + 1], ..Default::default() },
            ReadingUse { video_bytes: Some(0), ..Default::default() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_catalog_entry_reads_from_toml_shaped_json() {
        let read: ReadingUse = serde_json::from_value(serde_json::json!({"context_tokens": 1000, "frames": {"every_ms": 1000, "max": 8}})).unwrap();
        assert_eq!(read.frames, Some(FrameSampling { every_ms: 1000, max: 8 }));
        assert!(!read.timestamps && !read.speakers);
        assert!(serde_json::from_value::<ReadingUse>(serde_json::json!({"fps": 1})).is_err(), "unknown fields are refused");
    }
}
