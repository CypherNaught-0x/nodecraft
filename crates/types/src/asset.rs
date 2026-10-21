use serde::{Deserialize, Serialize};

use crate::{Error, ErrorKind};

/// A BLAKE3 digest, encoded as 64 lowercase hexadecimal characters in project files.
/// This type holds a digest; hashing media belongs to the asset store.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Blake3([u8; 32]);

impl Blake3 {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::str::FromStr for Blake3 {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::new(ErrorKind::InvalidInput, "BLAKE3 digest must contain 64 hexadecimal characters"));
        }
        let mut bytes = [0; 32];
        let (pairs, _) = value.as_bytes().as_chunks::<2>();
        for (slot, pair) in bytes.iter_mut().zip(pairs) {
            let mut nibbles = pair.iter().filter_map(|b| char::from(*b).to_digit(16));
            let high = nibbles.next().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid digest byte"))?;
            let low = nibbles.next().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "invalid digest byte"))?;
            *slot = (high * 16 + low) as u8;
        }
        Ok(Self(bytes))
    }
}

impl TryFrom<String> for Blake3 {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Blake3> for String {
    fn from(value: Blake3) -> Self {
        value.to_string()
    }
}

impl std::fmt::Display for Blake3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Blake3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Blake3({self})")
    }
}

/// A validated MIME value, including optional parameters (for example a text charset).
/// Equality preserves parameter spelling/spacing. Use `essence()` to compare media types.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Mime(String);

impl Mime {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Lowercase type/subtype without parameters, suitable for selecting a decoder.
    pub fn essence(&self) -> &str {
        self.0.split(';').next().unwrap_or_default().trim_end()
    }

    /// The value of token parameter `name` (matched without regard to case). A type with any
    /// quoted parameter answers none: a quoted string can hide a `;`, and splitting on it would
    /// read a different value than the MIME parser does.
    pub fn param(&self, name: &str) -> Option<&str> {
        if self.0.contains('"') {
            return None;
        }
        self.0.split(';').skip(1).find_map(|part| {
            let (key, value) = part.split_once('=')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }
}

impl std::str::FromStr for Mime {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Bound parser work and reject header injection before invoking the MIME parser.
        if value.len() > 1024 || value.bytes().any(|b| b.is_ascii_control()) {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid MIME value"));
        }
        let parsed = value.parse::<mime::Mime>().map_err(|_| Error::new(ErrorKind::InvalidInput, "invalid MIME value"))?;
        if parsed.type_().as_str().is_empty() || parsed.subtype().as_str().is_empty() || parsed.type_() == mime::STAR || parsed.subtype() == mime::STAR {
            return Err(Error::new(ErrorKind::InvalidInput, "asset MIME must be concrete"));
        }
        Ok(Self(parsed.to_string()))
    }
}

impl TryFrom<String> for Mime {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Mime> for String {
    fn from(value: Mime) -> Self {
        value.0
    }
}

/// What an image file says about its colour space (#393). NodeCraft decodes every image as sRGB
/// and applies no profile yet, so this records the file's claim, not a conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorSpace {
    /// The file declares no colour space. NodeCraft reads it as sRGB by convention; that is an
    /// assumption, not something the file proves.
    Untagged,
    /// The file declares sRGB.
    Srgb,
    /// The file embeds an ICC profile (or profile data that can't be read), which NodeCraft
    /// doesn't apply yet.
    IccProfile,
    /// The file declares another colour space without a profile (a PNG `cICP`, `gAMA` or `cHRM`
    /// chunk), which NodeCraft doesn't apply yet.
    Other,
}

/// Optional facts reported by a media probe. Integers avoid NaN/infinity; consumers must still
/// cap dimensions, durations and allocation sizes before decoding hostile assets. An absent
/// field means unknown: never read it as 8-bit, opaque or sRGB.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MediaMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channels: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    /// A 3D model's vertex count, after decoding (shared corners count once).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertices: Option<u32>,
    /// A 3D model's triangle count, after its faces are split into triangles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triangles: Option<u32>,
    /// An image decodes with an alpha channel, including transparency such as a PNG `tRNS` chunk
    /// or a GIF transparent colour (it may still be fully opaque).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_alpha: Option<bool>,
    /// An image's stored bits per channel (1, 2, 4, 8, 16 or 32), before any decoding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits_per_channel: Option<u8>,
    /// What an image file says about its colour space. Absent for formats whose colour signals
    /// NodeCraft doesn't all read (BMP, GIF, TIFF, or a JPEG/WebP/PNG carrying EXIF).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_space: Option<ColorSpace>,
}

impl MediaMeta {
    /// Whether this claims anything an image header can confirm: its size, alpha, bit depth or
    /// colour space.
    pub fn claims_image_header(&self) -> bool {
        self.width.is_some() || self.height.is_some() || self.has_alpha.is_some() || self.bits_per_channel.is_some() || self.color_space.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetRef {
    pub hash: Blake3,
    pub mime: Mime,
    #[serde(default)]
    pub meta: MediaMeta,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn asset_round_trip(
            bytes in any::<[u8; 32]>(),
            width in any::<u32>(),
            duration in any::<u64>(),
            has_alpha in any::<Option<bool>>(),
            bits_per_channel in any::<Option<u8>>(),
            color_space in proptest::option::of(prop_oneof![
                Just(ColorSpace::Untagged),
                Just(ColorSpace::Srgb),
                Just(ColorSpace::IccProfile),
                Just(ColorSpace::Other),
            ]),
        ) {
            let hash = Blake3::from_bytes(bytes);
            prop_assert_eq!(hash.to_string().parse::<Blake3>().unwrap(), hash);
            prop_assert_eq!(hash.to_string().to_uppercase().parse::<Blake3>().unwrap(), hash);
            let asset = AssetRef { hash, mime: "image/png".parse().unwrap(), meta: MediaMeta {
                width: Some(width),
                duration_ms: Some(duration),
                has_alpha,
                bits_per_channel,
                color_space,
                ..Default::default()
            } };
            prop_assert_eq!(serde_json::from_str::<AssetRef>(&serde_json::to_string(&asset).unwrap()).unwrap(), asset);
        }

        #[test]
        fn arbitrary_hash_and_mime_input_is_fallible(text in ".{0,1100}") {
            let _ = text.parse::<Blake3>();
            let _ = text.parse::<Mime>();
        }
    }

    #[test]
    fn rejects_invalid_digests_and_mime() {
        for value in ["".to_owned(), "0".repeat(63), "0".repeat(65), "g".repeat(64), "é".repeat(32)] {
            assert!(value.parse::<Blake3>().is_err());
            assert!(serde_json::from_value::<Blake3>(serde_json::json!(value)).is_err());
        }
        for value in ["", "image", "image/", "*/*", "image/*", "image/png\r\nx: y"] {
            assert!(value.parse::<Mime>().is_err(), "{value}");
        }
        assert!("text/plain; charset=utf-8".parse::<Mime>().is_ok());
        let json = serde_json::json!({"hash": "00".repeat(32), "mime": "image/png"});
        assert_eq!(serde_json::from_value::<AssetRef>(json).unwrap().meta, MediaMeta::default());
    }

    #[test]
    fn mime_essence_ignores_parameter_spelling_and_empty_metadata_is_compact() {
        for value in ["TEXT/PLAIN", "text/plain;charset=utf-8", "text/plain; charset=\"utf-8\"", "text/plain; charset=UTF-8"] {
            assert_eq!(value.parse::<Mime>().unwrap().essence(), "text/plain");
        }
        let style: Mime = "application/vnd.nodecraft.style+json; Family=flux-1; x=y".parse().unwrap();
        assert_eq!((style.param("family"), style.param("x"), style.param("charset")), (Some("flux-1"), Some("y"), None));
        let hidden: Mime = "application/vnd.nodecraft.style+json; x=\"a;family=sdxl-1\"; family=flux-1".parse().unwrap();
        assert_eq!(hidden.param("family"), None, "a quoted parameter is never split on its `;`");
        assert_eq!(serde_json::to_string(&MediaMeta::default()).unwrap(), "{}");
        assert!(!MediaMeta::default().claims_image_header());
        let hash = Blake3::from_bytes([0xab; 32]);
        assert_eq!(format!("{hash:?}"), format!("Blake3({})", "ab".repeat(32)));
    }

    #[test]
    fn image_header_facts_have_a_stable_spelling_and_absence_stays_unknown() {
        let meta = MediaMeta { has_alpha: Some(false), bits_per_channel: Some(16), color_space: Some(ColorSpace::IccProfile), ..Default::default() };
        assert_eq!(serde_json::to_value(&meta).unwrap(), serde_json::json!({"has_alpha": false, "bits_per_channel": 16, "color_space": "icc_profile"}));
        for (space, name) in
            [(ColorSpace::Untagged, "untagged"), (ColorSpace::Srgb, "srgb"), (ColorSpace::IccProfile, "icc_profile"), (ColorSpace::Other, "other")]
        {
            assert_eq!(serde_json::to_value(space).unwrap(), serde_json::json!(name));
        }
        assert!(meta.claims_image_header());
        // A reference written before #393 reads back with every new fact unknown.
        let old: MediaMeta = serde_json::from_value(serde_json::json!({"width": 4, "height": 2})).unwrap();
        assert_eq!((old.has_alpha, old.bits_per_channel, old.color_space), (None, None, None));
        assert!(serde_json::from_value::<MediaMeta>(serde_json::json!({"color_space": "display_p3"})).is_err());
        assert!(serde_json::from_value::<MediaMeta>(serde_json::json!({"bits_per_channel": 256})).is_err());
    }
}
