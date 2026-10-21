use std::fmt;

/// Everything that can go wrong reading media. Messages are safe to show to a person.
/// More variants will come with audio, video and 3D, so match with a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MediaError {
    /// The input is larger than the configured cap.
    TooLarge { size: u64, limit: u64 },
    /// The bytes don't match any format NodeCraft knows.
    Unrecognized,
    /// A known format that this operation can't handle (such as decoding a video as an image).
    Unsupported { mime: &'static str },
    /// The image is valid but exceeds a decode limit (dimensions or memory).
    LimitExceeded(String),
    /// The file is damaged or not what its header claims.
    Decode(String),
    /// A known format using a feature NodeCraft can't read yet (a compressed glTF, say). The
    /// message names the feature and is safe to show.
    UnsupportedFeature(String),
    /// A caller passed a parameter that can't work, such as a zero thumbnail size.
    InvalidParameter(&'static str),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { size, limit } => {
                write!(f, "media is {size} bytes, more than the {limit}-byte limit")
            }
            Self::Unrecognized => f.write_str("unrecognised media format"),
            Self::Unsupported { mime } => write!(f, "{mime} is not supported here"),
            Self::LimitExceeded(m) => write!(f, "media exceeds a decode limit: {m}"),
            Self::Decode(m) => write!(f, "could not decode media: {m}"),
            Self::UnsupportedFeature(m) => f.write_str(m),
            Self::InvalidParameter(m) => write!(f, "invalid parameter: {m}"),
        }
    }
}

impl std::error::Error for MediaError {}

impl From<image::ImageError> for MediaError {
    fn from(e: image::ImageError) -> Self {
        match e {
            image::ImageError::Limits(l) => Self::LimitExceeded(l.to_string()),
            other => Self::Decode(other.to_string()),
        }
    }
}
