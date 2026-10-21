//! Style descriptors (#167): what a trained style is, as NodeCraft keeps it. The weights stay with
//! the provider that trained them; the descriptor names the trainer, the base-model family it was
//! trained for, its trigger word and where the weights are (`docs/decisions/2026-10-21-trained-styles.md`).
//!
//! A descriptor is compact JSON whose first key is its format version, so [`crate::sniff`] tells it
//! from other text by its first bytes. It is read from provider replies and project files, so
//! [`decode`] bounds its size and checks every field.
use serde::{Deserialize, Serialize};

use crate::MediaError;

/// The MIME type of a style descriptor.
pub const MIME: &str = "application/vnd.nodecraft.style+json";
/// How every descriptor this version writes begins.
pub const MAGIC: &[u8] = b"{\"nodecraft_style\":1,";
/// Most bytes in a descriptor.
pub const MAX_BYTES: usize = 16 * 1024;
/// Most bytes in a weights link.
pub const MAX_URL_BYTES: usize = 2048;
/// Most bytes in a trigger word or phrase.
pub const MAX_TRIGGER_BYTES: usize = 256;
const MAX_ID_BYTES: usize = 128;

/// A descriptor as written: its format version, then the style.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    /// Always first, so the bytes start with [`MAGIC`].
    nodecraft_style: u32,
    provider: String,
    trainer: String,
    family: String,
    #[serde(default)]
    trigger: String,
    weights: String,
}

/// A trained style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Style {
    /// The provider adapter that trained it and keeps the weights (`fal`).
    pub provider: String,
    /// The provider's id for the trainer that trained it (`fal-ai/flux-lora-fast-training`), not
    /// the catalog id.
    pub trainer: String,
    /// The base-model family it was trained for: only models of this family can use it.
    pub family: String,
    /// The word or phrase that calls the style up in a prompt; empty when it has none.
    pub trigger: String,
    /// Where the provider keeps the weights.
    pub weights: String,
}

fn invalid(message: &str) -> MediaError {
    MediaError::Decode(format!("style: {message}"))
}

/// A catalog-style id: lowercase letters, digits and `.`, `-`, `_`, `/`.
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_' | b'/'))
}

/// A family: an id with no `/`, so it can be a MIME parameter unquoted. The same rule as
/// `nodecraft_types::style_family`, which this crate can't depend on.
fn family(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'))
}

/// Whether `url` is a plain `https` link: a host and path, with no user info, query or fragment,
/// so a signed or keyed link is never kept.
pub fn plain_https(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split('/').next().unwrap_or_default();
    url.len() <= MAX_URL_BYTES && !host.is_empty() && !host.contains('@') && !url.contains(['?', '#']) && url.bytes().all(|b| b.is_ascii_graphic())
}

impl Style {
    pub fn validate(&self) -> Result<(), MediaError> {
        for (name, value) in [("provider", &self.provider), ("trainer", &self.trainer)] {
            if !id(value) {
                return Err(invalid(&format!("the {name} isn't an id")));
            }
        }
        if !family(&self.family) {
            return Err(invalid("the family isn't a family id"));
        }
        if self.trigger.len() > MAX_TRIGGER_BYTES || self.trigger.chars().any(char::is_control) {
            return Err(invalid("the trigger is one line of at most 256 bytes"));
        }
        if !plain_https(&self.weights) {
            return Err(invalid("the weights link must be a plain https link with no query or user info"));
        }
        Ok(())
    }
}

/// The descriptor's bytes.
pub fn encode(style: &Style) -> Result<Vec<u8>, MediaError> {
    style.validate()?;
    let Style { provider, trainer, family, trigger, weights } = style.clone();
    let stored = Stored { nodecraft_style: 1, provider, trainer, family, trigger, weights };
    let bytes = serde_json::to_vec(&stored).map_err(|_| invalid("can't be written"))?;
    if bytes.len() > MAX_BYTES {
        return Err(MediaError::TooLarge { size: bytes.len() as u64, limit: MAX_BYTES as u64 });
    }
    Ok(bytes)
}

/// Read a descriptor, checking its size, version and every field.
pub fn decode(bytes: &[u8]) -> Result<Style, MediaError> {
    if bytes.len() > MAX_BYTES {
        return Err(MediaError::TooLarge { size: bytes.len() as u64, limit: MAX_BYTES as u64 });
    }
    if !bytes.starts_with(MAGIC) {
        return Err(invalid("not a NodeCraft style, or one from a newer version"));
    }
    let stored: Stored = serde_json::from_slice(bytes).map_err(|_| invalid("damaged"))?;
    if stored.nodecraft_style != 1 {
        return Err(invalid("not a NodeCraft style, or one from a newer version"));
    }
    let Stored { provider, trainer, family, trigger, weights, .. } = stored;
    let style = Style { provider, trainer, family, trigger, weights };
    style.validate()?;
    Ok(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> Style {
        Style {
            provider: "fal".into(),
            trainer: "fal-ai/flux-lora-fast-training".into(),
            family: "flux-1".into(),
            trigger: "inkwash style".into(),
            weights: "https://files.example/abc/weights.safetensors".into(),
        }
    }

    #[test]
    fn round_trips_and_sniffs_as_a_style() {
        let bytes = encode(&style()).unwrap();
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(decode(&bytes).unwrap(), style());
        let sniffed = crate::sniff(&bytes).unwrap();
        assert_eq!((sniffed.mime, sniffed.kind), (MIME, crate::MediaKind::Style));
    }

    #[test]
    fn keyed_or_odd_links_and_bad_fields_are_refused() {
        for weights in [
            "http://files.example/w",
            "https://user:pass@files.example/w",
            "https://files.example/w?token=secret",
            "https://files.example/w#x",
            "https:///w",
            "https://files.example/a b",
        ] {
            assert!(encode(&Style { weights: weights.into(), ..style() }).is_err(), "{weights}");
        }
        for family in ["Flux 1", "flux/dev", "flux;x=y", ""] {
            assert!(encode(&Style { family: family.into(), ..style() }).is_err(), "{family}");
        }
        assert!(encode(&Style { trigger: "a\nb".into(), ..style() }).is_err());
        assert!(encode(&Style { weights: format!("https://files.example/{}", "a".repeat(MAX_URL_BYTES)), ..style() }).is_err());
    }

    #[test]
    fn hostile_bytes_are_refused_not_panicked_on() {
        let good = encode(&style()).unwrap();
        let mut extra = good.clone();
        extra.truncate(extra.len() - 1);
        extra.extend_from_slice(b",\"run\":\"x\"}");
        let mut newer = good.clone();
        newer[MAGIC.len() - 2] = b'2';
        for bytes in [&b""[..], b"{}", &good[..good.len() - 1], &extra, &newer, &vec![b' '; MAX_BYTES + 1]] {
            assert!(decode(bytes).is_err());
        }
        let mut huge = MAGIC.to_vec();
        huge.resize(MAX_BYTES + 1, b' ');
        assert!(matches!(decode(&huge), Err(MediaError::TooLarge { .. })));
    }
}
