//! Real native lock ownership, including the former comparison/publication race.
use super::*;
use crate::{FolderBackend, StoreBackend, StoreLock};
use nodecraft_types::Error;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

struct Gated {
    inner: FolderBackend,
    entered: SyncSender<()>,
    release: Receiver<()>,
    fail: bool,
    panic: bool,
}
impl StoreBackend for Gated {
    fn transaction(&self) -> Result<Box<dyn StoreLock>, Error> {
        self.inner.transaction()
    }
    fn read_index(&self) -> Result<Option<Vec<u8>>, Error> {
        self.inner.read_index()
    }
    fn write_index(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!self.panic, "injected backend panic before index publication");
        if self.fail {
            return Err(Error::new(ErrorKind::Internal, "injected index failure"));
        }
        self.inner.write_index(bytes)
    }
    fn read_object(&self, d: &PackageDigest) -> Result<Vec<u8>, Error> {
        self.inner.read_object(d)
    }
    fn write_object(&mut self, d: &PackageDigest, bytes: &[u8]) -> Result<(), Error> {
        self.inner.write_object(d, bytes)
    }
    fn remove_object(&mut self, d: &PackageDigest) -> Result<(), Error> {
        self.inner.remove_object(d)
    }
    fn list_objects(&self) -> Result<Vec<PackageDigest>, Error> {
        self.inner.list_objects()
    }
}

fn open(path: &std::path::Path) -> Store<FolderBackend> {
    Store::open(FolderBackend::new(path.to_owned()).unwrap()).unwrap()
}

#[test]
fn an_independent_writer_cannot_enter_between_comparison_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let (entered, at_write) = sync_channel(1);
    let (release, permit) = sync_channel(1);
    let mut a = Store::open(Gated { inner: FolderBackend::new(dir.path().to_owned()).unwrap(), entered, release: permit, fail: false, panic: false }).unwrap();
    let mut b = open(dir.path());
    let task = std::thread::spawn(move || {
        a.import(&package(&spec("io.example.one")), &TrustStore::default(), false).unwrap();
        a
    });
    at_write.recv_timeout(Duration::from_secs(10)).unwrap();
    // The first Store has already compared the index but has not renamed it yet.
    assert_eq!(b.import(&package(&spec("io.example.two")), &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(Store::open(FolderBackend::new(dir.path().to_owned()).unwrap()).err().unwrap().kind, ErrorKind::Busy, "opening/sweeping uses the same lock");
    release.send(()).unwrap();
    let a = task.join().unwrap();
    assert_eq!(a.index().active.len(), 1);
    // Preserve the existing optimistic conflict contract; a retry takes the refreshed state.
    assert_eq!(b.import(&package(&spec("io.example.two")), &TrustStore::default(), false).unwrap_err().kind, ErrorKind::Conflict);
    b.import(&package(&spec("io.example.two")), &TrustStore::default(), false).unwrap();
    assert_eq!(open(dir.path()).index().active.len(), 2);
}

#[test]
fn errors_and_unwinding_release_the_real_file_lock_and_preserve_the_old_index() {
    for panic in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        open(dir.path()).import(&package(&spec("io.example.old")), &TrustStore::default(), false).unwrap();
        let before = std::fs::read(dir.path().join("index.json")).unwrap();
        let (entered, at_write) = sync_channel(1);
        let (release, permit) = sync_channel(1);
        let mut a = Store::open(Gated { inner: FolderBackend::new(dir.path().to_owned()).unwrap(), entered, release: permit, fail: !panic, panic }).unwrap();
        let task = std::thread::spawn(move || a.import(&package(&spec("io.example.new")), &TrustStore::default(), false));
        at_write.recv_timeout(Duration::from_secs(10)).unwrap();
        release.send(()).unwrap();
        let result = task.join();
        if panic {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap().unwrap_err().kind, ErrorKind::Internal);
        }
        assert_eq!(std::fs::read(dir.path().join("index.json")).unwrap(), before);
        assert_eq!(open(dir.path()).index().active.len(), 1, "the lock was released on either failure path");
        assert!(dir.path().join(".lock").is_file());
    }
}

#[test]
fn unrelated_stores_are_not_serialized_and_the_lock_file_is_never_replaced() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let backend = FolderBackend::new(a.path().to_owned()).unwrap();
    std::fs::write(a.path().join(".lock"), b"permanent lock marker").unwrap();
    let held = backend.transaction().unwrap();
    assert!(open(b.path()).index().active.is_empty());
    assert_eq!(FolderBackend::new(a.path().to_owned()).unwrap().transaction().err().unwrap().kind, ErrorKind::Busy);
    drop(held);
    open(a.path()).import(&package(&spec("io.example.one")), &TrustStore::default(), false).unwrap();
    assert_eq!(std::fs::read(a.path().join(".lock")).unwrap(), b"permanent lock marker");
}

// Same semantic inventory, different signature/ZIP representation: a rejected stale writer
// must not replace a committed archive merely because its local index predates that digest.
fn signed_variants(version: &str) -> (Vec<u8>, Vec<u8>, PackageDigest, TrustStore) {
    use ed25519_dalek::Signer;
    let key = ed25519_dalek::SigningKey::from_bytes(&random_seed());
    let mut sp = spec("io.example.signed");
    sp.version = version;
    sp.publisher = Some("pub-key");
    let f = files(&sp);
    let mut listed: Vec<(&str, &[u8])> = f.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
    let digest = digest_of(&sp);
    let sig = key.sign(&nodecraft_plugin::signature::signed_message(&digest));
    let hex: String = sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let signature = format!(r#"{{"algorithm":"ed25519","key_id":"pub-key","package_digest":"{digest}","signature":"{hex}","signature_version":1}}"#);
    let signed = archive::build(&listed, Some(signature.as_bytes())).unwrap();
    listed.reverse();
    let unsigned = archive::build(&listed, None).unwrap();
    assert_ne!(signed, unsigned);
    assert_eq!(archive::open(&signed, ArchiveLimits::DEFAULT).unwrap().digest, archive::open(&unsigned, ArchiveLimits::DEFAULT).unwrap().digest);
    let mut trust = TrustStore::default();
    trust.keys.insert(
        Token::parse("pub-key").unwrap(),
        TrustedKey { public_key: key.verifying_key().to_bytes(), provenance: "fixture".into(), status: KeyStatus::Trusted },
    );
    (signed, unsigned, digest, trust)
}

fn disk_snapshot(root: &std::path::Path) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let backend = FolderBackend::new(root.to_owned()).unwrap();
    let _guard = backend.transaction().unwrap();
    let mut objects: Vec<_> = std::fs::read_dir(root.join("objects"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name().into_string().unwrap(), std::fs::read(entry.path()).unwrap())
        })
        .collect();
    objects.sort();
    (std::fs::read(root.join("index.json")).unwrap(), objects)
}

#[test]
fn stale_import_preserves_the_committed_signed_archive_at_the_same_digest() {
    let dir = tempfile::tempdir().unwrap();
    let (signed, unsigned, digest, trust) = signed_variants("1.0.0");
    let (mut a, mut b) = (open(dir.path()), open(dir.path()));
    a.import(&signed, &trust, false).unwrap();
    let before = disk_snapshot(dir.path());
    assert_eq!(b.import(&unsigned, &trust, false).unwrap_err().kind, ErrorKind::Conflict);
    assert!(disk_snapshot(dir.path()) == before, "a refused request must not replace signed bytes with unsigned bytes");
    assert_eq!(b.index(), a.index(), "Conflict refreshes the stale writer");
    assert_eq!(b.backend().read_object(&digest).unwrap(), signed);
    assert_eq!(b.index().objects[&digest].signed_by.as_deref(), Some("pub-key"));
    assert!(b.load(&id("io.example.signed")).unwrap().signature.is_some());
    b.enable(&id("io.example.signed"), &trust, &none(), &BTreeSet::new()).unwrap();
}

#[test]
fn stale_update_preserves_the_new_committed_digest_and_all_prior_archives() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = spec("io.example.signed");
    old.version = "0.9.0";
    open(dir.path()).import(&package(&old), &TrustStore::default(), false).unwrap();
    let (mut a, mut b) = (open(dir.path()), open(dir.path()));
    let (signed, unsigned, digest, trust) = signed_variants("1.0.0");
    a.update(&signed, &trust, false).unwrap();
    let before = disk_snapshot(dir.path());
    assert_eq!(b.update(&unsigned, &trust, false).unwrap_err().kind, ErrorKind::Conflict);
    assert!(disk_snapshot(dir.path()) == before, "a refused update must preserve all bytes");
    assert_eq!(b.index(), a.index());
    assert_eq!(b.backend().read_object(&digest).unwrap(), signed);
    old.version = "1.1.0";
    b.update(&package(&old), &TrustStore::default(), false).unwrap();
    assert_eq!(b.backend().read_object(&digest).unwrap(), signed, "an explicit later update also retains the old signed digest");
}

#[test]
fn stale_related_mutations_refuse_before_noop_or_cleanup_and_refresh_the_cache() {
    for op in ["rollback", "enable", "disable", "uninstall", "prune"] {
        let dir = tempfile::tempdir().unwrap();
        let mut a = open(dir.path());
        let mut sp = spec("io.example.palette");
        let old = a.import(&package(&sp), &TrustStore::default(), false).unwrap().entry.digest;
        sp.version = "1.1.0";
        a.update(&package(&sp), &TrustStore::default(), false).unwrap();
        let mut stale = open(dir.path());
        a.import(&package(&spec("io.example.other")), &TrustStore::default(), false).unwrap();
        let before = disk_snapshot(dir.path());
        let temporary = dir.path().join("index.json.abandoned.new");
        let file = std::fs::File::create(&temporary).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(1200)).unwrap();
        drop(file);
        let result = match op {
            "rollback" => stale.rollback(&id(sp.id), &old),
            "enable" => stale.enable(&id(sp.id), &TrustStore::default(), &none(), &BTreeSet::new()).map(|_| ()),
            "disable" => stale.disable(&id(sp.id)),
            "uninstall" => stale.uninstall(&id(sp.id), &BTreeSet::new()).map(|_| ()),
            "prune" => stale.prune(&BTreeSet::new()).map(|_| ()),
            _ => panic!("fixture operation"),
        };
        assert_eq!(result.unwrap_err().kind, ErrorKind::Conflict, "{op}");
        assert!(temporary.exists(), "stale {op} must not sweep temporary files");
        assert_eq!(stale.index(), a.index(), "{op}");
        assert!(disk_snapshot(dir.path()) == before, "{op}");
        // Startup cleanup still happens, under the actual guard and current decoded index.
        drop(open(dir.path()));
        assert!(!temporary.exists());
    }
}
