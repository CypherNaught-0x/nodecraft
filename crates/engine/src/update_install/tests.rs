use super::*;
use crate::update_check::Release;
use ed25519_dalek::{Signer, SigningKey};
use std::collections::HashMap;
use std::sync::Mutex;

const SEED: [u8; 32] = [7; 32];

fn signing() -> SigningKey {
    SigningKey::from_bytes(&SEED)
}

fn keys() -> Vec<VerifyingKey> {
    vec![signing().verifying_key()]
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sign(key: &SigningKey, feed: &[u8]) -> Vec<u8> {
    let mut message = SIGNED_PREFIX.to_vec();
    message.extend_from_slice(feed);
    to_hex(&key.sign(&message).to_bytes()).into_bytes()
}

fn release(version: &str) -> Release {
    Release { version: version.into(), url: format!("https://github.com/CypherNaught-0x/NodeCraft/releases/tag/v{version}"), prerelease: false }
}

fn feed_for(version: &str, payload: &[u8]) -> Vec<u8> {
    feed_marked(version, payload, false)
}

fn feed_marked(version: &str, payload: &[u8], prerelease: bool) -> Vec<u8> {
    let targets = BTreeMap::from([(
        "test-target".to_owned(),
        Payload { file: "nodecraft-test.bin".into(), size: payload.len() as u64, blake3: blake3::hash(payload).to_hex().to_string() },
    )]);
    serde_json::to_vec(&Feed { format: FEED_FORMAT, version: version.into(), prerelease, targets }).unwrap()
}

/// Serves files by url, enforcing the request's size cap as the real client does.
#[derive(Default)]
struct Files(HashMap<String, Vec<u8>>, Mutex<Vec<HttpRequest>>);

impl Files {
    fn release(version: &str, payload: &[u8], key: &SigningKey) -> Self {
        Self::marked(version, payload, key, false)
    }

    fn marked(version: &str, payload: &[u8], key: &SigningKey, prerelease: bool) -> Self {
        let feed = feed_marked(version, payload, prerelease);
        let at = |file: &str| format!("https://feed.test/v{version}/{file}");
        let mut files = Self::default();
        files.0.insert(at(SIGNATURE_FILE), sign(key, &feed));
        files.0.insert(at(FEED_FILE), feed);
        files.0.insert(at("nodecraft-test.bin"), payload.to_vec());
        files
    }
}

impl Http for Files {
    fn send(&self, request: HttpRequest) -> nodecraft_providers::BoxFuture<'_, Result<nodecraft_providers::HttpResponse, nodecraft_providers::ProviderError>> {
        let reply = match self.0.get(&request.url) {
            Some(body) if body.len() > request.max_response_bytes => {
                Err(nodecraft_providers::ProviderError::new(nodecraft_providers::ErrorKind::Malformed, "the reply is larger than allowed"))
            }
            Some(body) => Ok(nodecraft_providers::HttpResponse { status: 200, headers: Vec::new(), body: body.clone() }),
            None => Ok(nodecraft_providers::HttpResponse { status: 404, headers: Vec::new(), body: Vec::new() }),
        };
        if let Ok(mut sent) = self.1.lock() {
            sent.push(request);
        }
        Box::pin(async move { reply })
    }
}

fn fetch(files: &Files, version: &str, keys: &[VerifyingKey]) -> Result<Downloaded, Error> {
    futures::executor::block_on(download(files, "https://feed.test/", &release(version), "0.1.0", "test-target", keys))
}

#[test]
fn a_signed_update_downloads_and_verifies() {
    let files = Files::release("0.2.0", b"new app", &signing());
    let downloaded = fetch(&files, "0.2.0", &keys()).unwrap();
    let blake3 = blake3::hash(b"new app").to_hex().to_string();
    assert_eq!(downloaded, Downloaded { version: "0.2.0".into(), file: "nodecraft-test.bin".into(), blake3, bytes: b"new app".to_vec() });
    let sent = files.1.lock().unwrap();
    let urls: Vec<&str> = sent.iter().map(|request| request.url.as_str()).collect();
    assert_eq!(
        urls,
        ["https://feed.test/v0.2.0/nodecraft-update.json", "https://feed.test/v0.2.0/nodecraft-update.json.sig", "https://feed.test/v0.2.0/nodecraft-test.bin"]
    );
    // Nothing personal is sent, and the payload may be no larger than the feed says.
    assert!(sent.iter().all(|request| request.headers.is_empty() && request.body.is_empty() && request.secret_headers.is_empty()));
    assert_eq!(sent[2].max_response_bytes, 7);
}

#[test]
fn a_tampered_feed_signature_or_payload_is_refused() {
    let other = SigningKey::from_bytes(&[9; 32]);
    let files = Files::release("0.2.0", b"new app", &other);
    let refused = fetch(&files, "0.2.0", &keys()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);
    assert_eq!(refused.message, "the update for NodeCraft 0.2.0 was refused: its signature doesn't match NodeCraft's release key");

    // The feed edited after signing.
    let mut files = Files::release("0.2.0", b"new app", &signing());
    let feed = files.0.get_mut("https://feed.test/v0.2.0/nodecraft-update.json").unwrap();
    *feed = String::from_utf8(feed.clone()).unwrap().replace("nodecraft-test.bin", "nodecraft-evil.bin").into_bytes();
    assert_eq!(fetch(&files, "0.2.0", &keys()).unwrap_err().kind, ErrorKind::InvalidInput);

    // The payload swapped, same size and different, or truncated.
    for swapped in [b"evil ap".to_vec(), b"new ap".to_vec()] {
        let mut files = Files::release("0.2.0", b"new app", &signing());
        files.0.insert("https://feed.test/v0.2.0/nodecraft-test.bin".into(), swapped);
        let refused = fetch(&files, "0.2.0", &keys()).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::InvalidInput, "{}", refused.message);
        assert!(refused.message.starts_with("the downloaded update was refused"), "{}", refused.message);
    }
    // A longer payload is cut off by the download cap before it is hashed.
    let mut files = Files::release("0.2.0", b"new app", &signing());
    files.0.insert("https://feed.test/v0.2.0/nodecraft-test.bin".into(), b"new app and more".to_vec());
    assert_eq!(fetch(&files, "0.2.0", &keys()).unwrap_err().kind, ErrorKind::Network);

    // A damaged or missing signature file.
    for signature in [b"zz".to_vec(), Vec::new(), vec![b'a'; 128]] {
        let mut files = Files::release("0.2.0", b"new app", &signing());
        files.0.insert("https://feed.test/v0.2.0/nodecraft-update.json.sig".into(), signature);
        assert_eq!(fetch(&files, "0.2.0", &keys()).unwrap_err().kind, ErrorKind::InvalidInput);
    }
    let mut files = Files::release("0.2.0", b"new app", &signing());
    files.0.remove("https://feed.test/v0.2.0/nodecraft-update.json.sig");
    assert_eq!(fetch(&files, "0.2.0", &keys()).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn a_feed_signed_for_another_release_or_an_older_one_is_refused() {
    // 0.2.0's signed feed served under 0.3.0's tag.
    let old = Files::release("0.2.0", b"old app", &signing());
    let mut files = Files::default();
    for (url, body) in &old.0 {
        files.0.insert(url.replace("/v0.2.0/", "/v0.3.0/"), body.clone());
    }
    let refused = fetch(&files, "0.3.0", &keys()).unwrap_err();
    assert_eq!(refused.message, "the update for NodeCraft 0.3.0 was refused: its feed is for a different release");
    // A release that isn't newer is never fetched.
    let files = Files::release("0.1.0", b"same", &signing());
    assert_eq!(fetch(&files, "0.1.0", &keys()).unwrap_err().kind, ErrorKind::InvalidInput);
    assert!(files.1.lock().unwrap().is_empty());
}

#[test]
fn a_prerelease_offered_as_a_stable_release_is_refused() {
    let files = Files::marked("0.2.0", b"new app", &signing(), true);
    let refused = fetch(&files, "0.2.0", &keys()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::InvalidInput);
    assert!(refused.message.contains("its signed feed says it is a prerelease"), "{}", refused.message);
    // The payload is never fetched.
    assert_eq!(files.1.lock().unwrap().len(), 2);
    // Offered as the prerelease it is, it installs.
    let offered = Release { prerelease: true, ..release("0.2.0") };
    futures::executor::block_on(download(&files, "https://feed.test/", &offered, "0.1.0", "test-target", &keys())).unwrap();
    // A feed without the field is one this build can't read.
    let body = br#"{"format":1,"version":"0.2.0","targets":{}}"#;
    assert!(verify_feed(body, &sign(&signing(), body), &keys(), "0.2.0").is_err());
}

#[test]
fn a_staged_update_is_checked_again_before_it_is_used() {
    let hash = blake3::hash(b"new app").to_hex().to_string();
    verify_staged(&mut &b"new app"[..], &hash, 7).unwrap();
    verify_staged(&mut &b"new app"[..], &hash.to_uppercase(), MAX_PAYLOAD_BYTES).unwrap();
    for changed in [&b"evil ap"[..], b"new ap", b"new app!", b""] {
        let refused = verify_staged(&mut &changed[..], &hash, 7).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::InvalidInput);
        assert!(refused.message.contains("changed after it was checked"), "{}", refused.message);
    }
    // A staged file that grew is refused once it is past the signed size, without reading it all.
    let mut endless = std::io::repeat(0);
    assert!(verify_staged(&mut endless, &hash, 7).is_err());
}

#[test]
fn a_build_without_a_key_or_a_platform_payload_cant_install() {
    let files = Files::release("0.2.0", b"new app", &signing());
    let refused = fetch(&files, "0.2.0", &[]).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert!(files.1.lock().unwrap().is_empty());
    let refused = futures::executor::block_on(download(&files, "https://feed.test", &release("0.2.0"), "0.1.0", "windows-x64", &keys())).unwrap_err();
    assert_eq!(refused.message, "NodeCraft 0.2.0 has no update for this platform; download it from the releases page");
}

#[test]
fn hostile_feeds_are_refused_not_panicked_on() {
    let good = Payload { file: "nodecraft.bin".into(), size: 3, blake3: "a".repeat(64) };
    let names = ["", ".hidden", "../app", "a/b", "a\\b", "c:app", "app\u{0}", &"a".repeat(200), "naïve"];
    for file in names {
        let feed = Feed {
            format: 1,
            version: "0.2.0".into(),
            prerelease: false,
            targets: BTreeMap::from([("t".into(), Payload { file: file.into(), ..good.clone() })]),
        };
        assert_eq!(payload(&feed, "t").unwrap_err().kind, ErrorKind::InvalidInput, "{file:?}");
    }
    for size in [0, MAX_PAYLOAD_BYTES + 1, u64::MAX] {
        let feed = Feed { format: 1, version: "0.2.0".into(), prerelease: false, targets: BTreeMap::from([("t".into(), Payload { size, ..good.clone() })]) };
        assert!(payload(&feed, "t").is_err(), "{size}");
    }
    for hash in ["", "zz", &"a".repeat(63), &"é".repeat(32)] {
        let feed = Feed {
            format: 1,
            version: "0.2.0".into(),
            prerelease: false,
            targets: BTreeMap::from([("t".into(), Payload { blake3: hash.into(), ..good.clone() })]),
        };
        assert!(payload(&feed, "t").is_err(), "{hash:?}");
    }
    // Signed, but not a feed this build reads.
    for body in [
        b"not json".to_vec(),
        br#"{"format":2,"version":"0.2.0","prerelease":false,"targets":{}}"#.to_vec(),
        br#"{"format":1,"version":"0.2.0","prerelease":false,"targets":{},"extra":1}"#.to_vec(),
    ] {
        let refused = verify_feed(&body, &sign(&signing(), &body), &keys(), "0.2.0").unwrap_err();
        assert_eq!(refused.kind, ErrorKind::InvalidInput);
    }
    let many: BTreeMap<String, Payload> = (0..=MAX_TARGETS).map(|n| (format!("t{n}"), good.clone())).collect();
    let body = serde_json::to_vec(&Feed { format: 1, version: "0.2.0".into(), prerelease: false, targets: many }).unwrap();
    assert!(verify_feed(&body, &sign(&signing(), &body), &keys(), "0.2.0").is_err());
    // A signature over the bare feed, without the prefix, isn't a feed signature.
    let body = feed_for("0.2.0", b"x");
    let bare = to_hex(&signing().sign(&body).to_bytes()).into_bytes();
    assert!(verify_feed(&body, &bare, &keys(), "0.2.0").is_err());
    assert!(verify_feed(&body, &sign(&signing(), &body), &keys(), "0.2.0").is_ok());
}

#[test]
fn keys_parse_from_the_committed_file_format() {
    let public = to_hex(signing().verifying_key().as_bytes());
    let text = format!("# comment\n\n  {public}  \n{}\n", public.to_uppercase());
    assert_eq!(parse_keys(&text).unwrap().len(), 2);
    for bad in ["abc", "zz", &"0".repeat(64), &"a".repeat(66)] {
        assert!(parse_keys(bad).is_err(), "{bad:?}");
    }
    assert!(parse_keys(&format!("{public}\n").repeat(MAX_KEYS + 1)).is_err());
    // The file built into the app parses (an empty key list only notifies).
    parse_keys(BUILT_IN_KEYS).unwrap();
}

/// `cargo xtask update-feed` signs with the same scheme; its tests check this same signature.
#[test]
fn the_signature_scheme_matches_the_release_tooling() {
    assert_eq!(to_hex(signing().verifying_key().as_bytes()), "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c");
    assert_eq!(
        String::from_utf8(sign(&signing(), b"{}")).unwrap(),
        "d83f7739e03e935b511ef87ddb6cd096ba08b912074caf58d2ce97bc4657c9dfa08b9bcd10ff87cadd1d2a25c662bee95b78901a6e20f06ae039116c64678b06"
    );
}

#[test]
fn the_commands_go_to_the_host_and_status_reports_the_install() {
    let mut session = Session::new(nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([3; 16]), "P").unwrap()).unwrap();
    for id in ["app.install_update", "app.restart_to_update"] {
        assert_eq!(session.dispatch(id, &json!({})).unwrap_err().kind, ErrorKind::Unsupported);
        assert!(session.dispatch(id, &json!({"x": 1})).is_err());
    }
    session.register_host_service(INSTALL_SERVICE, |_| Ok(json!({"pending": "0.2.0"}))).unwrap();
    session.register_host_service(APPLY_SERVICE, |_| Err(Error::new(ErrorKind::Busy, "a generation is running"))).unwrap();
    assert_eq!(session.dispatch("app.install_update", &json!({})).unwrap(), json!({"pending": "0.2.0"}));
    assert_eq!(session.dispatch("app.restart_to_update", &json!({})).unwrap_err().kind, ErrorKind::Busy);

    record(&mut session, Some(Install::Failed { version: "0.2.0".into(), error: "offline".into() }));
    let status = session.dispatch("app.update_status", &json!({})).unwrap();
    assert_eq!(status["install"], json!({"state": "failed", "version": "0.2.0", "error": "offline"}));
    record(&mut session, None);
    assert_eq!(session.dispatch("app.update_status", &json!({})).unwrap()["install"], Value::Null);
}
