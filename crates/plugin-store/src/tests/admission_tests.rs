//! What this host admits at all (#172): the versions and features it speaks and NodeCraft's own
//! namespaces; and a real disk failing under the folder backend.
use super::*;
use ed25519_dalek::Signer;

/// `spec`'s package with its manifest text edited by `from` → `to` (the manifest stays canonical:
/// only values change).
fn edited(spec: &Spec<'_>, from: &str, to: &str) -> Vec<u8> {
    let mut f = files(spec);
    let text = String::from_utf8(f[0].1.clone()).unwrap();
    assert!(text.contains(from), "{from} isn't in {text}");
    f[0].1 = text.replace(from, to).into_bytes();
    archive::build(&f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>(), None).unwrap()
}

const PROTOCOL: &str = r#""host_protocol":{"major":1,"max_minor":0,"min_minor":0}"#;
const CONTRACT: &str = r#""descriptor_contract":{"major":1,"minor":0}"#;

#[test]
fn a_package_made_for_another_protocol_or_contract_is_refused_even_in_a_dry_run() {
    let sp = spec("io.example.future");
    let refused = [
        (PROTOCOL, r#""host_protocol":{"major":2,"max_minor":0,"min_minor":0}"#),
        (PROTOCOL, r#""host_protocol":{"major":1,"max_minor":3,"min_minor":1}"#),
        (CONTRACT, r#""descriptor_contract":{"major":1,"minor":1}"#),
        (CONTRACT, r#""descriptor_contract":{"major":2,"minor":0}"#),
        (r#""host_protocol":"#, r#""features":{"optional":[],"required":["future.thing"]},"host_protocol":"#),
    ];
    for (from, to) in refused {
        let bytes = edited(&sp, from, to);
        for dry_run in [true, false] {
            let error = store().import(&bytes, &TrustStore::default(), dry_run).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Unsupported, "{to}: {}", error.message);
            assert!(error.message.contains("newer NodeCraft"), "{}", error.message);
        }
    }
    // An overlapping minor range and an unknown optional feature are fine.
    let accepted = [
        (PROTOCOL, r#""host_protocol":{"major":1,"max_minor":4,"min_minor":0}"#),
        (r#""host_protocol":"#, r#""features":{"optional":["future.thing"],"required":[]},"host_protocol":"#),
    ];
    for (from, to) in accepted {
        let mut st = store();
        st.import(&edited(&sp, from, to), &TrustStore::default(), false).unwrap();
        st.enable_with(&id("io.example.future"), &none(), &BTreeSet::new()).unwrap();
    }
}

#[test]
fn a_package_an_older_index_kept_is_negotiated_again_at_enable() {
    // A newer build installed a package this one doesn't speak, into the shared store.
    let bytes = edited(&spec("io.example.future"), CONTRACT, r#""descriptor_contract":{"major":1,"minor":1}"#);
    let package = archive::open(&bytes, ArchiveLimits::DEFAULT).unwrap();
    let pkg = id("io.example.future");
    let entry = crate::Entry {
        package_id: pkg.clone(),
        version: package.manifest.version,
        digest: package.digest,
        bytes: bytes.len() as u64,
        license: "MIT".into(),
        signed_by: None,
        registrations: vec!["plugin.io.example.future.warm".into()],
        dependencies: vec![],
    };
    let mut index = crate::Index::default();
    index.objects.insert(package.digest, entry);
    index.active.insert(pkg.clone(), (package.digest, State::Disabled));
    let backend = MemoryBackend {
        index: Some(crate::store::encode_index(&index).unwrap().into_bytes()),
        objects: [(package.digest, bytes)].into_iter().collect(),
        ..MemoryBackend::default()
    };
    let mut st = Store::open(backend).unwrap();
    let error = st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Unsupported, "{}", error.message);
    assert_eq!(st.index().active[&pkg].1, State::Disabled);
}

/// `sp` signed by `key` as `key_id`, and a trust file trusting that key.
fn signed(sp: &mut Spec<'_>, key_id: &'static str) -> (Vec<u8>, TrustStore) {
    let key = ed25519_dalek::SigningKey::from_bytes(&random_seed());
    sp.publisher = Some(key_id);
    let f = files(sp);
    let listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let digest = digest_of(sp);
    let sig = key.sign(&nodecraft_plugin::signature::signed_message(&digest));
    let hex = sig.to_bytes().iter().map(|x| format!("{x:02x}")).collect::<String>();
    let signature = format!(r#"{{"algorithm":"ed25519","key_id":"{key_id}","package_digest":"{digest}","signature":"{hex}","signature_version":1}}"#);
    let bytes = archive::build(&listed, Some(signature.as_bytes())).unwrap();
    let mut trust = TrustStore::default();
    trust.keys.insert(
        Token::parse(key_id).unwrap(),
        TrustedKey { public_key: key.verifying_key().to_bytes(), provenance: "test".into(), status: KeyStatus::Trusted },
    );
    (bytes, trust)
}

#[test]
fn nodecraft_namespaces_need_a_trusted_signature() {
    for name in ["nodecraft.example.pack", "io.github.cyphernaught0x.pack", "io.github.cyphernaught0x.nodecraft.pack"] {
        let error = store().import(&package(&spec(name)), &TrustStore::default(), true).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Forbidden, "{name}: {}", error.message);
        assert!(error.message.contains("trusted key"), "{}", error.message);
    }
    // Lookalikes outside the namespaces install unsigned.
    for name in ["nodecraftx.example.pack", "io.github.cyphernaught0xx.pack", "io.example.nodecraft"] {
        store().import(&package(&spec(name)), &TrustStore::default(), false).unwrap();
    }
    let mut sp = spec("nodecraft.example.pack");
    let (bytes, mut trust) = signed(&mut sp, "nc-key");
    // Signed by a key the trust file doesn't list: still refused.
    assert_eq!(store().import(&bytes, &TrustStore::default(), true).unwrap_err().kind, ErrorKind::Forbidden);
    let mut st = store();
    st.import(&bytes, &trust, false).unwrap();
    // The key is no longer trusted (forgotten, not revoked): enabling is refused.
    let saved = trust.clone();
    trust.keys.clear();
    let pkg = id("nodecraft.example.pack");
    assert_eq!(st.enable(&pkg, &trust, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    st.enable(&pkg, &saved, &none(), &BTreeSet::new()).unwrap();
}

/// The digest doesn't cover the signature, so when it is already kept the kept archive's
/// signature is the one checked: an unsigned copy of a kept signed package is the same package.
#[test]
fn the_namespace_is_checked_against_the_kept_archives_signature() {
    let mut sp = spec("nodecraft.example.kept");
    let (bytes, trust) = signed(&mut sp, "nc-key");
    let mut st = store();
    st.import(&bytes, &trust, false).unwrap();
    let error = st.import(&package(&sp), &trust, true).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict, "{}", error.message);
}

/// A real failed write under the folder backend: the folder made read-only. Skipped where the
/// permission doesn't hold (running as root).
#[cfg(unix)]
#[test]
fn a_real_failed_write_leaves_the_folder_store_as_it_was() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let mut st = Store::open(crate::FolderBackend::new(root.clone()).unwrap()).unwrap();
    st.import(&package(&spec("io.example.first")), &TrustStore::default(), false).unwrap();
    let index = std::fs::read(root.join("index.json")).unwrap();
    /// Makes the folder writable again however the test ends, so the temp dir can be removed.
    struct Writable(std::path::PathBuf);
    impl Drop for Writable {
        fn drop(&mut self) {
            for path in [self.0.join("objects"), self.0.clone()] {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
            }
        }
    }
    let _writable = Writable(root.clone());
    let set = |path: &std::path::Path, mode: u32| std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    let probe = |path: &std::path::Path| std::fs::write(path.join("probe"), b"").is_ok();
    // The archive can't be written: nothing changes.
    set(&root.join("objects"), 0o555);
    if probe(&root.join("objects")) {
        let _ = std::fs::remove_file(root.join("objects/probe"));
        eprintln!("skipped: permissions don't hold here");
        return;
    }
    assert!(st.import(&package(&spec("io.example.second")), &TrustStore::default(), false).is_err());
    set(&root.join("objects"), 0o755);
    assert_eq!(std::fs::read(root.join("index.json")).unwrap(), index);
    assert!(!st.index().active.contains_key(&id("io.example.second")));
    // The archive is written but the index can't be: still nothing changes, and the store
    // reopens to what it was, with the first package whole.
    set(&root, 0o555);
    let error = st.import(&package(&spec("io.example.second")), &TrustStore::default(), false).unwrap_err();
    set(&root, 0o755);
    assert_eq!(error.kind, ErrorKind::Forbidden, "{}", error.message);
    assert!(error.message.contains("permissions"), "{}", error.message);
    assert_eq!(std::fs::read(root.join("index.json")).unwrap(), index);
    assert!(!st.index().active.contains_key(&id("io.example.second")));
    let mut st = Store::open(crate::FolderBackend::new(root.clone()).unwrap()).unwrap();
    assert_eq!(st.index().active.keys().map(|k| k.as_str()).collect::<Vec<_>>(), ["io.example.first"]);
    st.load(&id("io.example.first")).unwrap();
    // And the store works again once the disk does.
    st.import(&package(&spec("io.example.second")), &TrustStore::default(), false).unwrap();
}
