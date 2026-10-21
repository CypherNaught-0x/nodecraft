//! `cargo xtask update-key` and `cargo xtask update-feed`: the signed update feed (#540).
//!
//! A release's feed, `nodecraft-update.json`, names each platform's update payload with its size
//! and BLAKE3 hash; `nodecraft-update.json.sig` is the Ed25519 signature of
//! `NodeCraft update feed v1\n` followed by the feed's bytes, in hex. The app checks both against
//! the public keys in `packaging/update-key.pub` (crates/engine/src/update_install.rs) before it
//! installs anything. The private key is the release secret `NODECRAFT_UPDATE_SIGNING_KEY`: 64
//! hex digits, never committed.

use ed25519_dalek::{Signer, SigningKey};
use std::path::Path;

/// Must match `update_install::SIGNED_PREFIX`.
const SIGNED_PREFIX: &[u8] = b"NodeCraft update feed v1\n";
const FEED_FILE: &str = "nodecraft-update.json";
const SIGNATURE_FILE: &str = "nodecraft-update.json.sig";
const PUBLIC_KEYS: &str = "packaging/update-key.pub";
const SECRET_ENV: &str = "NODECRAFT_UPDATE_SIGNING_KEY";

/// Each platform's payload: its feed target and its file name's ending after the version.
const PAYLOADS: &[(&str, &str)] = &[
    ("macos-universal", "-macos-universal.app.zip"),
    ("linux-x86_64", "-linux-x86_64.AppImage"),
    ("linux-aarch64", "-linux-aarch64.AppImage"),
    ("windows-x64", "-windows-x64.msi"),
    ("windows-arm64", "-windows-arm64.msi"),
];

pub fn key(root: &Path, rest: &[&str]) -> Result<(), String> {
    let [secret] = rest else {
        return Err("usage: cargo xtask update-key <file for the private key, outside the repository>".into());
    };
    let secret = Path::new(secret);
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no randomness for a key: {e}"))?;
    let signing = SigningKey::from_bytes(&seed);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(secret).map_err(|e| format!("{}: {e} (it must not exist yet)", secret.display()))?;
    std::io::Write::write_all(&mut file, format!("{}\n", hex(&seed)).as_bytes()).map_err(|e| format!("{}: {e}", secret.display()))?;
    let public = hex(signing.verifying_key().as_bytes());
    let keys = root.join(PUBLIC_KEYS);
    let mut text = std::fs::read_to_string(&keys).map_err(|e| format!("{PUBLIC_KEYS}: {e}"))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&public);
    text.push('\n');
    std::fs::write(&keys, text).map_err(|e| format!("{PUBLIC_KEYS}: {e}"))?;
    println!("public key {public} added to {PUBLIC_KEYS}; commit it.");
    println!("private key written to {}. Store it as the release secret, then delete the file:", secret.display());
    println!("  gh secret set {SECRET_ENV} --env release < {}", secret.display());
    Ok(())
}

pub fn feed(root: &Path, rest: &[&str]) -> Result<(), String> {
    let [dir, version] = rest else {
        return Err("usage: cargo xtask update-feed <release artifact folder> <version>".into());
    };
    crate::version::validate(version)?;
    let secret = std::env::var(SECRET_ENV).map_err(|_| format!("{SECRET_ENV} is not set"))?;
    let seed: [u8; 32] = unhex(secret.trim()).and_then(|bytes| bytes.try_into().ok()).ok_or_else(|| format!("{SECRET_ENV} isn't 64 hex digits"))?;
    let signing = SigningKey::from_bytes(&seed);
    // A feed the app can't verify would only fail on people's machines: refuse it here.
    let public = hex(signing.verifying_key().as_bytes());
    let keys = std::fs::read_to_string(root.join(PUBLIC_KEYS)).map_err(|e| format!("{PUBLIC_KEYS}: {e}"))?;
    if !keys.lines().map(str::trim).any(|line| line.eq_ignore_ascii_case(&public)) {
        return Err(format!("{SECRET_ENV}'s public key {public} isn't in {PUBLIC_KEYS}, so the app would refuse this feed"));
    }
    let dir = Path::new(dir);
    let (body, found) = build(dir, version)?;
    std::fs::write(dir.join(FEED_FILE), &body).map_err(|e| format!("{FEED_FILE}: {e}"))?;
    std::fs::write(dir.join(SIGNATURE_FILE), format!("{}\n", sign(&signing, &body))).map_err(|e| format!("{SIGNATURE_FILE}: {e}"))?;
    println!("signed {FEED_FILE} for {version}: {}", found.join(", "));
    Ok(())
}

/// The feed for the payloads in `dir`, and the targets it lists.
fn build(dir: &Path, version: &str) -> Result<(Vec<u8>, Vec<String>), String> {
    let mut targets = serde_json::Map::new();
    for (target, ending) in PAYLOADS {
        let file = format!("nodecraft-{version}{ending}");
        let path = dir.join(&file);
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| format!("{file}: {e}"))?;
        targets.insert((*target).into(), serde_json::json!({"file": file, "size": bytes.len(), "blake3": blake3::hash(&bytes).to_hex().as_str()}));
    }
    if targets.is_empty() {
        return Err(format!("no update payloads for {version} in {}", dir.display()));
    }
    let found = targets.keys().cloned().collect();
    let feed = serde_json::json!({"format": 1, "version": version, "targets": targets});
    let mut body = serde_json::to_vec_pretty(&feed).map_err(|e| e.to_string())?;
    body.push(b'\n');
    Ok((body, found))
}

fn sign(key: &SigningKey, feed: &[u8]) -> String {
    let mut message = SIGNED_PREFIX.to_vec();
    message.extend_from_slice(feed);
    hex(&key.sign(&message).to_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| text.get(at..at + 2).and_then(|pair| u8::from_str_radix(pair, 16).ok())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's `the_signature_scheme_matches_the_release_tooling` checks this same vector.
    #[test]
    fn signatures_match_the_app_s_scheme() {
        let key = SigningKey::from_bytes(&[7; 32]);
        assert_eq!(hex(key.verifying_key().as_bytes()), "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c");
        assert_eq!(
            sign(&key, b"{}"),
            "d83f7739e03e935b511ef87ddb6cd096ba08b912074caf58d2ce97bc4657c9dfa08b9bcd10ff87cadd1d2a25c662bee95b78901a6e20f06ae039116c64678b06"
        );
        assert_eq!(unhex("0aFf"), Some(vec![0x0a, 0xff]));
        assert_eq!(unhex("0"), None);
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex("é0"), None);
    }

    #[test]
    fn the_feed_lists_the_payloads_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("nodecraft-0.3.0-linux-x86_64.AppImage"), b"appimage").unwrap();
        std::fs::write(dir.path().join("nodecraft-0.3.0-windows-x64.msi"), b"msi").unwrap();
        // Another version's payload and other artifacts are left out.
        std::fs::write(dir.path().join("nodecraft-0.2.0-windows-arm64.msi"), b"old").unwrap();
        std::fs::write(dir.path().join("nodecraft-0.3.0-macos-universal.dmg"), b"dmg").unwrap();
        let (body, found) = build(dir.path(), "0.3.0").unwrap();
        assert_eq!(found, ["linux-x86_64", "windows-x64"]);
        let feed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(feed["version"], "0.3.0");
        assert_eq!(feed["format"], 1);
        assert_eq!(feed["targets"]["windows-x64"]["file"], "nodecraft-0.3.0-windows-x64.msi");
        assert_eq!(feed["targets"]["windows-x64"]["size"], 3);
        assert_eq!(feed["targets"]["windows-x64"]["blake3"], blake3::hash(b"msi").to_hex().as_str());
        assert!(build(dir.path(), "9.9.9").is_err());
    }
}
