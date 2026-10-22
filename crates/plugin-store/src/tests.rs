use crate::archive::{self, ArchiveLimits};
use crate::backend::MemoryBackend;
use crate::store::{State, Store};
use nodecraft_plugin::id::{HostId, PackageId, Token};
use nodecraft_plugin::json::{self, Json};
use nodecraft_plugin::signature::{KeyStatus, Provenance, TrustStore, TrustedKey};
use nodecraft_plugin::{Inventory, PackageDigest};
use nodecraft_types::ErrorKind;
use std::collections::BTreeSet;
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;

const DESCRIPTOR: &[u8] = br#"{"descriptor_version":1}"#;
const LICENSE: &[u8] = b"MIT License (fixture)\n";

struct Spec<'a> {
    id: &'a str,
    version: &'a str,
    deps: Vec<(&'a str, &'a str, PackageDigest)>,
    regs: Vec<(&'a str, Option<&'a str>)>,
    publisher: Option<&'a str>,
}

fn spec(id: &str) -> Spec<'_> {
    Spec { id, version: "1.0.0", deps: vec![], regs: vec![("warm", None)], publisher: None }
}

fn s(t: &str) -> Json {
    Json::Str(t.to_owned())
}

fn obj(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn manifest(spec: &Spec<'_>) -> String {
    let mut fields = vec![
        ("manifest_version", Json::Int(1)),
        ("package_id", s(spec.id)),
        ("version", s(spec.version)),
        ("host_protocol", obj(vec![("major", Json::Int(1)), ("min_minor", Json::Int(0)), ("max_minor", Json::Int(0))])),
        ("descriptor_contract", obj(vec![("major", Json::Int(1)), ("minor", Json::Int(0))])),
        ("license", s("MIT")),
        ("runtime", obj(vec![("kind", s("data"))])),
        (
            "dependencies",
            Json::Array(spec.deps.iter().map(|(id, v, d)| obj(vec![("package_id", s(id)), ("version", s(v)), ("digest", s(&d.to_string()))])).collect()),
        ),
        ("capabilities", obj(vec![("required", Json::Array(vec![])), ("optional", Json::Array(vec![]))])),
        (
            "registrations",
            Json::Array(
                spec.regs
                    .iter()
                    .map(|(id, binding)| {
                        let mut r = vec![("id", s(id)), ("kind", s("catalog")), ("descriptor", s(&format!("d/{id}.json")))];
                        if let Some(b) = binding {
                            r.push(("binding", s(b)));
                        }
                        obj(r)
                    })
                    .collect(),
            ),
        ),
    ];
    if let Some(p) = spec.publisher {
        fields.push(("publisher_key_id", s(p)));
    }
    json::canonical(&obj(fields)).unwrap()
}

/// The package's files: manifest, license and one descriptor per registration.
fn files(spec: &Spec<'_>) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![("manifest.json".to_owned(), manifest(spec).into_bytes()), ("LICENSE".to_owned(), LICENSE.to_vec())];
    for (id, _) in &spec.regs {
        out.push((format!("d/{id}.json"), DESCRIPTOR.to_vec()));
    }
    out
}

fn package(spec: &Spec<'_>) -> Vec<u8> {
    let f = files(spec);
    archive::build(&f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>(), None).unwrap()
}

fn digest_of(spec: &Spec<'_>) -> PackageDigest {
    let f = files(spec);
    Inventory::from_files(f.iter().map(|(n, b)| (n.as_str(), b.as_slice()))).unwrap().digest()
}

/// Writes entries as given (names, bytes) with the inventory of `listed`: for adversaries.
fn raw(entries: &[(&str, &[u8])], listed: &[(&str, &[u8])]) -> Vec<u8> {
    let inventory = Inventory::from_files(listed.iter().copied()).unwrap().canonical_json();
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in entries.iter().copied().chain([("inventory.json", inventory.as_bytes())]) {
        out.start_file(name, options).unwrap();
        out.write_all(bytes).unwrap();
    }
    out.finish().unwrap().into_inner()
}

fn store() -> Store<MemoryBackend> {
    Store::open(MemoryBackend::default()).unwrap()
}

fn id(text: &str) -> PackageId {
    PackageId::parse(text).unwrap()
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

trait EnableUnsigned {
    /// Enable with an empty trust file (the packages here are unsigned).
    fn enable_with(
        &mut self,
        package: &PackageId,
        taken: &BTreeSet<String>,
        bindings: &BTreeSet<HostId>,
    ) -> Result<crate::EnableReport, nodecraft_types::Error>;
}
impl<B: crate::StoreBackend> EnableUnsigned for Store<B> {
    fn enable_with(
        &mut self,
        package: &PackageId,
        taken: &BTreeSet<String>,
        bindings: &BTreeSet<HostId>,
    ) -> Result<crate::EnableReport, nodecraft_types::Error> {
        self.enable(package, &TrustStore::default(), taken, bindings)
    }
}

/// The store reopened over a copy of its backend (removing files its index no longer names).
fn reopen(st: &Store<MemoryBackend>) -> Store<MemoryBackend> {
    Store::open(MemoryBackend {
        index: st.backend().index.clone(),
        objects: st.backend().objects.clone(),
        grants: st.backend().grants.clone(),
        ..MemoryBackend::default()
    })
    .unwrap()
}

#[test]
fn a_package_installs_disabled_and_survives_reopening() {
    let mut st = store();
    let bytes = package(&spec("io.example.palette"));
    let dry = st.import(&bytes, &TrustStore::default(), true).unwrap();
    assert!(st.index().active.is_empty() && st.backend().objects.is_empty() && st.backend().index.is_none(), "a dry run writes nothing");
    let report = st.import(&bytes, &TrustStore::default(), false).unwrap();
    assert_eq!(report, dry);
    assert_eq!(report.provenance, Provenance::Unsigned);
    assert_eq!(report.notices, ["LICENSE"]);
    assert_eq!(report.registrations[0].as_str(), "plugin.io.example.palette.warm");
    assert_eq!(st.index().active.get(&id("io.example.palette")), Some(&(report.entry.digest, State::Disabled)));
    assert_eq!(st.import(&bytes, &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Conflict, "already installed");
    let reopened = Store::open(MemoryBackend {
        index: st.backend().index.clone(),
        objects: st.backend().objects.clone(),
        grants: st.backend().grants.clone(),
        ..MemoryBackend::default()
    })
    .unwrap();
    assert_eq!(reopened.index(), st.index());
    assert_eq!(reopened.load(&id("io.example.palette")).unwrap().digest, report.entry.digest);
}

#[test]
fn hostile_archives_are_refused_before_anything_is_kept() {
    let good = files(&spec("io.example.palette"));
    let listed: Vec<(&str, &[u8])> = good.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let refuse = |bytes: Vec<u8>| {
        let mut st = store();
        let error = st.import(&bytes, &TrustStore::default(), false).unwrap_err();
        assert!(st.index().active.is_empty() && st.backend().objects.is_empty(), "nothing kept");
        error
    };
    // Traversal, absolute and backslashed names.
    for bad in ["../x", "/x", "a\\b", "C:/x", "a/./b"] {
        let mut entries = listed.clone();
        entries.push((bad, b"x"));
        refuse(raw(&entries, &listed));
    }
    // An unlisted file, and a listed one missing.
    let mut extra = listed.clone();
    extra.push(("extra.bin", b"x"));
    refuse(raw(&extra, &listed));
    refuse(raw(&listed[1..], &listed));
    // Names equal after case folding.
    let mut twins = listed.clone();
    twins.push(("license", LICENSE));
    refuse(raw(&twins, &listed));
    // A file whose bytes don't match the inventory.
    let mut tampered: Vec<(&str, &[u8])> = listed.clone();
    tampered[1] = ("LICENSE", b"GPL\n");
    let e = refuse(raw(&tampered, &listed));
    assert!(e.message.contains("bad_digest"), "{}", e.message);
    // No inventory; not a ZIP; a link.
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    out.start_file("manifest.json", SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
    refuse(out.finish().unwrap().into_inner());
    refuse(b"not a zip".to_vec());
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    out.add_symlink("LICENSE", "/etc/passwd", SimpleFileOptions::default()).unwrap();
    refuse(out.finish().unwrap().into_inner());
    // No license file.
    let unlicensed: Vec<(&str, &[u8])> = listed.iter().copied().filter(|(n, _)| *n != "LICENSE").collect();
    let e = refuse(archive::build(&unlicensed, None).unwrap());
    assert!(e.message.contains("LICENSE"));
    // A descriptor that isn't JSON.
    let mut broken = listed.clone();
    broken[2] = ("d/warm.json", b"{oops");
    refuse(archive::build(&broken, None).unwrap());
}

#[test]
fn archive_limits_hold_before_reading_entries() {
    let bytes = package(&spec("io.example.palette"));
    let small = ArchiveLimits { archive_bytes: bytes.len() - 1, ..ArchiveLimits::DEFAULT };
    assert!(archive::open(&bytes, small).unwrap_err().message.contains("larger"));
    let few = ArchiveLimits { entries: 2, ..ArchiveLimits::DEFAULT };
    assert!(archive::open(&bytes, few).unwrap_err().message.contains("too many entries"));
    let tight = ArchiveLimits { expanded_bytes: 10, ..ArchiveLimits::DEFAULT };
    assert!(archive::open(&bytes, tight).unwrap_err().message.contains("expands"));
    assert!(archive::open(&bytes, ArchiveLimits::DEFAULT).is_ok());
}

#[test]
fn executable_packages_wait_for_a_later_phase() {
    let mut f = files(&spec("io.example.exec"));
    let text = String::from_utf8(f[0].1.clone()).unwrap().replace(r#"{"kind":"data"}"#, r#"{"kind":"rust_wasm","module":"g.wasm"}"#);
    f[0].1 = text.into_bytes();
    f.push(("g.wasm".into(), b"\0asm\x01\0\0\0".to_vec()));
    let bytes = archive::build(&f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>(), None).unwrap();
    assert_eq!(store().import(&bytes, &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Unsupported);
}

fn random_seed() -> [u8; 32] {
    use std::hash::BuildHasher;
    let state = std::collections::hash_map::RandomState::new();
    let mut seed = [0u8; 32];
    for (i, chunk) in seed.chunks_mut(8).enumerate() {
        chunk.copy_from_slice(&state.hash_one(i).to_le_bytes());
    }
    seed
}

#[test]
fn signatures_are_checked_at_import_against_the_named_publisher() {
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(&random_seed());
    let mut sp = spec("io.example.signed");
    sp.publisher = Some("pub-key");
    let f = files(&sp);
    let listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let digest = digest_of(&sp);
    let sig = key.sign(&nodecraft_plugin::signature::signed_message(&digest));
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let signature =
        format!(r#"{{"algorithm":"ed25519","key_id":"pub-key","package_digest":"{digest}","signature":"{}","signature_version":1}}"#, hex(&sig.to_bytes()));
    let bytes = archive::build(&listed, Some(signature.as_bytes())).unwrap();
    let mut trust = TrustStore::default();
    trust.keys.insert(
        Token::parse("pub-key").unwrap(),
        TrustedKey { public_key: key.verifying_key().to_bytes(), provenance: "test".into(), status: KeyStatus::Trusted },
    );
    let report = store().import(&bytes, &trust, false).unwrap();
    assert_eq!(report.entry.signed_by.as_deref(), Some("pub-key"));
    assert!(matches!(store().import(&bytes, &TrustStore::default(), false).unwrap().provenance, Provenance::UnknownKey { .. }));
    trust.keys.values_mut().for_each(|k| k.status = KeyStatus::Revoked);
    assert!(store().import(&bytes, &trust, false).is_err(), "a revoked key's package isn't installed");
}

#[test]
fn enabling_is_all_or_nothing_and_follows_exact_locks() {
    let mut st = store();
    let base = spec("io.example.base");
    let base_digest = digest_of(&base);
    st.import(&package(&base), &TrustStore::default(), false).unwrap();
    let mut app = spec("io.example.app");
    app.deps = vec![("io.example.base", "1.0.0", base_digest)];
    app.regs = vec![("one", None), ("two", Some("color.palette")), ("three", Some("unknown.algo"))];
    st.import(&package(&app), &TrustStore::default(), false).unwrap();
    let bindings = BTreeSet::from([HostId::parse("color.palette").unwrap()]);
    // Its dependency is installed but off.
    assert!(st.enable_with(&id("io.example.app"), &none(), &bindings).unwrap_err().message.contains("enable io.example.base first"));
    st.enable_with(&id("io.example.base"), &none(), &bindings).unwrap();
    // A host ID already taken refuses the whole package.
    let taken = BTreeSet::from(["plugin.io.example.app.two".to_owned()]);
    assert_eq!(st.enable_with(&id("io.example.app"), &taken, &bindings).unwrap_err().kind, ErrorKind::Conflict);
    assert_eq!(st.index().active[&id("io.example.app")].1, State::Disabled);
    let report = st.enable_with(&id("io.example.app"), &none(), &bindings).unwrap();
    assert_eq!(report.registered.iter().map(|r| r.as_str()).collect::<Vec<_>>(), ["plugin.io.example.app.one", "plugin.io.example.app.two"]);
    assert_eq!(report.unbound.iter().map(|r| r.as_str()).collect::<Vec<_>>(), ["plugin.io.example.app.three"], "unknown bindings stay off");
    // The dependency can't be disabled under it.
    assert!(st.disable(&id("io.example.base")).unwrap_err().message.contains("io.example.app"));
    st.disable(&id("io.example.app")).unwrap();
    st.disable(&id("io.example.base")).unwrap();
}

#[test]
fn updates_keep_the_old_digest_for_rollback_and_uninstall_keeps_what_projects_use() {
    let mut st = store();
    let v1 = spec("io.example.palette");
    let mut v2 = spec("io.example.palette");
    v2.version = "1.1.0";
    let (d1, d2) = (digest_of(&v1), digest_of(&v2));
    assert_eq!(st.update(&package(&v2), &TrustStore::default(), false).unwrap_err().kind, ErrorKind::NotFound);
    st.import(&package(&v1), &TrustStore::default(), false).unwrap();
    st.enable_with(&id("io.example.palette"), &none(), &BTreeSet::new()).unwrap();
    assert!(st.update(&package(&v2), &TrustStore::default(), false).unwrap_err().message.contains("disable"));
    st.disable(&id("io.example.palette")).unwrap();
    let report = st.update(&package(&v2), &TrustStore::default(), false).unwrap();
    assert_eq!(report.replaced, Some(d1));
    assert_eq!(st.index().active[&id("io.example.palette")], (d2, State::Disabled), "an update comes in disabled");
    assert!(st.index().objects.contains_key(&d1), "the old digest is kept");
    st.rollback(&id("io.example.palette"), &d1).unwrap();
    assert_eq!(st.index().active[&id("io.example.palette")].0, d1);
    // Projects still reference v2: it stays; v1 goes.
    let report = st.uninstall(&id("io.example.palette"), &BTreeSet::from([d2])).unwrap();
    assert_eq!((report.removed, report.kept), (vec![d1], vec![d2]));
    assert!(st.index().active.is_empty());
    assert!(st.backend().objects.contains_key(&d1), "files go only when the store is next opened");
    let st = reopen(&st);
    assert!(st.index().objects.contains_key(&d2) && st.backend().objects.contains_key(&d2));
    assert!(!st.backend().objects.contains_key(&d1));
}

#[test]
fn a_failed_write_leaves_the_store_as_it_was() {
    let mut st = store();
    let bytes = package(&spec("io.example.palette"));
    st.import(&bytes, &TrustStore::default(), false).unwrap();
    let before = (st.index().clone(), st.backend().index.clone());
    let mut st = Store::open(MemoryBackend {
        index: st.backend().index.clone(),
        objects: st.backend().objects.clone(),
        fail_next_write: true,
        ..MemoryBackend::default()
    })
    .unwrap();
    assert!(st.enable_with(&id("io.example.palette"), &none(), &BTreeSet::new()).is_err());
    assert_eq!((st.index().clone(), st.backend().index.clone()), before, "still disabled, on disk too");
    st.enable_with(&id("io.example.palette"), &none(), &BTreeSet::new()).unwrap();
    let mut st2 = Store::open(MemoryBackend { fail_next_write: true, ..MemoryBackend::default() }).unwrap();
    assert!(st2.import(&bytes, &TrustStore::default(), false).is_err());
    assert!(st2.index().active.is_empty() && st2.backend().index.is_none());
}

#[test]
fn a_damaged_or_newer_index_is_an_error_not_an_empty_store() {
    for (bytes, kind) in [(&b"{oops"[..], ErrorKind::InvalidInput), (br#"{"active":{},"index_version":2,"objects":[]}"#, ErrorKind::Unsupported), (br#"{"active":{"io.example.x":{"digest":"blake3:0000000000000000000000000000000000000000000000000000000000000000","state":"enabled"}},"index_version":1,"objects":[]}"#, ErrorKind::InvalidInput)] {
        let error = Store::open(MemoryBackend { index: Some(bytes.to_vec()), ..MemoryBackend::default() }).err().unwrap();
        assert_eq!(error.kind, kind, "{}", String::from_utf8_lossy(bytes));
    }
}

#[test]
fn the_folder_backend_keeps_packages_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = package(&spec("io.example.palette"));
    {
        let mut st = Store::open(crate::FolderBackend::new(dir.path().to_path_buf()).unwrap()).unwrap();
        st.import(&bytes, &TrustStore::default(), false).unwrap();
        st.enable_with(&id("io.example.palette"), &none(), &BTreeSet::new()).unwrap();
    }
    let st = Store::open(crate::FolderBackend::new(dir.path().to_path_buf()).unwrap()).unwrap();
    assert_eq!(st.index().active[&id("io.example.palette")].1, State::Enabled);
    assert_eq!(st.load(&id("io.example.palette")).unwrap().manifest.package_id.as_str(), "io.example.palette");
    // A stored archive altered on disk is caught when it is loaded.
    let objects = dir.path().join("objects");
    let file = std::fs::read_dir(&objects).unwrap().next().unwrap().unwrap().path();
    let mut altered = std::fs::read(&file).unwrap();
    let at = altered.windows(LICENSE.len()).position(|w| w == LICENSE).unwrap();
    altered[at] ^= 0x20;
    std::fs::write(&file, altered).unwrap();
    assert!(st.load(&id("io.example.palette")).is_err());
}

proptest::proptest! {
    #[test]
    fn any_bytes_are_refused_without_panicking(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..2048)) {
        let _ = archive::open(&bytes, ArchiveLimits::DEFAULT);
    }

    #[test]
    fn a_damaged_package_never_panics(at in 0usize..4096, byte in proptest::prelude::any::<u8>()) {
        let mut bytes = package(&spec("io.example.palette"));
        let i = at % bytes.len();
        bytes[i] = byte;
        let _ = archive::open(&bytes, ArchiveLimits::DEFAULT);
    }
}

/// A valid package's bytes with `edit` applied, for structural adversaries.
fn patched(edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut bytes = package(&spec("io.example.palette"));
    edit(&mut bytes);
    bytes
}

fn find(bytes: &[u8], needle: &[u8], nth: usize) -> usize {
    bytes.windows(needle.len()).enumerate().filter(|(_, w)| *w == needle).nth(nth).map(|(i, _)| i).unwrap()
}

#[test]
fn only_one_reading_of_the_zip_structure_is_accepted() {
    let refuse = |bytes: Vec<u8>, why: &str| {
        let error = archive::open(&bytes, ArchiveLimits::DEFAULT).err().unwrap_or_else(|| panic!("{why}: opened"));
        assert_eq!(error.kind, ErrorKind::InvalidInput, "{why}");
    };
    // Two entries with one name: the zip crate would keep only the last, so the layout check
    // refuses it first.
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in ["aaaa", "bbbb"] {
        out.start_file(name, SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        out.write_all(b"x").unwrap();
    }
    let mut twins = out.finish().unwrap().into_inner();
    // In the local record, then the central one.
    for _ in 0..2 {
        let at = find(&twins, b"bbbb", 0);
        twins[at..at + 4].copy_from_slice(b"aaaa");
    }
    assert!(crate::layout::check(&twins, 10).err().unwrap().message.contains("same name"));
    refuse(twins, "duplicate names");
    // Junk before the first entry.
    refuse(patched(|b| b.splice(0..0, [0u8; 64]).for_each(drop)), "prepended bytes");
    // An archive comment.
    refuse(
        patched(|b| {
            let n = b.len();
            b[n - 2] = 3;
            b.extend_from_slice(b"hey");
        }),
        "a comment",
    );
    // A data descriptor flag, in both headers.
    refuse(
        patched(|b| {
            let local = find(b, &[0x50, 0x4b, 0x03, 0x04], 0);
            let central = find(b, &[0x50, 0x4b, 0x01, 0x02], 0);
            b[local + 6] |= 0x08;
            b[central + 8] |= 0x08;
        }),
        "a data descriptor",
    );
    // ZIP64 markers in the end record.
    refuse(
        patched(|b| {
            let n = b.len();
            b[n - 12..n - 10].copy_from_slice(&[0xff, 0xff]);
            b[n - 14..n - 12].copy_from_slice(&[0xff, 0xff]);
        }),
        "zip64",
    );
    // A central name that differs from its local name.
    refuse(
        patched(|b| {
            let central = find(b, &[0x50, 0x4b, 0x01, 0x02], 0);
            b[central + 46] ^= 0x01;
        }),
        "local and central names differ",
    );
    // A directory entry, and a symlink's mode.
    let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
    out.add_directory("d/", SimpleFileOptions::default()).unwrap();
    refuse(out.finish().unwrap().into_inner(), "a directory");
    // A huge entry count is refused from the end record, before any entry is read.
    let bytes = package(&spec("io.example.palette"));
    let few = ArchiveLimits { entries: 3, ..ArchiveLimits::DEFAULT };
    assert!(archive::open(&bytes, few).unwrap_err().message.contains("too many entries"));
}

#[test]
fn a_kept_digest_is_never_overwritten_by_a_different_archive() {
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(&random_seed());
    let mut sp = spec("io.example.signed");
    sp.publisher = Some("pub-key");
    let f = files(&sp);
    let listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let digest = digest_of(&sp);
    let sig = key.sign(&nodecraft_plugin::signature::signed_message(&digest));
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let signature =
        format!(r#"{{"algorithm":"ed25519","key_id":"pub-key","package_digest":"{digest}","signature":"{}","signature_version":1}}"#, hex(&sig.to_bytes()));
    let signed = archive::build(&listed, Some(signature.as_bytes())).unwrap();
    let mut trust = TrustStore::default();
    trust.keys.insert(
        Token::parse("pub-key").unwrap(),
        TrustedKey { public_key: key.verifying_key().to_bytes(), provenance: "test".into(), status: KeyStatus::Trusted },
    );
    let mut st = store();
    st.import(&signed, &trust, false).unwrap();
    st.uninstall(&id("io.example.signed"), &BTreeSet::from([digest])).unwrap();
    // The same digest, unsigned and padded: the kept (signed) archive stays and is what's reported.
    let unsigned = archive::build(&listed, None).unwrap();
    let report = st.import(&unsigned, &trust, false).unwrap();
    assert_eq!(report.entry.signed_by.as_deref(), Some("pub-key"));
    assert_eq!(st.backend().objects[&digest], signed);
    // A key revoked after import stops the package being enabled.
    trust.keys.values_mut().for_each(|k| k.status = KeyStatus::Revoked);
    assert!(st.enable(&id("io.example.signed"), &trust, &none(), &BTreeSet::new()).is_err());
    assert_eq!(st.index().active[&id("io.example.signed")].1, State::Disabled);
}

#[test]
fn installed_packages_never_share_an_id_whichever_is_enabled_first() {
    let mut st = store();
    let mut short = spec("io.example.text");
    short.regs = vec![("pack.x", None)];
    let mut long = spec("io.example.text.pack");
    long.regs = vec![("x", None)];
    st.import(&package(&short), &TrustStore::default(), false).unwrap();
    assert_eq!(st.import(&package(&long), &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Conflict, "refused at import, not at enable");
}

#[test]
fn uninstall_keeps_what_installed_packages_lock_and_prune_drops_old_versions() {
    let mut st = store();
    let base = spec("io.example.base");
    let base_digest = digest_of(&base);
    st.import(&package(&base), &TrustStore::default(), false).unwrap();
    let mut app = spec("io.example.app");
    app.deps = vec![("io.example.base", "1.0.0", base_digest)];
    st.import(&package(&app), &TrustStore::default(), false).unwrap();
    let report = st.uninstall(&id("io.example.base"), &BTreeSet::new()).unwrap();
    assert_eq!(report.kept, [base_digest], "the app still locks it");
    assert_eq!(report.dependents, [id("io.example.app")]);
    // Prune drops old versions nobody needs.
    let mut v2 = spec("io.example.app");
    v2.version = "2.0.0";
    st.update(&package(&v2), &TrustStore::default(), false).unwrap();
    let app_v1 = digest_of(&app);
    let dropped = st.prune(&BTreeSet::new()).unwrap();
    assert!(dropped.contains(&app_v1), "the old app version");
    assert!(!dropped.contains(&base_digest) || !st.index().objects.contains_key(&app_v1));
    let st = reopen(&st);
    assert!(!st.backend().objects.contains_key(&app_v1));
}

#[test]
fn a_second_writer_is_noticed_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let open = || Store::open(crate::FolderBackend::new(dir.path().to_path_buf()).unwrap()).unwrap();
    let (mut a, mut b) = (open(), open());
    a.import(&package(&spec("io.example.one")), &TrustStore::default(), false).unwrap();
    let error = b.import(&package(&spec("io.example.two")), &TrustStore::default(), false).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert!(b.index().active.contains_key(&id("io.example.one")), "b took a's state");
    b.import(&package(&spec("io.example.two")), &TrustStore::default(), false).unwrap();
    let c = open();
    assert_eq!(c.index().active.len(), 2, "nothing lost");
}

#[test]
fn a_failed_object_write_changes_nothing_and_leaves_nothing() {
    struct Full(MemoryBackend);
    impl crate::StoreBackend for Full {
        fn read_index(&self) -> Result<Option<Vec<u8>>, nodecraft_types::Error> {
            self.0.read_index()
        }
        fn write_index(&mut self, bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
            self.0.write_index(bytes)
        }
        fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, nodecraft_types::Error> {
            self.0.read_object(digest)
        }
        fn write_object(&mut self, _: &PackageDigest, _: &[u8]) -> Result<(), nodecraft_types::Error> {
            Err(nodecraft_types::Error::new(ErrorKind::Internal, "disk full"))
        }
        fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), nodecraft_types::Error> {
            self.0.remove_object(digest)
        }
        fn list_objects(&self) -> Result<Vec<PackageDigest>, nodecraft_types::Error> {
            self.0.list_objects()
        }
    }
    let mut st = Store::open(Full(MemoryBackend::default())).unwrap();
    assert!(st.import(&package(&spec("io.example.palette")), &TrustStore::default(), false).is_err());
    assert!(st.index().active.is_empty() && st.backend().0.index.is_none() && st.backend().0.objects.is_empty());
}

proptest::proptest! {
    #[test]
    fn the_index_reads_back_as_written(versions in proptest::collection::vec((0u32..50, 0u32..50), 1..6)) {
        let mut st = store();
        for (i, (minor, patch)) in versions.iter().enumerate() {
            let name = format!("io.example.p{i}");
            let version = format!("1.{minor}.{patch}");
            let mut sp = spec(&name);
            sp.version = &version;
            st.import(&package(&sp), &TrustStore::default(), false).unwrap();
        }
        let again = reopen(&st);
        proptest::prop_assert_eq!(again.index(), st.index());
    }
}

#[test]
fn an_import_in_flight_elsewhere_isnt_swept_and_refresh_sees_other_windows() {
    let dir = tempfile::tempdir().unwrap();
    let open = || Store::open(crate::FolderBackend::new(dir.path().to_path_buf()).unwrap()).unwrap();
    let mut a = open();
    // Another process has written its archive but not yet its index.
    let mut backend = crate::FolderBackend::new(dir.path().to_path_buf()).unwrap();
    let digest = digest_of(&spec("io.example.inflight"));
    crate::StoreBackend::write_object(&mut backend, &digest, &package(&spec("io.example.inflight"))).unwrap();
    let _b = open();
    assert!(crate::StoreBackend::read_object(&backend, &digest).is_ok(), "a fresh archive isn't swept");
    // A change made by another store is seen at the next request.
    open().import(&package(&spec("io.example.other")), &TrustStore::default(), false).unwrap();
    assert!(a.index().active.is_empty());
    a.refresh().unwrap();
    assert!(a.index().active.contains_key(&id("io.example.other")));
}

mod grant_tests;
#[cfg(not(target_arch = "wasm32"))]
mod lock_tests;
