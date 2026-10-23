//! Sealing a large shared asset holds its project's storage for a while (#855): small imports
//! wait in memory meanwhile, an ask the storage is too busy for is answered later, and a seal
//! stops when asked to, or once no card uses its asset any more (#880).
use super::*;
use nodecraft_format::asset_source::{AssetRange, AssetSource, MemorySource};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Bytes that trickle out a byte at a time, slowly, until `open`: a seal of them holds its
/// storage for as long as a test needs. `started` is set at the first read.
struct Slow {
    bytes: Arc<[u8]>,
    open: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
    _gone: Option<SetOnDrop>,
}

/// Sets its flag once dropped: says a spool's asset was let go.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl AssetSource for Slow {
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        self.started.store(true, Ordering::Release);
        let rest = self.bytes.get(usize::try_from(offset).unwrap()..).unwrap_or_default();
        let n = if self.open.load(Ordering::Acquire) {
            rest.len().min(buffer.len())
        } else {
            std::thread::sleep(Duration::from_millis(2));
            rest.len().min(buffer.len()).min(1)
        };
        buffer[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}

/// A seal of `len` bytes of noise on `store`, in another thread, held until `open` is set, and
/// begun when this returns.
fn seal_held(store: nodecraft_format::AssetStore, len: usize, cancel: TransferCancel) -> (Arc<AtomicBool>, std::thread::JoinHandle<Result<Blake3, Error>>) {
    let open = Arc::new(AtomicBool::new(false));
    let started = Arc::new(AtomicBool::new(false));
    let source = Slow { bytes: noise(len).into(), open: open.clone(), started: started.clone(), _gone: None };
    let range = AssetRange::new(Arc::new(source), 0, len as u64).unwrap();
    let sealing = std::thread::spawn(move || store.seal(&range, &cancel, &StreamMetrics::default()).map(|sealed| sealed.hash()));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "the seal never began");
        std::thread::sleep(Duration::from_millis(1));
    }
    (open, sealing)
}

#[test]
fn a_model_result_arriving_while_a_shared_asset_is_sealed_is_imported_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), dir.path());
    let (open, sealing) = seal_held(session.assets().clone(), 1 << 20, TransferCancel::default());

    // As the run host imports a model's result: no Busy, so nothing is held for a retry.
    let hash = session.import_asset(b"a model's result").unwrap();
    assert_eq!(session.assets().get(&hash), Some(&b"a model's result"[..]), "it waits in memory");
    // A paste or drop goes the same way.
    let range = AssetRange::new(Arc::new(MemorySource::new(Arc::from(&b"pasted"[..]))), 0, 6).unwrap();
    let pasted = session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap();
    assert!(session.assets().contains(&pasted));
    // Only while there's room: a large one still waits for the storage.
    let large: Arc<[u8]> = noise(nodecraft_format::MAX_ASSET_BYTES + 1).into();
    let len = large.len() as u64;
    let range = AssetRange::new(Arc::new(MemorySource::new(large)), 0, len).unwrap();
    assert_eq!(session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap_err().kind, ErrorKind::Busy);

    open.store(true, Ordering::Release);
    sealing.join().unwrap().unwrap();
    // Once the storage is free, imports go to disk again.
    let later = session.import_asset(b"a later result").unwrap();
    assert!(session.assets().contains(&later) && session.assets().get(&later).is_none());
    // And what waited in memory is still read back whole.
    let held = session.assets().verified_content(&hash, &Default::default(), &Default::default()).unwrap();
    assert_eq!(held.byte_len(), 16);
}

#[test]
fn an_ask_the_storage_is_too_busy_for_is_answered_later_in_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), &dir.path().join("sharer"));
    let bytes: Arc<[u8]> = noise(1 << 20).into();
    let range = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, 1 << 20).unwrap();
    let hash = session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap();
    let mut card = Node::new(nid(1), "text");
    card.params.insert("image".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: card }])).unwrap();
    let mut net = Net::new();
    net.enter(Person { id: 1, collab: Collab::share(&session, room_id()).unwrap(), session: Some(session) });
    net.settle();

    let store = net.session(1).assets().clone();
    let (open, sealing) = seal_held(store, 1 << 20, TransferCancel::default());
    let Person { collab, session, .. } = net.people.iter_mut().find(|p| p.id == 1).unwrap();
    let session = session.as_ref().unwrap();
    collab.receive(Frame::AssetGet(vec![*hash.as_bytes()])).unwrap();
    let answers = |frames: &[Frame]| frames.iter().filter(|frame| matches!(frame, Frame::AssetPut { .. } | Frame::AssetPart { .. })).count();
    assert_eq!(answers(&collab.publish(session).unwrap()), 0, "the storage is busy");
    assert!(collab.puts_off(), "the ask waits rather than being dropped");
    assert!(!collab.owes_assets(), "and isn't tried turn after turn");

    open.store(true, Ordering::Release);
    sealing.join().unwrap().unwrap();
    assert!(collab.retry_put_off());
    assert!(!collab.puts_off());
    assert!(answers(&collab.publish(session).unwrap()) >= 1, "answered once the storage is free");
}

#[test]
fn stop_cancels_a_seal_underway_within_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let session = on_disk(empty(), dir.path());
    let cancel = TransferCancel::default();
    let (_open, sealing) = seal_held(session.assets().clone(), 1 << 20, cancel.clone());
    let stopped = Instant::now();
    cancel.cancel();
    let error = sealing.join().unwrap().unwrap_err();
    assert!(TransferCancel::is_cancellation(&error), "{error:?}");
    assert!(stopped.elapsed() < Duration::from_secs(1), "{:?}", stopped.elapsed());
    // The storage is free again at once.
    let mut session = session;
    let hash = session.import_asset(b"after the stop").unwrap();
    assert!(session.assets().get(&hash).is_none(), "on disk, not waiting in memory");
}

#[test]
fn one_large_asset_is_sealed_a_turn_and_a_stopped_seal_waits() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), &dir.path().join("sharer"));
    let mut hashes = Vec::new();
    for (n, seed) in [(1u8, 3usize), (2, 5)] {
        let bytes: Arc<[u8]> = noise(nodecraft_sync::WINDOW_BYTES as usize + seed).into();
        let len = bytes.len() as u64;
        let range = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, len).unwrap();
        let hash = session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap();
        let mut card = Node::new(nid(n), "text");
        card.params.insert("image".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
        session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: card }])).unwrap();
        hashes.push(hash);
    }
    assert_ne!(hashes[0], hashes[1]);
    let mut net = Net::new();
    net.disk = Some(dir.path().join("joiners"));
    net.enter(Person { id: 1, collab: Collab::share(&session, room_id()).unwrap(), session: Some(session) });
    let joiner = Collab::join(room_id()).with_spool(Box::new(MemorySpool::new(64 << 20))).sealing();
    net.enter(Person { id: 2, collab: joiner, session: None });
    assert_eq!(net.person(2).collab.waiting_for_assets(), 2);

    let store = net.session(2).assets().clone();
    // Stopped before it began: nothing is sealed or given up.
    let stopped = TransferCancel::default();
    stopped.cancel();
    assert!(!net.collab(2).seal_arrivals(&store, &stopped));
    assert_eq!(net.person(2).collab.waiting_for_assets(), 2);
    assert_eq!(net.person(2).collab.unusable_assets(), 0);
    // One a turn.
    assert!(net.collab(2).seal_arrivals(&store, &Default::default()));
    assert!(net.person(2).collab.seals_pending());
    assert!(net.collab(2).seal_arrivals(&store, &Default::default()));
    assert!(!net.person(2).collab.seals_pending());
    assert!(!net.collab(2).seal_arrivals(&store, &Default::default()));
    net.settle();
    assert_eq!(net.person(2).collab.waiting_for_assets(), 0);
    for hash in &hashes {
        assert!(net.session(2).assets().contains(hash));
    }
}

/// A spool in memory whose assets, once whole, trickle out as [`Slow`] does, so a seal of one
/// is under way for as long as a test needs; `gone` is set once the last is let go.
struct SlowSpool {
    kept: BTreeMap<Blake3, Vec<u8>>,
    started: Arc<AtomicBool>,
    gone: Arc<AtomicBool>,
}

impl SlowSpool {
    fn new() -> (Self, Arc<AtomicBool>, Arc<AtomicBool>) {
        let (started, gone) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        (Self { kept: BTreeMap::new(), started: started.clone(), gone: gone.clone() }, started, gone)
    }
}

impl Spool for SlowSpool {
    fn limit(&self) -> u64 {
        64 << 20
    }

    fn budget(&self) -> u64 {
        64 << 20
    }

    fn begin(&mut self, hash: Blake3, _total: u64) -> Result<(), Error> {
        self.kept.insert(hash, Vec::new());
        Ok(())
    }

    fn append(&mut self, hash: &Blake3, bytes: &[u8]) -> Result<(), Error> {
        self.kept.get_mut(hash).unwrap().extend_from_slice(bytes);
        Ok(())
    }

    fn finish(&mut self, hash: &Blake3) -> Result<AssetRange, Error> {
        let bytes: Arc<[u8]> = self.kept.remove(hash).unwrap().into();
        let len = bytes.len() as u64;
        let gone = Some(SetOnDrop(self.gone.clone()));
        let source = Slow { bytes, open: Arc::new(AtomicBool::new(false)), started: self.started.clone(), _gone: gone };
        AssetRange::new(Arc::new(source), 0, len)
    }

    fn discard(&mut self, hash: &Blake3) {
        self.kept.remove(hash);
    }
}

/// A card on the shared canvas showing `hash`.
fn showing(n: u8, hash: Blake3) -> Node {
    let mut card = Node::new(nid(n), "text");
    card.params.insert("image".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    card
}

/// Imports a large asset of noise into `session`.
fn large(session: &mut Session, seed: usize) -> Blake3 {
    let bytes: Arc<[u8]> = noise(nodecraft_sync::WINDOW_BYTES as usize + seed).into();
    let len = bytes.len() as u64;
    let range = AssetRange::new(Arc::new(MemorySource::new(bytes)), 0, len).unwrap();
    session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap()
}

/// A sharer whose one card shows a large asset, and a joiner whose spool holds it, whole and
/// waiting to be sealed: its seal starts, on another thread, and has begun when this returns.
fn sealing_for_a_card(dir: &std::path::Path) -> (Net, Blake3, Arc<AtomicBool>, std::thread::JoinHandle<Result<SealedAsset, Error>>) {
    let mut session = on_disk(empty(), &dir.join("sharer"));
    let hash = large(&mut session, 7);
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: showing(1, hash) }])).unwrap();
    let mut net = Net::new();
    net.disk = Some(dir.join("joiners"));
    net.enter(Person { id: 1, collab: Collab::share(&session, room_id()).unwrap(), session: Some(session) });
    let (spool, started, gone) = SlowSpool::new();
    net.enter(Person { id: 2, collab: Collab::join(room_id()).with_spool(Box::new(spool)).sealing(), session: None });
    let (seal, assets) = start_seal(&mut net, hash, &started);
    let sealing = std::thread::spawn(move || seal.run(&assets));
    wait_started(&started);
    (net, hash, gone, sealing)
}

/// The joiner's next seal, of `hash`, and the store it seals into, once it can begin.
fn start_seal(net: &mut Net, hash: Blake3, started: &AtomicBool) -> (Seal, nodecraft_format::AssetStore) {
    assert!(net.person(2).collab.seals_pending());
    let seal = net.collab(2).next_seal().unwrap();
    assert_eq!(seal.hash(), hash);
    assert!(net.collab(2).next_seal().is_none(), "one at a time");
    assert!(!net.person(2).collab.seals_pending(), "none waits to start while one is under way");
    assert!(!started.load(Ordering::Acquire));
    (seal, net.session(2).assets().clone())
}

/// Waits for the seal to read its asset.
fn wait_started(started: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !started.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline, "the seal never began");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn deleting_the_only_card_that_uses_an_asset_stops_its_seal_and_frees_its_spool() {
    let dir = tempfile::tempdir().unwrap();
    let (mut net, hash, gone, sealing) = sealing_for_a_card(dir.path());

    // The sharer deletes the card while the joiner seals what it shows.
    net.session(1).dispatch("node.remove", &json!({"canvas": canvas(), "id": nid(1)})).unwrap();
    let deleted = Instant::now();
    net.settle();
    let result = sealing.join().unwrap();
    assert!(deleted.elapsed() < Duration::from_secs(1), "{:?}", deleted.elapsed());
    assert!(result.as_ref().is_err_and(TransferCancel::is_cancellation), "{result:?}");
    assert!(gone.load(Ordering::Acquire), "the spool let go of it");

    assert!(net.collab(2).sealed(hash, result), "another may start at once");
    let collab = &net.person(2).collab;
    assert_eq!((collab.waiting_for_assets(), collab.unusable_assets()), (0, 0));
    assert!(!collab.seals_pending());
    assert!(net.collab(2).next_seal().is_none(), "nothing is left to seal");
    assert!(!net.session(2).assets().contains(&hash));
    // The storage is free again at once.
    let later = net.session(2).import_asset(b"after the seal stopped").unwrap();
    assert!(net.session(2).assets().get(&later).is_none(), "on disk, not waiting in memory");
}

#[test]
fn a_joiner_deleting_the_card_that_waits_for_an_asset_lets_it_go_before_it_is_sealed() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), &dir.path().join("sharer"));
    let small = session.import_asset(&noise(100)).unwrap();
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: showing(1, small) }])).unwrap();
    let mut net = Net::new();
    net.disk = Some(dir.path().join("joiners"));
    net.enter(Person { id: 1, collab: Collab::share(&session, room_id()).unwrap(), session: Some(session) });
    let (spool, _, gone) = SlowSpool::new();
    net.enter(Person { id: 2, collab: Collab::join(room_id()).with_spool(Box::new(spool)).sealing(), session: None });
    assert!(net.session(2).document().canvases()[&canvas()].nodes().contains_key(&nid(1)));

    // The sharer shows a large asset on the card; the joiner keeps the card as it was meanwhile.
    let hash = large(net.session(1), 9);
    let changes = vec![Change::RemoveNode { canvas: canvas(), id: nid(1) }, Change::AddNode { canvas: canvas(), node: showing(1, hash) }];
    net.session(1).apply_remote(Changeset::new(changes)).unwrap();
    net.settle();
    assert!(net.person(2).collab.seals_pending());

    // The joiner deletes the card itself before the seal starts: its edit goes out, and the
    // asset is let go rather than sealed.
    net.session(2).dispatch("node.remove", &json!({"canvas": canvas(), "id": nid(1)})).unwrap();
    net.settle();
    assert!(!net.person(2).collab.seals_pending());
    assert!(net.collab(2).next_seal().is_none());
    assert_eq!(net.person(2).collab.waiting_for_assets(), 0);
    assert!(gone.load(Ordering::Acquire), "the spool let go of it");
    assert!(!net.session(1).document().canvases()[&canvas()].nodes().contains_key(&nid(1)), "the sharer took the deletion");
}

#[test]
fn letting_go_of_a_collab_stops_its_seal_under_way() {
    let dir = tempfile::tempdir().unwrap();
    let (mut net, _, gone, sealing) = sealing_for_a_card(dir.path());
    // Still used: the seal goes on through a settle.
    net.settle();
    std::thread::sleep(Duration::from_millis(50));
    assert!(!sealing.is_finished());
    assert!(!gone.load(Ordering::Acquire));

    let stopped = Instant::now();
    drop(net.drop_out(2));
    let result = sealing.join().unwrap();
    assert!(stopped.elapsed() < Duration::from_secs(1), "{:?}", stopped.elapsed());
    assert!(result.is_err_and(|error| TransferCancel::is_cancellation(&error)));
    assert!(gone.load(Ordering::Acquire));
}

#[test]
fn a_seal_stopped_while_its_card_is_still_used_waits_to_start_again() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), &dir.path().join("sharer"));
    let hash = large(&mut session, 11);
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: showing(1, hash) }])).unwrap();
    let mut net = Net::new();
    net.disk = Some(dir.path().join("joiners"));
    net.enter(Person { id: 1, collab: Collab::share(&session, room_id()).unwrap(), session: Some(session) });
    net.enter(Person { id: 2, collab: Collab::join(room_id()).with_spool(Box::new(MemorySpool::new(64 << 20))).sealing(), session: None });
    let seal = net.collab(2).next_seal().unwrap();
    let stopped = TransferCancel::default();
    stopped.cancel();
    let result = net.session(2).assets().clone().seal(&seal.range, &stopped, &Default::default());
    assert!(result.as_ref().is_err_and(TransferCancel::is_cancellation));
    assert!(!net.collab(2).sealed(hash, result));
    assert!(net.person(2).collab.seals_pending());
    // Started again, it seals, and the card comes in.
    let assets = net.session(2).assets().clone();
    let seal = net.collab(2).next_seal().unwrap();
    let result = seal.run(&assets);
    assert!(net.collab(2).sealed(hash, result));
    net.settle();
    assert!(net.session(2).document().canvases()[&canvas()].nodes().contains_key(&nid(1)));
    assert!(net.session(2).assets().contains(&hash));
}
