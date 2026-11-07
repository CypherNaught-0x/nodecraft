use super::*;
use nodecraft_sync::{Frame, Mirror, Role};

fn grant(role: Role, name: &str, expires: Option<u64>) -> Grant {
    Grant { role, name: name.into(), expires }
}

/// A room holding a little document, one link and one asset, as a relay would hand it over.
fn snapshot(used: u64) -> Snapshot {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "Kept").unwrap();
    nodecraft_graph::Change::AddCanvas { canvas: nodecraft_graph::Canvas::new(nodecraft_types::CanvasId::from_bytes([2; 16]), "Main") }
        .apply(&mut project)
        .unwrap();
    let (_, document) = Mirror::share(&project).unwrap();
    let bytes = b"pixels".to_vec();
    Snapshot { document, links: vec![([9; 32], grant(Role::View, "Ana", None))], assets: vec![(*blake3::hash(&bytes).as_bytes(), bytes)], used }
}

#[test]
fn a_kept_room_comes_back_after_the_store_is_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), 3600, 1 << 30, 100).unwrap();
    let room = key(&nodecraft_sync::RoomId::from_bytes([7; 32]));
    store.save(room, snapshot(100), &BTreeSet::new()).unwrap();
    // The folder is named by the hash, and the room id is written nowhere.
    let id = nodecraft_sync::RoomId::from_bytes([7; 32]);
    for entry in walk(dir.path()) {
        let name = entry.to_string_lossy().into_owned();
        assert!(!name.contains(id.as_str()), "{name}");
        assert!(!fs::read(&entry).unwrap_or_default().windows(id.as_str().len()).any(|w| w == id.as_str().as_bytes()), "{name}");
    }
    drop(store);

    let mut store = Store::open(dir.path(), 3600, 1 << 30, 200).unwrap();
    assert!(store.has(&room));
    assert_eq!(store.holding(&[9; 32], 200), Some(room));
    assert_eq!(store.holding(&[8; 32], 200), None);
    let mut restored = store.load(&room).unwrap();
    assert_eq!(restored.links().count(), 1);
    assert_eq!(restored.assets().count(), 1);
    let out = restored.join_link(1, [9; 32], 200);
    assert!(out.iter().any(|o| matches!(o, nodecraft_sync::Out::Send(1, Frame::Welcome { role: Role::View, .. }))), "{out:?}");
}

#[test]
fn rooms_nobody_used_for_the_keep_are_forgotten_and_a_busy_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), 3600, 1 << 30, 0).unwrap();
    let (old, busy) = ([1; 32], [2; 32]);
    store.save(old, snapshot(0), &BTreeSet::new()).unwrap();
    store.save(busy, snapshot(0), &BTreeSet::new()).unwrap();
    store.expire(10_000, &[busy].into());
    assert!(!store.has(&old) && store.has(&busy));
    assert!(!dir.path().join(hex(&old)).exists(), "its files go too");
    // Opened again later, nothing is busy and both are past the keep.
    drop(store);
    let store = Store::open(dir.path(), 3600, 1 << 30, 10_000).unwrap();
    assert_eq!(store.held().0, 0);
}

#[test]
fn past_its_bytes_a_kept_room_that_grows_lets_the_longest_unused_go_first_and_a_new_one_takes_only_free_space() {
    let dir = tempfile::tempdir().unwrap();
    let one = {
        let mut probe = Store::open(&dir.path().join("probe"), 3600, 1 << 30, 0).unwrap();
        probe.save([0; 32], snapshot(0), &BTreeSet::new()).unwrap();
        probe.held().1
    };
    // Room for three rooms' bytes, not four. Half a room of slack: rooms that differ in a few
    // bytes (a longer `used`, the document's encoding) must not decide whether the third fits.
    let cap = one * 3 + one / 2;
    let mut store = Store::open(&dir.path().join("store"), 3600, cap, 0).unwrap();
    store.save([1; 32], snapshot(10), &BTreeSet::new()).unwrap();
    store.save([2; 32], snapshot(5), &BTreeSet::new()).unwrap();
    // A new room fits in the free space; the next new one doesn't, and pushes nobody out
    // (anyone can open a room, so new ones never take kept rooms' places).
    store.save([3; 32], snapshot(20), &BTreeSet::new()).unwrap();
    assert_eq!(store.save([4; 32], snapshot(30), &BTreeSet::new()), Err(NotKept::Full));
    assert!([[1; 32], [2; 32], [3; 32]].iter().all(|key| store.has(key)) && !store.has(&[4; 32]));
    // A kept room that grows makes space: the one used longest ago goes, never a busy one.
    let mut grown = snapshot(40);
    let more = vec![7u8; usize::try_from(one).unwrap()];
    grown.assets.push((*blake3::hash(&more).as_bytes(), more));
    store.save([1; 32], grown, &[[2; 32]].into()).unwrap();
    assert!(store.has(&[1; 32]) && store.has(&[2; 32]) && !store.has(&[3; 32]), "the oldest idle room went: {:?}", store.held());
    assert!(store.held().1 <= cap);
    // Saving a room again doesn't count its old bytes twice.
    store.save([2; 32], snapshot(50), &BTreeSet::new()).unwrap();
    assert!(store.has(&[1; 32]) && store.has(&[2; 32]), "{:?}", store.held());
    // A room discarded (a save of it failed) is gone with its files.
    store.discard(&[2; 32]);
    assert!(!store.has(&[2; 32]) && !dir.path().join("store").join(hex(&[2; 32])).exists());
}

#[test]
fn what_doesnt_read_back_is_forgotten_and_never_panics() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path(), 3600, 1 << 30, 0).unwrap();
    store.save([1; 32], snapshot(0), &BTreeSet::new()).unwrap();
    store.save([2; 32], snapshot(0), &BTreeSet::new()).unwrap();
    store.save([3; 32], snapshot(0), &BTreeSet::new()).unwrap();
    // A garbled document, a links file that isn't JSON, and a stray folder and file.
    fs::write(dir.path().join(hex(&[1; 32])).join("room"), b"NCRROOM1garbage").unwrap();
    fs::write(dir.path().join(hex(&[2; 32])).join("links.json"), b"{nope").unwrap();
    fs::create_dir_all(dir.path().join("not-a-room")).unwrap();
    fs::write(dir.path().join("notes.txt"), b"mine").unwrap();
    drop(store);
    let mut store = Store::open(dir.path(), 3600, 1 << 30, 0).unwrap();
    assert!(!store.has(&[2; 32]), "an unreadable links file is forgotten when opened");
    assert!(store.load(&[1; 32]).is_none(), "a garbled document doesn't restore");
    assert!(!store.has(&[1; 32]));
    assert!(store.load(&[3; 32]).is_some());
    assert!(dir.path().join("not-a-room").exists() && dir.path().join("notes.txt").exists(), "what isn't a room is left alone");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        out.push(path.clone());
        if path.is_dir() {
            out.extend(walk(&path));
        }
    }
    out
}
