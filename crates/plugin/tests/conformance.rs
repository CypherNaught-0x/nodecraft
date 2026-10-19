//! Runs the fixtures in `crates/plugin/fixtures` (see its README): every valid file decodes,
//! every invalid one fails with the code its name starts with, and the sample package has the
//! recorded digest and a signature that verifies.
use nodecraft_plugin::frame::{self, FrameDecoder};
use nodecraft_plugin::inventory::Inventory;
use nodecraft_plugin::signature::{self, KeyStatus, PackageSignature, Provenance, TrustStore};
use nodecraft_plugin::{Code, Manifest, Message, PackageDigest, json};
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn files(dir: &str) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = std::fs::read_dir(fixtures().join(dir))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())
        })
        .collect();
    out.sort();
    assert!(!out.is_empty(), "{dir} has fixtures");
    out
}

/// The code an invalid fixture expects, from its name: `<code>--<what>.json`.
fn expected(name: &str) -> Code {
    let code = name.split("--").next().unwrap();
    Code::ALL.into_iter().find(|c| c.as_str() == code).unwrap_or_else(|| panic!("{name}: unknown code {code}"))
}

#[test]
fn manifests() {
    for (name, bytes) in files("manifest/valid") {
        Manifest::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    for (name, bytes) in files("manifest/invalid") {
        let error = Manifest::decode(&bytes).err().unwrap_or_else(|| panic!("{name} decoded"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
}

#[test]
fn messages() {
    for (name, bytes) in files("frame/valid") {
        let message = Message::decode(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        // Re-encoding gives the same bytes: fixtures are canonical, so are frames.
        let framed = frame::encode(&message).unwrap();
        assert_eq!(framed.get(4..).unwrap(), bytes.as_slice(), "{name}");
        assert_eq!(framed.get(..4).unwrap(), (bytes.len() as u32).to_be_bytes(), "{name}");
    }
    for (name, bytes) in files("frame/invalid") {
        let error = Message::decode(&bytes).err().unwrap_or_else(|| panic!("{name} decoded"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
}

struct Package {
    files: Vec<(String, Vec<u8>)>,
    inventory: Vec<u8>,
    signature: Vec<u8>,
}

fn package() -> Package {
    let root = fixtures().join("package/textpack");
    let mut files = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                files.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    let take = |files: &mut Vec<(String, Vec<u8>)>, name: &str| {
        // Absent only while the fixture is being regenerated.
        files.iter().position(|(n, _)| n == name).map(|i| files.remove(i).1).unwrap_or_default()
    };
    let inventory = take(&mut files, "inventory.json");
    let signature = take(&mut files, "signature.json");
    Package { files, inventory, signature }
}

fn recorded_digest() -> PackageDigest {
    let expected = std::fs::read(fixtures().join("package/expected.json")).unwrap();
    let json::Json::Object(map) = json::parse(&expected, json::Limits::WIRE).unwrap() else { panic!() };
    let Some(json::Json::Str(digest)) = map.get("digest") else { panic!() };
    PackageDigest::parse(digest).unwrap()
}

fn trust() -> TrustStore {
    TrustStore::decode(&std::fs::read(fixtures().join("package/trust.json")).unwrap()).unwrap()
}

#[test]
fn the_sample_package_has_its_recorded_digest_and_verifies() {
    let p = package();
    let built = Inventory::from_files(p.files.iter().map(|(n, b)| (n.as_str(), b.as_slice()))).unwrap();
    let listed = Inventory::decode(&p.inventory).unwrap();
    assert_eq!(built, listed);
    assert_eq!(listed.canonical_json().as_bytes(), p.inventory.as_slice(), "inventory.json is canonical");
    assert_eq!(listed.digest(), recorded_digest());
    listed.check_listing(p.files.iter().map(|(n, _)| n.as_str()).chain(["inventory.json", "signature.json"])).unwrap();
    for (name, bytes) in &p.files {
        listed.verify(name, bytes).unwrap();
    }
    let manifest = Manifest::decode(&p.files.iter().find(|(n, _)| n == "manifest.json").unwrap().1).unwrap();
    assert_eq!(manifest.registered_ids().next().unwrap().as_str(), "plugin.io.example.textpack.normalize");
    let sig = PackageSignature::decode(&p.signature).unwrap();
    assert_eq!(Some(&sig.key_id), manifest.publisher_key_id.as_ref());
    let digest = listed.digest();
    assert_eq!(signature::verify(&digest, Some(&sig), &trust()).unwrap(), Provenance::Verified { key_id: sig.key_id.clone() });
    assert_eq!(signature::verify(&digest, None, &trust()).unwrap(), Provenance::Unsigned);
    assert!(matches!(signature::verify(&digest, Some(&sig), &TrustStore::default()).unwrap(), Provenance::UnknownKey { .. }));
    // Revoked, tampered, or for another package: refused, never downgraded.
    let mut revoked = trust();
    revoked.keys.values_mut().for_each(|k| k.status = KeyStatus::Revoked);
    assert_eq!(signature::verify(&digest, Some(&sig), &revoked).unwrap_err().code, Code::BadSignature);
    let mut tampered = sig.clone();
    tampered.signature[0] ^= 1;
    assert_eq!(signature::verify(&digest, Some(&tampered), &trust()).unwrap_err().code, Code::BadSignature);
    let other = PackageDigest([7; 32]);
    let mut moved = sig.clone();
    moved.package_digest = other;
    assert_eq!(signature::verify(&other, Some(&moved), &trust()).unwrap_err().code, Code::BadSignature);
    assert_eq!(signature::verify(&other, Some(&sig), &trust()).unwrap_err().code, Code::BadDigest);
}

#[test]
fn any_changed_byte_changes_the_digest_or_fails_verification() {
    let p = package();
    let listed = Inventory::decode(&p.inventory).unwrap();
    for (name, bytes) in &p.files {
        for i in [0, bytes.len() / 2, bytes.len() - 1] {
            let mut changed = bytes.clone();
            changed[i] ^= 0x20;
            assert_eq!(listed.verify(name, &changed).unwrap_err().code, Code::BadDigest, "{name}[{i}]");
            let files = p.files.iter().map(|(n, b)| (n.as_str(), if n == name { changed.as_slice() } else { b.as_slice() }));
            assert_ne!(Inventory::from_files(files).unwrap().digest(), listed.digest(), "{name}[{i}]");
        }
    }
    // A file added or removed is caught by the listing check.
    let names: Vec<&str> = p.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(listed.check_listing(names.iter().copied().chain(["extra.bin"])).unwrap_err().code, Code::BadPath);
    assert_eq!(listed.check_listing(names.iter().copied().skip(1)).unwrap_err().code, Code::MissingField);
}

#[test]
fn inventories_refuse_ambiguous_listings() {
    let m: &[u8] = b"{}";
    let code = |files: &[(&str, &[u8])]| Inventory::from_files(files.iter().copied()).unwrap_err().code;
    assert_eq!(code(&[("manifest.json", m), ("Data/a", m), ("data/A", m)]), Code::Collision);
    assert_eq!(code(&[("manifest.json", m), ("inventory.json", m)]), Code::BadPath);
    assert_eq!(code(&[("manifest.json", m), ("signature.json", m)]), Code::BadPath);
    assert_eq!(code(&[("data/a", m)]), Code::MissingField);
    for bad in ["a//b", "./a", "a/..", "a/", "a\\b", "c:/a", "a b", "a.", "NUL.txt", "a/b/c/d/e/f/g/h/i", "é"] {
        assert_eq!(code(&[("manifest.json", m), (bad, m)]), Code::BadPath, "{bad}");
    }
    let unsorted = br#"{"files":[{"blake3":"0000000000000000000000000000000000000000000000000000000000000000","bytes":"1","path":"manifest.json"},{"blake3":"0000000000000000000000000000000000000000000000000000000000000000","bytes":"1","path":"data"}],"inventory_version":1}"#;
    assert_eq!(Inventory::decode(unsorted).unwrap_err().code, Code::NotCanonical);
    let huge = br#"{"files":[{"blake3":"0000000000000000000000000000000000000000000000000000000000000000","bytes":"134217729","path":"manifest.json"}],"inventory_version":1}"#;
    assert_eq!(Inventory::decode(huge).unwrap_err().code, Code::TooLarge);
}

#[test]
fn frames_check_lengths_before_buffering() {
    let message = Message::decode(&std::fs::read(fixtures().join("frame/valid/accept.json")).unwrap()).unwrap();
    let framed = frame::encode(&message).unwrap();
    // Delivered one byte at a time, twice over.
    let mut decoder = FrameDecoder::default();
    let mut got = Vec::new();
    for byte in framed.iter().chain(framed.iter()) {
        decoder.push(std::slice::from_ref(byte)).unwrap();
        while let Some(payload) = decoder.next_frame().unwrap() {
            got.push(Message::decode(&payload).unwrap());
        }
    }
    assert_eq!(got, vec![message.clone(), message]);
    // A length over 1 MiB is refused at the header, and the stream stays refused.
    let mut decoder = FrameDecoder::default();
    assert_eq!(decoder.push(&(frame::MAX_FRAME_BYTES as u32 + 1).to_be_bytes()).unwrap_err().code, Code::TooLarge);
    assert!(decoder.push(&framed).is_err());
    assert!(decoder.next_frame().is_err());
    let mut decoder = FrameDecoder::default();
    assert_eq!(decoder.push(&[0, 0, 0, 0]).unwrap_err().code, Code::TooLarge);
    // A frame held back while more arrives can't grow the buffer past one frame.
    let mut decoder = FrameDecoder::default();
    decoder.push(&(frame::MAX_FRAME_BYTES as u32).to_be_bytes()).unwrap();
    decoder.push(&vec![b' '; frame::MAX_FRAME_BYTES]).unwrap();
    assert_eq!(decoder.push(b"x").unwrap_err().code, Code::TooLarge);
}

/// Regenerates `package/textpack/{inventory,signature}.json`, `package/trust.json` and
/// `package/expected.json` from the package's files. The signing seed comes from the
/// environment (32 bytes as hex) and is never written down:
/// `NODECRAFT_FIXTURE_SEED=$(openssl rand -hex 32) cargo test -p nodecraft-plugin --test conformance -- --ignored`
#[test]
#[ignore = "regenerates fixtures"]
fn regenerate_package_fixture() {
    use ed25519_dalek::{Signer, SigningKey};
    let seed: [u8; 32] = json::parse(format!("\"{}\"", std::env::var("NODECRAFT_FIXTURE_SEED").unwrap()).as_bytes(), json::Limits::WIRE)
        .ok()
        .and_then(|v| if let json::Json::Str(s) = v { Some(s) } else { None })
        .and_then(|s| (0..32).map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()).collect::<Option<Vec<u8>>>())
        .unwrap()
        .try_into()
        .unwrap();
    let key = SigningKey::from_bytes(&seed);
    let p = package();
    let inventory = Inventory::from_files(p.files.iter().map(|(n, b)| (n.as_str(), b.as_slice()))).unwrap();
    let digest = inventory.digest();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let sig = key.sign(&signature::signed_message(&digest));
    let root = fixtures().join("package");
    std::fs::write(root.join("textpack/inventory.json"), inventory.canonical_json()).unwrap();
    std::fs::write(
        root.join("textpack/signature.json"),
        format!(
            r#"{{"algorithm":"ed25519","key_id":"example-test-key","package_digest":"{digest}","signature":"{}","signature_version":1}}"#,
            hex(&sig.to_bytes())
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("trust.json"),
        format!(
            r#"{{"keys":[{{"algorithm":"ed25519","key_id":"example-test-key","provenance":"Generated for the NodeCraft plugin fixtures; the private key was discarded.","public_key":"{}","status":"trusted"}}],"trust_version":1}}"#,
            hex(key.verifying_key().as_bytes())
        ),
    )
    .unwrap();
    std::fs::write(root.join("expected.json"), format!(r#"{{"digest":"{digest}"}}"#)).unwrap();
}
