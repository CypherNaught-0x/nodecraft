//! Property tests (#824): structured hostile archives are refused with nothing kept, and random
//! operation sequences with injected write failures never leave a failed step half done.
use super::grant_tests::text_package;
use super::*;
use crate::grants::{self, Grant, Grants};
use crate::store::Index;
use proptest::prelude::*;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

// ---- Hostile archives ----

/// Paths no package may hold, each refused for its own reason: traversal, absolute, device
/// names, empty or dot segments, characters outside the portable set, reserved names, and names
/// colliding (exactly or after case folding) with the package's other files.
const HOSTILE_NAMES: &[&str] = &[
    "../evil",
    "a/../../b",
    "/etc/passwd",
    "./a",
    "a/./b",
    "a//b",
    "..",
    "d/",
    "CON",
    "con.txt",
    "NUL",
    "d/aux.json",
    "COM1",
    "lpt9.md",
    "a\\b",
    "C:x",
    "x.",
    "x ",
    "a\0b",
    "é",
    "inventory.json",
    "Signature.json",
    "manifest.json",
    "MANIFEST.JSON",
    "LICENSE",
    "license",
    "D/WARM.JSON",
];

/// One structured way an archive can be hostile.
#[derive(Clone, Debug)]
enum Hostile {
    /// A listed file renamed, in its headers and the inventory alike.
    Name(&'static str),
    /// A file that isn't listed.
    Unlisted(String),
    /// A listed file left out.
    Missing(usize),
    /// A listed file stored as a symbolic link to its own listed bytes, so only the entry type
    /// is wrong.
    Link,
    /// The first entry's declared sizes, in both headers, set to `size`.
    Declared(u32),
    /// One byte of the inventory's JSON changed.
    Inventory(usize, u8),
}

fn hostile() -> impl Strategy<Value = Hostile> {
    prop_oneof![
        proptest::sample::select(HOSTILE_NAMES).prop_map(Hostile::Name),
        "[a-z]{1,8}(/[a-z]{1,8}){0,2}".prop_map(Hostile::Unlisted),
        (0usize..8).prop_map(Hostile::Missing),
        Just(Hostile::Link),
        any::<u32>().prop_map(Hostile::Declared),
        (any::<usize>(), any::<u8>()).prop_map(|(at, byte)| Hostile::Inventory(at, byte)),
    ]
}

/// A valid package holding an extra file named by `len` placeholder letters, for renaming.
fn placeholder_files(len: usize) -> (String, Vec<(String, Vec<u8>)>) {
    let name = "z".repeat(len);
    let mut f = files(&spec("io.example.hostile"));
    f.push((name.clone(), b"x".to_vec()));
    (name, f)
}

fn build(f: &[(String, Vec<u8>)]) -> Vec<u8> {
    archive::build(&f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect::<Vec<_>>(), None).unwrap()
}

fn replace_all(bytes: &mut [u8], from: &[u8], to: &[u8]) {
    let mut at = 0;
    while let Some(i) = bytes.get(at..).and_then(|rest| rest.windows(from.len()).position(|w| w == from)) {
        bytes[at + i..at + i + to.len()].copy_from_slice(to);
        at += i + to.len();
    }
}

fn hostile_bytes(h: &Hostile) -> Vec<u8> {
    let stored = || SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    match h {
        Hostile::Name(name) => {
            let (placeholder, f) = placeholder_files(name.len().max(1));
            let mut bytes = build(&f);
            replace_all(&mut bytes, placeholder.as_bytes(), name.as_bytes());
            bytes
        }
        Hostile::Unlisted(extra) => {
            let f = files(&spec("io.example.hostile"));
            let listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
            let mut entries = listed.clone();
            entries.push((extra.as_str(), b"hidden"));
            raw(&entries, &listed)
        }
        Hostile::Missing(which) => {
            let f = files(&spec("io.example.hostile"));
            let listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
            let mut entries = listed.clone();
            entries.remove(which % entries.len());
            raw(&entries, &listed)
        }
        Hostile::Link => {
            let f = files(&spec("io.example.hostile"));
            let listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
            let inventory = Inventory::from_files(listed.iter().copied()).unwrap().canonical_json();
            let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (name, bytes) in &listed {
                if name.starts_with("d/") {
                    out.add_symlink(*name, std::str::from_utf8(bytes).unwrap(), stored()).unwrap();
                } else {
                    out.start_file(*name, stored()).unwrap();
                    out.write_all(bytes).unwrap();
                }
            }
            out.start_file("inventory.json", stored()).unwrap();
            out.write_all(inventory.as_bytes()).unwrap();
            out.finish().unwrap().into_inner()
        }
        Hostile::Declared(size) => {
            let mut bytes = package(&spec("io.example.hostile"));
            let actual = u32::from_le_bytes(bytes[22..26].try_into().unwrap());
            let size = if *size == actual { size.wrapping_add(1) } else { *size };
            let central = find(&bytes, &[0x50, 0x4b, 0x01, 0x02], 0);
            // Compressed and uncompressed sizes: local at 18 and 22, central at 20 and 24.
            for at in [18, 22, central + 20, central + 24] {
                bytes[at..at + 4].copy_from_slice(&size.to_le_bytes());
            }
            bytes
        }
        Hostile::Inventory(at, byte) => {
            let mut bytes = package(&spec("io.example.hostile"));
            let start = find(&bytes, br#"{"files":"#, 0);
            let len = bytes[start..].iter().position(|b| *b == 0x50).unwrap_or(1).max(1);
            let i = start + at % len;
            bytes[i] = if bytes[i] == *byte { byte.wrapping_add(1) } else { *byte };
            bytes
        }
    }
}

fn refused_with_nothing_kept(h: &Hostile) -> Result<(), TestCaseError> {
    let bytes = hostile_bytes(h);
    let mut st = store();
    let dry = st.import(&bytes, &TrustStore::default(), true).err();
    prop_assert_eq!(dry.map(|e| e.kind), Some(ErrorKind::InvalidInput), "{:?}: inspected", h);
    let error = st.import(&bytes, &TrustStore::default(), false).err();
    prop_assert_eq!(error.map(|e| e.kind), Some(ErrorKind::InvalidInput), "{:?}: imported", h);
    prop_assert!(st.index().active.is_empty() && st.index().objects.is_empty(), "{h:?}");
    prop_assert!(st.backend().index.is_none() && st.backend().objects.is_empty() && st.backend().grants.is_none(), "{h:?}");
    Ok(())
}

proptest! {
    #[test]
    fn a_structurally_hostile_archive_is_refused_with_nothing_kept(h in hostile()) {
        refused_with_nothing_kept(&h)?;
    }
}

/// Every hostile name, every run: sampling alone would skip one now and then.
#[test]
fn every_hostile_name_is_refused() {
    for name in HOSTILE_NAMES {
        refused_with_nothing_kept(&Hostile::Name(name)).unwrap();
    }
    refused_with_nothing_kept(&Hostile::Link).unwrap();
}

// ---- Operation sequences with failing writes ----

/// Which write fails, once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    Index,
    Object,
    Grants,
}

#[derive(Default, Debug)]
struct Faults {
    index: Cell<bool>,
    object: Cell<bool>,
    grants: Cell<bool>,
}

/// A memory backend whose next index, archive or grants write can be made to fail.
#[derive(Debug)]
struct Failing {
    inner: MemoryBackend,
    faults: Rc<Faults>,
}

fn full() -> nodecraft_types::Error {
    nodecraft_types::Error::new(ErrorKind::Internal, "disk full")
}

impl crate::StoreBackend for Failing {
    fn read_index(&self) -> Result<Option<Vec<u8>>, nodecraft_types::Error> {
        self.inner.read_index()
    }
    fn write_index(&mut self, bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
        if self.faults.index.take() {
            return Err(full());
        }
        self.inner.write_index(bytes)
    }
    fn read_object(&self, digest: &PackageDigest) -> Result<Vec<u8>, nodecraft_types::Error> {
        self.inner.read_object(digest)
    }
    fn write_object(&mut self, digest: &PackageDigest, bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
        if self.faults.object.take() {
            return Err(full());
        }
        self.inner.write_object(digest, bytes)
    }
    fn remove_object(&mut self, digest: &PackageDigest) -> Result<(), nodecraft_types::Error> {
        self.inner.remove_object(digest)
    }
    fn list_objects(&self) -> Result<Vec<PackageDigest>, nodecraft_types::Error> {
        self.inner.list_objects()
    }
    fn read_grants(&self) -> Result<Option<Vec<u8>>, nodecraft_types::Error> {
        self.inner.read_grants()
    }
    fn write_grants(&mut self, bytes: &[u8]) -> Result<(), nodecraft_types::Error> {
        if self.faults.grants.take() {
            return Err(full());
        }
        self.inner.write_grants(bytes)
    }
}

/// The packages a sequence works with: a text action package (it needs a grant to run), a data
/// package, and a data package locking the data package's first version.
const PACKAGES: [&str; 3] = ["io.example.text", "io.example.data", "io.example.needs"];

#[derive(Clone, Debug)]
enum Op {
    Import(usize, usize),
    Update(usize, usize),
    Enable(usize),
    Disable(usize),
    Rollback(usize, usize),
    Uninstall(usize),
    Prune,
    Grant(usize),
    Revoke(usize),
}

fn op() -> impl Strategy<Value = Op> {
    let p = 0usize..PACKAGES.len();
    let v = 0usize..2;
    prop_oneof![
        3 => (p.clone(), v.clone()).prop_map(|(p, v)| Op::Import(p, v)),
        2 => (p.clone(), v.clone()).prop_map(|(p, v)| Op::Update(p, v)),
        3 => p.clone().prop_map(Op::Enable),
        2 => p.clone().prop_map(Op::Disable),
        1 => (p.clone(), v).prop_map(|(p, v)| Op::Rollback(p, v)),
        1 => p.clone().prop_map(Op::Uninstall),
        1 => Just(Op::Prune),
        3 => p.prop_map(Op::Grant),
        2 => (0usize..4).prop_map(Op::Revoke),
    ]
}

fn fault() -> impl Strategy<Value = Fault> {
    prop_oneof![3 => Just(Fault::None), 1 => Just(Fault::Index), 1 => Just(Fault::Object), 1 => Just(Fault::Grants)]
}

/// Every package's archive at each of its two versions.
struct Packages {
    bytes: Vec<[Vec<u8>; 2]>,
    digests: Vec<[PackageDigest; 2]>,
}

fn packages() -> Packages {
    let data = |version: &str| {
        let mut sp = spec(PACKAGES[1]);
        sp.version = version;
        sp.regs = vec![("warm", None)];
        (package(&sp), digest_of(&sp))
    };
    let (d1, d1_digest) = data("1.0.0");
    let (d2, d2_digest) = data("2.0.0");
    let needs = |version: &str| {
        let mut sp = spec(PACKAGES[2]);
        sp.version = version;
        sp.regs = vec![("cool", None)];
        sp.deps = vec![(PACKAGES[1], "1.0.0", d1_digest)];
        (package(&sp), digest_of(&sp))
    };
    let (n1, n1_digest) = needs("1.0.0");
    let (n2, n2_digest) = needs("1.1.0");
    let text = |version: &str| {
        let bytes = text_package(PACKAGES[0], version);
        let digest = archive::open(&bytes, ArchiveLimits::DEFAULT).unwrap().digest;
        (bytes, digest)
    };
    let (t1, t1_digest) = text("1.0.0");
    let (t2, t2_digest) = text("1.1.0");
    Packages { bytes: vec![[t1, t2], [d1, d2], [n1, n2]], digests: vec![[t1_digest, t2_digest], [d1_digest, d2_digest], [n1_digest, n2_digest]] }
}

fn grants_of(backend: &MemoryBackend) -> Grants {
    backend.grants.as_deref().map(|b| grants::decode(b).unwrap()).unwrap_or_default()
}

/// What a step may change: the store's index, the archives it names, and the grants.
#[derive(Clone, Debug, PartialEq)]
struct Snapshot {
    index: Index,
    on_disk: Option<Vec<u8>>,
    objects: BTreeMap<PackageDigest, Vec<u8>>,
    grants: Grants,
}

fn snapshot(st: &Store<Failing>) -> Snapshot {
    let inner = &st.backend().inner;
    // Archives the index doesn't name are swept at the next open, so only named ones count.
    let objects = inner.objects.iter().filter(|(d, _)| st.index().objects.contains_key(*d)).map(|(d, b)| (*d, b.clone())).collect();
    Snapshot { index: st.index().clone(), on_disk: inner.index.clone(), objects, grants: grants_of(inner) }
}

/// Whether `after` grants nothing `before` didn't: the same grants, any of them now revoked, and
/// the same next epoch.
fn only_revoked(before: &Grants, after: &Grants) -> bool {
    before.next_epoch == after.next_epoch
        && before.grants.len() == after.grants.len()
        && before.grants.iter().zip(&after.grants).all(|(b, a)| Grant { revoked: a.revoked, ..b.clone() } == *a && (a.revoked || !b.revoked))
}

fn run(st: &mut Store<Failing>, p: &Packages, op: &Op) -> Result<(), nodecraft_types::Error> {
    let trust = TrustStore::default();
    let pkg = |i: usize| id(PACKAGES[i]);
    match *op {
        Op::Import(i, v) => st.import(&p.bytes[i][v], &trust, false).map(drop),
        Op::Update(i, v) => st.update(&p.bytes[i][v], &trust, false).map(drop),
        Op::Enable(i) => st.enable(&pkg(i), &trust, &none(), &BTreeSet::new()).map(drop),
        Op::Disable(i) => st.disable(&pkg(i)),
        Op::Rollback(i, v) => st.rollback(&pkg(i), &p.digests[i][v]),
        Op::Uninstall(i) => st.uninstall(&pkg(i), &BTreeSet::new()).map(drop),
        Op::Prune => st.prune(&BTreeSet::new()).map(drop),
        Op::Grant(i) => st.grant(&pkg(i), &trust).map(drop),
        Op::Revoke(n) => {
            let ids: Vec<String> = grants_of(&st.backend().inner).grants.into_iter().map(|g| g.grant_id).collect();
            let grant_id = ids.get(n % ids.len().max(1)).cloned().unwrap_or_else(|| "g999".to_owned());
            st.revoke(&grant_id).map(drop)
        }
    }
}

/// Whether `after` differs from `before` only by enabled packages now disabled: what a revoke
/// commits before it writes the grants.
fn only_disabled(before: &Index, after: &Index) -> bool {
    before.objects == after.objects
        && before.active.len() == after.active.len()
        && before.active.iter().zip(&after.active).all(|((bi, (bd, bs)), (ai, (ad, as_)))| bi == ai && bd == ad && (bs == as_ || *as_ == State::Disabled))
}

/// An enabled package that runs anything holds a live grant of exactly its active digest.
fn enabled_text_holds_a_live_grant(st: &Store<Failing>) -> bool {
    let grants = grants_of(&st.backend().inner);
    st.index().active.get(&id(PACKAGES[0])).is_none_or(|(digest, state)| *state == State::Disabled || grants.live(&id(PACKAGES[0]), digest).is_some())
}

/// Runs `steps`, checking after each that a failed step left nothing half done.
fn check_sequence(steps: &[(Op, Fault)]) -> Result<(), TestCaseError> {
    let p = packages();
    let faults = Rc::new(Faults::default());
    let mut st = Store::open(Failing { inner: MemoryBackend::default(), faults: faults.clone() }).unwrap();
    let mut epoch = 0u64;
    for (step, (op, fault)) in steps.iter().enumerate() {
        let before = snapshot(&st);
        faults.index.set(*fault == Fault::Index);
        faults.object.set(*fault == Fault::Object);
        faults.grants.set(*fault == Fault::Grants);
        let result = run(&mut st, &p, op);
        let injected = (*fault == Fault::Index && !faults.index.get())
            || (*fault == Fault::Object && !faults.object.get())
            || (*fault == Fault::Grants && !faults.grants.get());
        faults.index.set(false);
        faults.object.set(false);
        faults.grants.set(false);
        let after = snapshot(&st);
        let at = format!("step {step}: {op:?} with {fault:?} -> {result:?}");
        match &result {
            Err(error) if injected => {
                prop_assert_eq!(error.message.as_str(), "disk full", "{}", at);
                if matches!(op, Op::Revoke(_)) && *fault == Fault::Grants {
                    // A revoke disables first, so a failed grants write leaves it disabled.
                    prop_assert!(only_disabled(&before.index, &after.index), "{at}: {:?} -> {:?}", before.index, after.index);
                } else {
                    // A failed write may revoke before it fails, never grant: the index and the
                    // archives it names are as they were, and grants only went away.
                    prop_assert_eq!(&after.index, &before.index, "{}", at);
                    prop_assert_eq!(&after.on_disk, &before.on_disk, "{}", at);
                }
                prop_assert_eq!(&after.objects, &before.objects, "{}", at);
                prop_assert!(only_revoked(&before.grants, &after.grants), "{at}: grants {:?} -> {:?}", before.grants, after.grants);
            }
            Err(_) => prop_assert_eq!(&after, &before, "a refused step changes nothing: {}", at),
            Ok(()) => prop_assert!(!injected, "{at}: the failed write was ignored"),
        }
        // Epochs never go down, and the store's view is what a fresh open reads.
        prop_assert!(after.grants.next_epoch >= epoch, "{at}: epoch {} after {epoch}", after.grants.next_epoch);
        epoch = after.grants.next_epoch;
        prop_assert!(enabled_text_holds_a_live_grant(&st), "{at}: enabled without a grant");
        let reopened = Store::open(MemoryBackend {
            index: st.backend().inner.index.clone(),
            objects: st.backend().inner.objects.clone(),
            grants: st.backend().inner.grants.clone(),
            ..MemoryBackend::default()
        })
        .unwrap();
        prop_assert_eq!(reopened.index(), st.index(), "{}", at);
        for digest in st.index().objects.keys() {
            prop_assert!(reopened.backend().objects.contains_key(digest), "{at}: a named archive is missing");
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]

    #[test]
    fn a_failed_step_leaves_nothing_half_done(steps in proptest::collection::vec((op(), fault()), 1..24)) {
        check_sequence(&steps)?;
    }
}

/// Revoking the grant an enabled package runs under, with the grants write failing: it stays
/// disabled, and that is the only change.
#[test]
fn a_revoke_whose_grants_write_fails_leaves_the_package_disabled() {
    check_sequence(&[(Op::Import(0, 0), Fault::None), (Op::Grant(0), Fault::None), (Op::Enable(0), Fault::None), (Op::Revoke(0), Fault::Grants)]).unwrap();
}
