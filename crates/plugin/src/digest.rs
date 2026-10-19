//! The package digest: BLAKE3 of `nodecraft-plugin-v1\0` followed by the canonical inventory
//! JSON. On the wire it is `blake3:` and 64 lowercase hex digits; the prefix isn't hashed.
use crate::error::{Code, Result, err};
use crate::json;
use std::fmt;

/// Domain separation for package digests.
pub const DIGEST_DOMAIN: &[u8] = b"nodecraft-plugin-v1\0";
/// Wire prefix of a package digest.
pub const DIGEST_PREFIX: &str = "blake3:";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageDigest(pub [u8; 32]);

impl PackageDigest {
    /// The digest of a package whose canonical inventory JSON is `inventory`.
    pub fn of_inventory(inventory: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(DIGEST_DOMAIN);
        hasher.update(inventory.as_bytes());
        Self(*hasher.finalize().as_bytes())
    }

    /// Parses `blake3:<64 lowercase hex>`.
    pub fn parse(text: &str) -> Result<Self> {
        match text.strip_prefix(DIGEST_PREFIX) {
            Some(hex) => json::hex::<32>(hex).map(Self),
            None => err(Code::BadDigest, "a package digest is blake3: and 64 lowercase hex digits"),
        }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for PackageDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{DIGEST_PREFIX}{}", json::to_hex(&self.0))
    }
}
