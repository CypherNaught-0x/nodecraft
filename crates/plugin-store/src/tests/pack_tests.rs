//! Property tests (#866): an author's folder, generated with every shape the inventory allows
//! (portable paths, nesting, sizes, file counts), packs to the archive install verifies, and
//! `pack` → `inventory` → verify agree on every file, size and the digest.
use super::*;
use crate::author;
use nodecraft_plugin::inventory::{MAX_FILES, MAX_PATH, MAX_SEGMENTS};
use proptest::prelude::*;
use std::collections::BTreeMap;

/// The files every generated package has: its manifest, license and the descriptor it registers.
fn base() -> Vec<(String, Vec<u8>)> {
    files(&spec("io.example.packed"))
}

/// Whether a generated path can sit beside the base files and the others in a real folder:
/// portable, not reserved, no case-fold collision, and no file where another needs a folder.
fn fits(path: &str, taken: &BTreeMap<String, ()>) -> bool {
    if nodecraft_plugin::inventory::InventoryPath::parse(path).is_err() {
        return false;
    }
    let folded = path.to_ascii_lowercase();
    let prefix_clash = taken.keys().any(|other| other.starts_with(&format!("{folded}/")) || folded.starts_with(&format!("{other}/")));
    !taken.contains_key(&folded) && !prefix_clash
}

/// A folder: the base files plus `extra`, keeping only paths that fit.
fn folder(extra: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>)> {
    let mut out = base();
    let mut taken: BTreeMap<String, ()> = out.iter().map(|(p, _)| (p.to_ascii_lowercase(), ())).collect();
    for (path, bytes) in extra {
        if out.len() < MAX_FILES && fits(&path, &taken) {
            taken.insert(path.to_ascii_lowercase(), ());
            out.push((path, bytes));
        }
    }
    out
}

/// One path segment from the portable set, never `.`/`..` and never ending in a dot.
fn segment() -> impl Strategy<Value = String> {
    "[A-Za-z0-9_-]([A-Za-z0-9._-]{0,14}[A-Za-z0-9_-])?"
}

/// A path of 1..=MAX_SEGMENTS segments, cut to at most MAX_PATH bytes at a segment boundary.
fn path() -> impl Strategy<Value = String> {
    prop::collection::vec(segment(), 1..=MAX_SEGMENTS).prop_map(|segments| {
        let mut out = String::new();
        for s in segments {
            let next = if out.is_empty() { s.clone() } else { format!("{out}/{s}") };
            if next.len() > MAX_PATH {
                break;
            }
            out = next;
        }
        out
    })
}

fn contents() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![Just(Vec::new()), prop::collection::vec(any::<u8>(), 0..64), prop::collection::vec(any::<u8>(), 0..16 * 1024),]
}

/// Packs `folder`, checks every agreement, and returns what install verified.
fn round_trip(folder: &[(String, Vec<u8>)]) -> archive::Package {
    let packed = author::pack(folder.to_vec(), None).unwrap();
    let (inventory, digest) = author::inventory(folder.to_vec()).unwrap();
    assert_eq!((&packed.inventory, packed.digest), (&inventory, digest), "pack and inventory agree");
    // The listing reads back to itself.
    let decoded = Inventory::decode(inventory.as_bytes()).unwrap();
    assert_eq!(decoded.canonical_json(), inventory);
    assert_eq!(decoded.digest(), digest);
    // Install's verifier sees exactly the folder: each path, its size and its bytes.
    let package = archive::open(&packed.bytes, ArchiveLimits::DEFAULT).unwrap();
    assert_eq!(package.digest, digest);
    assert_eq!(package.inventory, decoded);
    let opened: BTreeMap<&str, &[u8]> = package.files.iter().map(|(p, b)| (p.as_str(), &b[..])).collect();
    let given: BTreeMap<&str, &[u8]> = folder.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
    assert_eq!(opened, given);
    for (path, bytes) in folder {
        assert_eq!(package.inventory.get(path).map(|e| e.bytes), Some(bytes.len() as u64), "{path}");
    }
    // The same files in any order pack to the same bytes.
    let mut reversed = folder.to_vec();
    reversed.reverse();
    assert_eq!(author::pack(reversed, None).unwrap().bytes, packed.bytes);
    package
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn generated_folders_pack_list_and_verify_alike(extra in prop::collection::vec((path(), contents()), 0..48)) {
        let folder = folder(extra);
        round_trip(&folder);
        // The package installs as packed.
        let mut st = store();
        let packed = author::pack(folder, None).unwrap();
        prop_assert_eq!(st.import(&packed.bytes, &TrustStore::default(), false).unwrap().entry.digest, packed.digest);
    }

    #[test]
    fn a_changed_byte_or_size_changes_the_digest(extra in prop::collection::vec((path(), contents()), 1..8), pick in any::<prop::sample::Index>(), grow in any::<bool>()) {
        let folder = folder(extra);
        let before = author::inventory(folder.clone()).unwrap().1;
        let mut changed = folder;
        // A generated file, after the base ones (the manifest and descriptor must stay valid JSON).
        let generated = changed.len() - base().len();
        prop_assume!(generated > 0);
        let at = base().len() + pick.index(generated);
        let bytes = &mut changed[at].1;
        if grow || bytes.is_empty() { bytes.push(0) } else { bytes[0] ^= 1 }
        prop_assert_ne!(author::pack(changed, None).unwrap().digest, before);
    }
}

#[test]
fn folders_at_the_inventory_limits_round_trip_and_one_past_them_is_refused() {
    // The deepest and longest path allowed: MAX_SEGMENTS segments, MAX_PATH bytes.
    let head = "a/".repeat(MAX_SEGMENTS - 1);
    let deepest = format!("{head}{}", "z".repeat(MAX_PATH - head.len()));
    assert_eq!((deepest.len(), deepest.split('/').count()), (MAX_PATH, MAX_SEGMENTS));
    // The most files allowed, nested, with one large file.
    let mut extra = vec![(deepest.clone(), b"deep".to_vec()), ("big.bin".to_owned(), vec![7; 4 * 1024 * 1024])];
    for i in 0..MAX_FILES {
        extra.push((format!("n/{:02}/f{i}.txt", i % 37), i.to_le_bytes().to_vec()));
    }
    let full = folder(extra);
    assert_eq!(full.len(), MAX_FILES);
    let package = round_trip(&full);
    assert_eq!(package.files.len(), MAX_FILES);

    // One past each limit is refused, by pack and inventory alike.
    let one_more = |path: String| {
        let mut f = base();
        f.push((path, b"x".to_vec()));
        f
    };
    let too_long = format!("{head}{}", "z".repeat(MAX_PATH - head.len() + 1));
    let too_deep = format!("{}z", "a/".repeat(MAX_SEGMENTS));
    for (case, files) in [
        ("path length", one_more(too_long)),
        ("nesting", one_more(too_deep)),
        ("file count", {
            let mut f = full.clone();
            f.push(("one-more.txt".to_owned(), Vec::new()));
            f
        }),
    ] {
        assert_eq!(author::pack(files.clone(), None).unwrap_err().kind, ErrorKind::InvalidInput, "{case}");
        assert_eq!(author::inventory(files).unwrap_err().kind, ErrorKind::InvalidInput, "{case}");
    }
}

#[test]
fn a_package_larger_than_an_archive_may_be_is_refused_before_it_is_written() {
    let mut f = base();
    f.push(("big.bin".to_owned(), vec![0; ArchiveLimits::DEFAULT.archive_bytes]));
    // The inventory allows it (under 128 MiB); the archive install opens does not.
    assert!(author::inventory(f.clone()).is_ok());
    assert_eq!(author::pack(f, None).unwrap_err().kind, ErrorKind::InvalidInput);
}
