use std::fmt;

/// BLAKE3 hash of a piece of media's bytes: the key for the asset store and thumbnail cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    /// Hashes `bytes`.
    pub fn of(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    /// Parses the 64-character hex form (either case), as used in asset store file names.
    pub fn from_hex(hex: &str) -> Option<Self> {
        blake3::Hash::from_hex(hex).ok().map(|h| Self(*h.as_bytes()))
    }

    /// Lowercase hex, 64 characters.
    pub fn to_hex(&self) -> String {
        blake3::Hash::from_bytes(self.0).to_hex().to_string()
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({})", self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_stable_blake3() {
        // BLAKE3 of the empty input, from the BLAKE3 specification's test vectors.
        assert_eq!(ContentHash::of(b"").to_hex(), "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
        assert_ne!(ContentHash::of(b"a"), ContentHash::of(b"b"));
        assert_eq!(ContentHash::of(b"a").to_string().len(), 64);
    }

    #[test]
    fn hex_round_trip_and_bad_input() {
        let h = ContentHash::of(b"cat");
        assert_eq!(ContentHash::from_hex(&h.to_hex()), Some(h));
        assert_eq!(ContentHash::from_hex(&h.to_hex().to_uppercase()), Some(h));
        for bad in ["", "abc", &"g".repeat(64), &"a".repeat(65), "é".repeat(32).as_str()] {
            assert_eq!(ContentHash::from_hex(bad), None, "{bad}");
        }
    }
}
