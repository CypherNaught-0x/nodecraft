//! Authors' signing keys (#866): a signature made with a key file verifies against the trust
//! entry printed for it, and the secret never shows outside the key file.
use nodecraft_plugin::PackageDigest;
use nodecraft_plugin::id::Token;
use nodecraft_plugin::signature::{self, PackageSignature, Provenance, SigningKey, TrustStore};
use std::path::Path;

const SECRET: [u8; 32] = [7; 32];

fn key() -> SigningKey {
    SigningKey::from_secret(Token::parse("author-key").unwrap(), SECRET)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn a_signature_verifies_against_the_printed_trust_entry() {
    let key = key();
    let digest = PackageDigest::of_inventory("{}");
    let written = key.sign(&digest).encode().unwrap();
    let signature = PackageSignature::decode(written.as_bytes()).unwrap();
    let entry = key.trust_entry("From the author's site, \"verified\" in person").unwrap();
    let trust = TrustStore::decode(format!(r#"{{"keys":[{entry}],"trust_version":1}}"#).as_bytes()).unwrap();
    let provenance = signature::verify(&digest, Some(&signature), &trust, Some(key.key_id())).unwrap();
    assert_eq!(provenance, Provenance::Verified { key_id: key.key_id().clone() });
    // Another package's digest doesn't verify with this signature.
    assert!(signature::verify(&PackageDigest::of_inventory("[]"), Some(&signature), &trust, None).is_err());
}

#[test]
fn the_key_file_round_trips_and_the_secret_stays_in_it() {
    let key = key();
    let file = key.encode_secret().unwrap();
    assert!(file.contains(&hex(&SECRET)));
    let read = SigningKey::decode(file.as_bytes()).unwrap();
    assert_eq!((read.key_id(), read.public_key()), (key.key_id(), key.public_key()));
    let shown = format!("{read:?}{}", key.trust_entry("note").unwrap());
    assert!(!shown.contains(&hex(&SECRET)), "{shown}");
}

#[test]
fn a_bad_key_file_names_the_field_and_never_its_value() {
    let secret = "zz".repeat(32);
    let bad = format!(r#"{{"algorithm":"ed25519","key_id":"author-key","key_version":1,"secret_key":"{secret}"}}"#);
    let error = SigningKey::decode(bad.as_bytes()).unwrap_err();
    assert_eq!(error.at, "/secret_key");
    assert!(!error.to_string().contains(&secret), "{error}");
    for (file, at) in [
        (r#"{"algorithm":"ed25519","key_id":"author-key","key_version":2,"secret_key":""}"#, "/key_version"),
        (r#"{"algorithm":"rsa","key_id":"author-key","key_version":1,"secret_key":""}"#, "/algorithm"),
        (r#"{"algorithm":"ed25519","key_id":"Author","key_version":1,"secret_key":""}"#, "/key_id"),
    ] {
        assert_eq!(SigningKey::decode(file.as_bytes()).unwrap_err().at, at, "{file}");
    }
    assert!(key().trust_entry("two\nlines").is_err());
}

#[test]
fn encoding_matches_the_fixture_signature_byte_for_byte() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/package/textpack/signature.json");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(PackageSignature::decode(&bytes).unwrap().encode().unwrap().as_bytes(), bytes.as_slice());
}
