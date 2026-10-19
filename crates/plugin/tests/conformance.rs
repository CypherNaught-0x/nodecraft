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
    assert_eq!(
        signature::verify(&digest, Some(&sig), &trust(), manifest.publisher_key_id.as_ref()).unwrap(),
        Provenance::Verified { key_id: sig.key_id.clone() }
    );
    assert_eq!(signature::verify(&digest, None, &trust(), manifest.publisher_key_id.as_ref()).unwrap(), Provenance::Unsigned);
    assert!(matches!(signature::verify(&digest, Some(&sig), &TrustStore::default(), None).unwrap(), Provenance::UnknownKey { .. }));
    // Revoked, tampered, or for another package: refused, never downgraded.
    let mut revoked = trust();
    revoked.keys.values_mut().for_each(|k| k.status = KeyStatus::Revoked);
    assert_eq!(signature::verify(&digest, Some(&sig), &revoked, None).unwrap_err().code, Code::BadSignature);
    let mut tampered = sig.clone();
    tampered.signature[0] ^= 1;
    assert_eq!(signature::verify(&digest, Some(&tampered), &trust(), None).unwrap_err().code, Code::BadSignature);
    let other = PackageDigest([7; 32]);
    let mut moved = sig.clone();
    moved.package_digest = other;
    assert_eq!(signature::verify(&other, Some(&moved), &trust(), None).unwrap_err().code, Code::BadSignature);
    assert_eq!(signature::verify(&other, Some(&sig), &trust(), None).unwrap_err().code, Code::BadDigest);
    // Signed by a trusted key, but not the publisher the manifest names.
    let someone_else = nodecraft_plugin::id::Token::parse("another-publisher").unwrap();
    assert_eq!(signature::verify(&digest, Some(&sig), &trust(), Some(&someone_else)).unwrap_err().code, Code::BadSignature);
    // Every file the manifest names is listed, so all of it is under the digest.
    listed.check_manifest(&manifest).unwrap();
    let mut stray = manifest.clone();
    stray.registrations[0].descriptor = nodecraft_plugin::inventory::InventoryPath::parse("descriptors/other.json").unwrap();
    assert_eq!(listed.check_manifest(&stray).unwrap_err().code, Code::BadPath);
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
    // Frames left untaken while more arrives can't grow the buffer past two frames.
    let mut decoder = FrameDecoder::default();
    decoder.push(&(frame::MAX_FRAME_BYTES as u32).to_be_bytes()).unwrap();
    decoder.push(&vec![b' '; frame::MAX_FRAME_BYTES]).unwrap();
    decoder.push(&vec![b' '; frame::MAX_FRAME_BYTES + 4]).unwrap();
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

#[test]
fn a_read_that_completes_one_frame_and_starts_the_next_is_accepted() {
    // The decoder splits frames; it doesn't read payloads, so plain bytes will do.
    let raw = |len: usize| {
        let mut f = (len as u32).to_be_bytes().to_vec();
        f.resize(4 + len, b'x');
        f
    };
    let mut stream = Vec::new();
    for _ in 0..3 {
        stream.extend_from_slice(&raw(frame::MAX_FRAME_BYTES));
    }
    stream.extend_from_slice(&raw(1));
    let mut decoder = FrameDecoder::default();
    let mut frames = 0;
    // Each read holds the end of one full-size frame and the start of the next.
    for chunk in stream.chunks(frame::MAX_FRAME_BYTES + 11) {
        decoder.push(chunk).unwrap();
        while decoder.next_frame().unwrap().is_some() {
            frames += 1;
        }
    }
    assert_eq!(frames, 4);
}

#[test]
fn many_small_frames_are_taken_in_linear_time() {
    let one = frame::encode(&Message::Accept { job_id: nodecraft_plugin::id::Token::parse("j").unwrap() }).unwrap();
    let stream: Vec<u8> = one.iter().copied().cycle().take(frame::MAX_FRAME_BYTES / one.len() * one.len()).collect();
    let started = std::time::Instant::now();
    let mut decoder = FrameDecoder::default();
    decoder.push(&stream).unwrap();
    let mut frames = 0;
    while decoder.next_frame().unwrap().is_some() {
        frames += 1;
    }
    assert_eq!(frames, stream.len() / one.len());
    // Quadratic draining took seconds here; linear takes milliseconds, even unoptimised.
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
}

#[test]
fn json_profile() {
    for (name, bytes) in files("json/canonical") {
        let Some(stem) = name.strip_suffix(".json") else { continue };
        let expected = std::fs::read(fixtures().join("json/canonical").join(format!("{stem}.canonical"))).unwrap();
        let value = json::parse(&bytes, json::Limits::WIRE).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(json::canonical(&value).unwrap().as_bytes(), expected.as_slice(), "{name}");
    }
    for (name, bytes) in files("json/invalid") {
        let error = json::parse(&bytes, json::Limits::WIRE).err().unwrap_or_else(|| panic!("{name} parsed"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
}

#[test]
fn inventories_signatures_and_trust_files() {
    for (name, bytes) in files("inventory/invalid") {
        let error = Inventory::decode(&bytes).err().unwrap_or_else(|| panic!("{name} decoded"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
    for (name, bytes) in files("signature/invalid") {
        let error = PackageSignature::decode(&bytes).err().unwrap_or_else(|| panic!("{name} decoded"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
    for (name, bytes) in files("trust/invalid") {
        let error = TrustStore::decode(&bytes).err().unwrap_or_else(|| panic!("{name} decoded"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
}

/// Feeds a whole stream; the frames it held, or the first error.
fn split(bytes: &[u8]) -> Result<Vec<Vec<u8>>, nodecraft_plugin::WireError> {
    let mut decoder = FrameDecoder::default();
    decoder.push(bytes)?;
    let mut frames = Vec::new();
    while let Some(payload) = decoder.next_frame()? {
        frames.push(payload);
    }
    Ok(frames)
}

#[test]
fn streams() {
    for (name, bytes) in files("stream/valid") {
        let count: usize = name.split("--").next().unwrap().parse().unwrap();
        let frames = split(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(frames.len(), count, "{name}");
        for payload in frames {
            Message::decode(&payload).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
    for (name, bytes) in files("stream/invalid") {
        let error = split(&bytes).err().unwrap_or_else(|| panic!("{name} split"));
        assert_eq!(error.code, expected(&name), "{name}: {error}");
    }
}
