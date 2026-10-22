//! Text action packages and their grants (#173).
use super::*;
use crate::grants;

const DEFINITION: &[u8] = br#"{"id":"user.shout","inputs":[{"kind":"text","name":"words"}],"label":"Shout","outputs":[{"expr":"upper(words)","kind":"text","name":"out"}],"schema":1,"version":1}"#;

/// A text action package registering `shout`, at `version`.
fn text_files(id: &str, version: &str) -> Vec<(String, Vec<u8>)> {
    let manifest = manifest(&Spec { version, regs: vec![("shout", None)], ..spec(id) })
        .replace(r#"{"kind":"data"}"#, r#"{"kind":"text_actions","schema":1}"#)
        .replace(r#""kind":"catalog""#, r#""kind":"action""#);
    vec![("manifest.json".to_owned(), manifest.into_bytes()), ("LICENSE".to_owned(), LICENSE.to_vec()), ("d/shout.json".to_owned(), DEFINITION.to_vec())]
}

fn text_package(id: &str, version: &str) -> Vec<u8> {
    let f = text_files(id, version);
    archive::build(&f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>(), None).unwrap()
}

fn installed() -> (Store<MemoryBackend>, PackageId) {
    let mut st = store();
    st.import(&text_package("io.example.text", "1.0.0"), &TrustStore::default(), false).unwrap();
    (st, id("io.example.text"))
}

#[test]
fn a_text_action_package_installs_but_needs_a_grant_to_enable() {
    let (mut st, pkg) = installed();
    let refused = st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Forbidden);
    assert_eq!(st.index().active[&pkg].1, State::Disabled);
    let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
    assert_eq!((grant.grant_id.as_str(), grant.epoch, grant.revoked), ("g1", 1, false));
    assert_eq!(grant.digest, st.index().active[&pkg].0);
    // Granting again gives the same grant.
    assert_eq!(st.grant(&pkg, &TrustStore::default()).unwrap(), grant);
    let report = st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap();
    assert_eq!(report.grant, Some(grant));
    assert_eq!(report.registered[0].as_str(), "plugin.io.example.text.shout");
}

#[test]
fn a_data_package_needs_no_grant() {
    let mut st = store();
    st.import(&package(&spec("io.example.data")), &TrustStore::default(), false).unwrap();
    assert_eq!(st.grant(&id("io.example.data"), &TrustStore::default()).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(st.enable_with(&id("io.example.data"), &none(), &BTreeSet::new()).unwrap().grant, None);
    assert_eq!(st.grant(&id("io.example.none"), &TrustStore::default()).unwrap_err().kind, ErrorKind::NotFound);
}

#[test]
fn revoking_disables_and_epochs_only_go_up() {
    let (mut st, pkg) = installed();
    let first = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap();
    let revoked = st.revoke(&first.grant_id).unwrap();
    assert!(revoked.disabled && revoked.grant.revoked);
    assert_eq!(st.index().active[&pkg].1, State::Disabled);
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    // Revoking again changes nothing; an unknown grant is not found.
    assert!(!st.revoke(&first.grant_id).unwrap().disabled);
    assert_eq!(st.revoke("g99").unwrap_err().kind, ErrorKind::NotFound);
    let second = st.grant(&pkg, &TrustStore::default()).unwrap();
    assert!(second.epoch > first.epoch);
    // The tombstone stays.
    assert!(st.grants().unwrap().get(&first.grant_id).unwrap().revoked);
}

#[test]
fn an_update_or_rollback_never_carries_a_grant_over() {
    let (mut st, pkg) = installed();
    let old = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.update(&text_package("io.example.text", "1.1.0"), &TrustStore::default(), false).unwrap();
    assert!(st.grants().unwrap().get(&old.grant_id).unwrap().revoked);
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    let new = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.rollback(&pkg, &old.digest).unwrap();
    assert!(st.grants().unwrap().get(&new.grant_id).unwrap().revoked);
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    // A dry run revokes nothing.
    let again = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.update(&text_package("io.example.text", "1.1.0"), &TrustStore::default(), true).unwrap();
    assert!(!st.grants().unwrap().get(&again.grant_id).unwrap().revoked);
}

#[test]
fn uninstalling_revokes() {
    let (mut st, pkg) = installed();
    let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.uninstall(&pkg, &BTreeSet::new()).unwrap();
    assert!(st.grants().unwrap().get(&grant.grant_id).unwrap().revoked);
}

#[test]
fn a_failed_grant_write_grants_nothing() {
    let (mut st, pkg) = installed();
    let first = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.revoke(&first.grant_id).unwrap();
    let before = st.backend().grants.clone();
    let mut st = Store::open(MemoryBackend { fail_next_grants_write: true, ..reopened_backend(&st) }).unwrap();
    assert_eq!(st.grant(&pkg, &TrustStore::default()).unwrap_err().kind, ErrorKind::Internal);
    assert_eq!(st.backend().grants, before);
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    // The epoch a failed write would have used isn't reused: the next is still above the last.
    assert!(st.grant(&pkg, &TrustStore::default()).unwrap().epoch > first.epoch);
}

fn reopened_backend(st: &Store<MemoryBackend>) -> MemoryBackend {
    MemoryBackend { index: st.backend().index.clone(), objects: st.backend().objects.clone(), grants: st.backend().grants.clone(), ..MemoryBackend::default() }
}

#[test]
fn grants_survive_reopening_and_damage_fails_closed() {
    let (mut st, pkg) = installed();
    let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
    let mut reopened = Store::open(reopened_backend(&st)).unwrap();
    assert_eq!(reopened.grants().unwrap().live(&pkg, &grant.digest), Some(&grant));
    reopened.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap();
    let bytes = st.backend().grants.clone().unwrap();
    let text = String::from_utf8(bytes).unwrap();
    for damaged in [
        text.replace(r#""scope":"local""#, r#""scope":"everywhere""#),
        text.replace(r#""capabilities":[]"#, r#""capabilities":["network"]"#),
        text.replace(r#""grant_id":"g1""#, r#""grant_id":"g7""#),
        text.replace(r#""next_epoch":"2""#, r#""next_epoch":"1""#),
        text.replace(r#""grants_version":1"#, r#""grants_version":0"#),
        "{".to_owned(),
    ] {
        assert_ne!(damaged, text);
        let mut st = Store::open(MemoryBackend { grants: Some(damaged.into_bytes()), ..reopened_backend(&st) }).unwrap();
        assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::InvalidInput);
        // A damaged grants file blocks nothing else: data packages and uninstalling still work.
        st.import(&package(&spec("io.example.data")), &TrustStore::default(), false).unwrap();
        st.enable_with(&id("io.example.data"), &none(), &BTreeSet::new()).unwrap();
        // Granting again replaces the damaged file and lets the package run.
        let again = st.grant(&pkg, &TrustStore::default()).unwrap();
        assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap().grant, Some(again));
        st.disable(&pkg).unwrap();
        st.uninstall(&pkg, &BTreeSet::new()).unwrap();
    }
    // Grants from a newer build may hold rights this one can't see: nothing runs, and nothing
    // that would have to revoke them changes.
    let newer = text.replace(r#""grants_version":1"#, r#""grants_version":2"#);
    let mut st = Store::open(MemoryBackend { grants: Some(newer.into_bytes()), ..reopened_backend(&st) }).unwrap();
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(st.grant(&pkg, &TrustStore::default()).unwrap_err().kind, ErrorKind::Unsupported);
    let before = st.index().clone();
    assert_eq!(st.update(&text_package("io.example.text", "1.1.0"), &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(st.uninstall(&pkg, &BTreeSet::new()).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(st.index(), &before);
}

#[test]
fn a_backend_without_grants_runs_nothing() {
    struct NoGrants(MemoryBackend);
    impl crate::StoreBackend for NoGrants {
        fn read_index(&self) -> Result<Option<Vec<u8>>, nodecraft_types::Error> {
            self.0.read_index()
        }
        fn write_index(&mut self, bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
            self.0.write_index(bytes)
        }
        fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, nodecraft_types::Error> {
            self.0.read_object(digest)
        }
        fn write_object(&mut self, digest: &PackageDigest, bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
            self.0.write_object(digest, bytes)
        }
        fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), nodecraft_types::Error> {
            self.0.remove_object(digest)
        }
        fn list_objects(&self) -> Result<Vec<PackageDigest>, nodecraft_types::Error> {
            self.0.list_objects()
        }
    }
    let mut st = Store::open(NoGrants(MemoryBackend::default())).unwrap();
    st.import(&text_package("io.example.text", "1.0.0"), &TrustStore::default(), false).unwrap();
    let pkg = id("io.example.text");
    assert_eq!(st.grant(&pkg, &TrustStore::default()).unwrap_err().kind, ErrorKind::Unsupported);
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
}

#[test]
fn tombstones_are_bounded() {
    let pkg = id("io.example.text");
    let digest = digest_of(&spec("io.example.text"));
    let mut g = grants::Grants::default();
    for _ in 0..grants::MAX_GRANTS + 10 {
        let grant = g.grant(&pkg, &digest).unwrap();
        g.revoke_package(&pkg);
        assert!(grant.epoch >= 1);
    }
    assert_eq!(g.grants.len(), grants::MAX_GRANTS);
    let bytes = grants::encode(&g).unwrap();
    assert_eq!(grants::decode(bytes.as_bytes()).unwrap(), g);
    // Live grants are never dropped to make room.
    let mut full = grants::Grants::default();
    for i in 0..grants::MAX_GRANTS {
        full.grant(&id(&format!("io.example.p{i}")), &digest).unwrap();
    }
    assert_eq!(full.grant(&pkg, &digest).unwrap_err().kind, ErrorKind::Budget);
}

#[test]
fn a_failed_update_keeps_its_revocation() {
    let (st, pkg) = installed();
    let mut st = Store::open(MemoryBackend { fail_next_write: true, ..reopened_backend(&st) }).unwrap();
    let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
    let before = st.index().clone();
    assert_eq!(st.update(&text_package("io.example.text", "1.1.0"), &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Internal);
    // The package and index are as they were; the grant stays revoked, and its epoch isn't reused.
    assert_eq!(st.index(), &before);
    assert!(st.grants().unwrap().get(&grant.grant_id).unwrap().revoked);
    assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    assert!(st.grant(&pkg, &TrustStore::default()).unwrap().epoch > grant.epoch);
}

#[test]
fn a_failed_rollback_or_uninstall_keeps_its_revocation() {
    let (mut st, pkg) = installed();
    let first = st.index().active[&pkg].0;
    st.update(&text_package("io.example.text", "1.1.0"), &TrustStore::default(), false).unwrap();
    for undo in [true, false] {
        let mut st = Store::open(MemoryBackend { fail_next_write: true, ..reopened_backend(&st) }).unwrap();
        let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
        let before = st.index().clone();
        let failed = if undo { st.rollback(&pkg, &first) } else { st.uninstall(&pkg, &BTreeSet::new()).map(|_| ()) };
        assert_eq!(failed.unwrap_err().kind, ErrorKind::Internal);
        assert_eq!(st.index(), &before);
        assert!(st.grants().unwrap().get(&grant.grant_id).unwrap().revoked);
        assert_eq!(st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap_err().kind, ErrorKind::Forbidden);
    }
}

#[test]
fn a_failed_revoke_leaves_the_package_and_its_grant_as_they_were() {
    let (st, pkg) = installed();
    let mut st = Store::open(reopened_backend(&st)).unwrap();
    let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
    st.enable_with(&pkg, &none(), &BTreeSet::new()).unwrap();
    let mut st = Store::open(MemoryBackend { fail_next_write: true, ..reopened_backend(&st) }).unwrap();
    assert_eq!(st.revoke(&grant.grant_id).unwrap_err().kind, ErrorKind::Internal);
    assert_eq!(st.index().active[&pkg].1, State::Enabled);
    assert_eq!(st.grants().unwrap().live(&pkg, &grant.digest), Some(&grant));
    // A failed grants write after the package is disabled leaves it disabled, never enabled
    // without its grant.
    let mut st = Store::open(MemoryBackend { fail_next_grants_write: true, ..reopened_backend(&st) }).unwrap();
    assert_eq!(st.revoke(&grant.grant_id).unwrap_err().kind, ErrorKind::Internal);
    assert_eq!(st.index().active[&pkg].1, State::Disabled);
    assert!(st.revoke(&grant.grant_id).unwrap().grant.revoked);
}

#[test]
fn revoking_disables_the_packages_depending_on_it() {
    let (mut st, pkg) = installed();
    let f = text_files("io.example.text", "1.0.0");
    let digest = Inventory::from_files(f.iter().map(|(n, b)| (n.as_str(), b.as_slice()))).unwrap().digest();
    let mut app = spec("io.example.app");
    app.deps = vec![("io.example.text", "1.0.0", digest)];
    st.import(&package(&app), &TrustStore::default(), false).unwrap();
    let mut top = spec("io.example.top");
    top.regs = vec![("cold", None)];
    top.deps = vec![("io.example.app", "1.0.0", digest_of(&app))];
    st.import(&package(&top), &TrustStore::default(), false).unwrap();
    let grant = st.grant(&pkg, &TrustStore::default()).unwrap();
    for p in [&pkg, &id("io.example.app"), &id("io.example.top")] {
        st.enable_with(p, &none(), &BTreeSet::new()).unwrap();
    }
    let report = st.revoke(&grant.grant_id).unwrap();
    assert!(report.disabled);
    assert_eq!(report.dependents.len(), 2);
    assert!(st.index().active.values().all(|(_, s)| *s == State::Disabled));
}

#[test]
fn granting_checks_the_signature_again() {
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(&random_seed());
    let manifest = manifest(&Spec { regs: vec![("shout", None)], publisher: Some("pub-key"), ..spec("io.example.text") })
        .replace(r#"{"kind":"data"}"#, r#"{"kind":"text_actions","schema":1}"#)
        .replace(r#""kind":"catalog""#, r#""kind":"action""#);
    let f = [("manifest.json", manifest.as_bytes()), ("LICENSE", LICENSE), ("d/shout.json", DEFINITION)];
    let digest = Inventory::from_files(f.iter().copied()).unwrap().digest();
    let sig = key.sign(&nodecraft_plugin::signature::signed_message(&digest));
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let signature =
        format!(r#"{{"algorithm":"ed25519","key_id":"pub-key","package_digest":"{digest}","signature":"{}","signature_version":1}}"#, hex(&sig.to_bytes()));
    let mut trust = TrustStore::default();
    trust.keys.insert(
        Token::parse("pub-key").unwrap(),
        TrustedKey { public_key: key.verifying_key().to_bytes(), provenance: "test".into(), status: KeyStatus::Trusted },
    );
    let mut st = store();
    st.import(&archive::build(&f, Some(signature.as_bytes())).unwrap(), &trust, false).unwrap();
    // A key revoked after import stops the package being granted.
    trust.keys.values_mut().for_each(|k| k.status = KeyStatus::Revoked);
    assert!(st.grant(&id("io.example.text"), &trust).is_err());
    assert!(st.grants().unwrap().grants.is_empty());
}
