//! Detached signatures (`signature.json`) and the local trust file.
//!
//! **Algorithm: Ed25519** (RFC 8032), verified strictly (non-canonical encodings and small-order
//! keys are refused). The signed message is `nodecraft-plugin-signature-v1\0` followed by the
//! 32-byte package digest, a different domain from the digest's own so a signature can never be
//! mistaken for a hash input. A signature proves only that a key the person trusts signed this
//! exact digest; it says nothing about safety or licensing.
//!
//! **Trust is local.** The trust file lists public keys the person added, each with where it came
//! from (free text, shown to them) and a status. Revoking a key keeps its entry with
//! `"status":"revoked"`, so a later import of the same key can't silently trust it again; every
//! signature by a revoked key is refused, whenever it was made (protocol 1 has no timestamps).
//! There is no registry, no network lookup and no key rotation chain.
use crate::digest::PackageDigest;
use crate::error::{Code, Result, WireError, err};
use crate::id::{Token, located};
use crate::json::{self, Fields, Limits};
use std::collections::BTreeMap;

/// The signature message's domain.
pub const SIGNATURE_DOMAIN: &[u8] = b"nodecraft-plugin-signature-v1\0";
/// Signature and trust file formats this host reads.
pub const SIGNATURE_VERSION: u32 = 1;
pub const TRUST_VERSION: u32 = 1;
/// Most keys in a trust file.
pub const MAX_KEYS: usize = 256;
/// Longest provenance note.
pub const MAX_PROVENANCE: usize = 256;

const LIMITS: Limits = Limits { bytes: 128 * 1024, depth: 4, string_bytes: 1024, entries: MAX_KEYS + 1, raw_numbers: false };

/// The bytes a package signature signs.
pub fn signed_message(digest: &PackageDigest) -> Vec<u8> {
    let mut message = SIGNATURE_DOMAIN.to_vec();
    message.extend_from_slice(digest.as_bytes());
    message
}

/// A decoded `signature.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageSignature {
    pub key_id: Token,
    pub package_digest: PackageDigest,
    pub signature: [u8; 64],
}

impl PackageSignature {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut f = Fields::new(json::parse(bytes, LIMITS)?, "")?;
        if f.u32("signature_version")? != SIGNATURE_VERSION {
            return Err(WireError::new(Code::BadVersion, "this host reads signature version 1").at("/signature_version"));
        }
        if f.string("algorithm")? != "ed25519" {
            return Err(WireError::new(Code::UnsupportedFeature, "the only signature algorithm is ed25519").at("/algorithm"));
        }
        let key_id = located(Token::parse(&f.string("key_id")?), "/key_id")?;
        let package_digest = located(PackageDigest::parse(&f.string("package_digest")?), "/package_digest")?;
        let signature = located(json::hex::<64>(&f.string("signature")?), "/signature")?;
        f.finish()?;
        Ok(Self { key_id, package_digest, signature })
    }
}

/// Whether a key may be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStatus {
    Trusted,
    Revoked,
}

/// One key in the trust file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedKey {
    pub public_key: [u8; 32],
    /// Where the person got it, as they wrote it.
    pub provenance: String,
    pub status: KeyStatus,
}

/// The local trust file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustStore {
    pub keys: BTreeMap<Token, TrustedKey>,
}

impl TrustStore {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut f = Fields::new(json::parse(bytes, LIMITS)?, "")?;
        if f.u32("trust_version")? != TRUST_VERSION {
            return Err(WireError::new(Code::BadVersion, "this host reads trust version 1").at("/trust_version"));
        }
        let items = f.array("keys", MAX_KEYS)?;
        f.finish()?;
        let mut keys = BTreeMap::new();
        let mut publics = std::collections::BTreeSet::new();
        for (i, item) in items.into_iter().enumerate() {
            let at = format!("/keys/{i}");
            let mut k = Fields::new(item, &at)?;
            let key_id = located(Token::parse(&k.string("key_id")?), &k.path("key_id"))?;
            if k.string("algorithm")? != "ed25519" {
                return Err(WireError::new(Code::UnsupportedFeature, "the only key algorithm is ed25519").at(&k.path("algorithm")));
            }
            let public_key = located(json::hex::<32>(&k.string("public_key")?), &k.path("public_key"))?;
            if ed25519_dalek::VerifyingKey::from_bytes(&public_key).map_or(true, |key| key.is_weak()) {
                return Err(WireError::new(Code::BadSignature, "not a usable Ed25519 public key").at(&k.path("public_key")));
            }
            let provenance = k.string("provenance")?;
            if provenance.len() > MAX_PROVENANCE || provenance.chars().any(char::is_control) {
                return Err(WireError::new(Code::TooLarge, "provenance is one line of at most 256 bytes").at(&k.path("provenance")));
            }
            let status = match k.string("status")?.as_str() {
                "trusted" => KeyStatus::Trusted,
                "revoked" => KeyStatus::Revoked,
                _ => return Err(WireError::new(Code::WrongType, "status is trusted or revoked").at(&k.path("status"))),
            };
            k.finish()?;
            if !publics.insert(public_key) || keys.contains_key(&key_id) {
                return Err(WireError::new(Code::Collision, "a key ID or public key is listed twice").at(&at));
            }
            keys.insert(key_id, TrustedKey { public_key, provenance, status });
        }
        Ok(Self { keys })
    }
}

/// What a package's signature establishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// No signature: the package is local and unverified until the person trusts it.
    Unsigned,
    /// Signed by a trusted key.
    Verified { key_id: Token },
    /// Signed, but by a key the trust file doesn't list.
    UnknownKey { key_id: Token },
}

/// Checks `signature` (if any) for the package with `digest` against `trust`. `publisher` is
/// the manifest's `publisher_key_id`: when the manifest names one, only that key may have signed.
/// A signature for another digest or by another key, by a revoked key, or that doesn't verify is
/// an error, never a weaker status.
pub fn verify(digest: &PackageDigest, signature: Option<&PackageSignature>, trust: &TrustStore, publisher: Option<&Token>) -> Result<Provenance> {
    let Some(signature) = signature else {
        return Ok(Provenance::Unsigned);
    };
    if signature.package_digest != *digest {
        return err(Code::BadDigest, "the signature is for another package digest");
    }
    if publisher.is_some_and(|p| *p != signature.key_id) {
        return err(Code::BadSignature, "signed by a key other than the manifest's publisher");
    }
    let Some(key) = trust.keys.get(&signature.key_id) else {
        return Ok(Provenance::UnknownKey { key_id: signature.key_id.clone() });
    };
    if key.status == KeyStatus::Revoked {
        return err(Code::BadSignature, "the signing key is revoked");
    }
    let public = ed25519_dalek::VerifyingKey::from_bytes(&key.public_key).map_err(|_| WireError::new(Code::BadSignature, "not a usable Ed25519 public key"))?;
    let sig = ed25519_dalek::Signature::from_bytes(&signature.signature);
    public.verify_strict(&signed_message(digest), &sig).map_err(|_| WireError::new(Code::BadSignature, "the signature doesn't verify"))?;
    Ok(Provenance::Verified { key_id: signature.key_id.clone() })
}
