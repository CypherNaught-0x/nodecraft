use super::*;

fn png(w: u32, h: u32) -> Vec<u8> {
    nodecraft_media::encode_png(w, h, &vec![200; (w * h * 4) as usize], &Limits::default()).unwrap()
}

#[test]
fn buckets_round_up_and_cap() {
    assert_eq!((bucket(0.0), bucket(100.0), bucket(129.0), bucket(300.0), bucket(5000.0)), (128, 128, 256, 512, 512));
    assert_eq!((bucket(f32::NAN), bucket(-3.0), bucket(f32::INFINITY)), (128, 128, 128));
}

#[test]
fn decoded_previews_keep_the_aspect_and_broken_ones_are_remembered() {
    let ctx = egui::Context::default();
    let hash = Blake3::from_bytes([1; 32]);
    let previews = Previews::inline(&ctx, hash, 128, &png(400, 200));
    let mut previews = previews;
    match previews.request(&ctx, hash, 100.0, || None) {
        Preview::Ready { size, .. } => assert_eq!(size, [128, 64]),
        _ => panic!("ready"),
    }
    assert_eq!(previews.bytes(), 128 * 64 * 4);
    // A corrupt asset: no worker in this cache, so it decodes inline, fails once, and stays failed.
    let broken = Blake3::from_bytes([2; 32]);
    let mut reads = 0;
    for _ in 0..3 {
        let preview = previews.request(&ctx, broken, 100.0, || {
            reads += 1;
            Some(b"\\x89PNG\\r\\n\\x1a\\nbroken".to_vec())
        });
        assert!(matches!(preview, Preview::Unavailable("Can't preview this image")));
    }
    assert_eq!(reads, 1, "a failure isn't decoded again every frame");
    let missing = Blake3::from_bytes([3; 32]);
    assert!(matches!(previews.request(&ctx, missing, 100.0, || None), Preview::Unavailable("This image's data is missing")));
    // Missing data isn't remembered: once the asset is back, it previews.
    assert!(matches!(previews.request(&ctx, missing, 100.0, || Some(png(4, 4))), Preview::Ready { .. }));
}

#[test]
fn a_smaller_preview_shows_while_a_sharper_one_is_wanted_and_eviction_keeps_the_budget() {
    let ctx = egui::Context::default();
    let hash = Blake3::from_bytes([4; 32]);
    let mut previews = Previews::inline(&ctx, hash, 128, &png(256, 256));
    // Asking for 512 decodes inline here; the 128 one is still in the cache and usable.
    assert!(matches!(previews.request(&ctx, hash, 400.0, || Some(png(256, 256))), Preview::Ready { size: [256, 256], .. }));
    // Fill past the budget with 512×512 previews (1 MiB each): the oldest go, one per frame's clock.
    for n in 0..120u8 {
        previews.poll(&ctx);
        let h = Blake3::from_bytes([n.wrapping_add(10); 32]);
        let _ = previews.request(&ctx, h, 512.0, || Some(png(512, 512)));
    }
    assert!(previews.bytes() <= MAX_TEXTURE_BYTES, "{}", previews.bytes());
}

#[test]
fn a_smaller_ready_preview_stands_in_while_the_wanted_one_is_pending() {
    let ctx = egui::Context::default();
    let hash = Blake3::from_bytes([5; 32]);
    let mut previews = Previews::inline(&ctx, hash, 128, &png(300, 300));
    previews.entries.insert((hash, 512, 0), Entry::Pending);
    assert!(matches!(previews.request(&ctx, hash, 400.0, || None), Preview::Ready { size: [128, 128], .. }));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_dead_worker_releases_its_pending_previews_and_a_fresh_one_starts() {
    let mut previews = Previews::default();
    let hash = Blake3::from_bytes([6; 32]);
    previews.entries.insert((hash, 256, 0), Entry::Pending);
    previews.in_flight = MAX_IN_FLIGHT;
    previews.worker_died();
    assert_eq!(previews.in_flight, 0, "the in-flight cap doesn't stay full");
    assert!(previews.entries.is_empty(), "its images are asked for again");
    assert!(!previews.worker_failed, "a fresh worker may start");
    for _ in 0..MAX_RESTARTS {
        previews.worker_died();
    }
    assert!(previews.worker_failed, "but not forever");
    // A panic inside a decode is a failure, not a dead worker.
    assert!(decode(&[], 128, 0).is_err());
}

#[test]
fn shutting_down_skips_queued_decodes() {
    let ctx = egui::Context::default();
    let mut previews = Previews::default();
    for n in 0..MAX_IN_FLIGHT as u8 {
        let _ = previews.request(&ctx, Blake3::from_bytes([n + 20; 32]), 512.0, || Some(png(2048, 2048)));
    }
    let started = std::time::Instant::now();
    drop(previews);
    // At most the one decode in progress finishes; a 2048² decode takes well under this.
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn resident_asset_capabilities_keep_inline_preview_support() {
    let ctx = egui::Context::default();
    let mut store = nodecraft_format::AssetStore::default();
    let body = png(8, 6);
    let hash = store.insert(&body).unwrap();
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    let mut previews = Previews::default();
    #[cfg(not(target_arch = "wasm32"))]
    {
        previews.worker_failed = true;
    }
    assert!(matches!(previews.request_asset(&ctx, hash, 128.0, || store.preview_read(&asset)), Preview::Ready { size: [8, 6], .. }));
    assert!(matches!(previews.request_asset(&ctx, hash, 128.0, || panic!("a Ready hit must not reacquire")), Preview::Ready { .. }));
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn lazy_asset_without_worker_is_unsupported_rather_than_missing() {
    let root = std::env::temp_dir().join(format!("nodecraft-preview348-inline-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let workspace = nodecraft_format::native_assets::NativeAssetWorkspace::new(&root, Default::default()).unwrap();
    let context = nodecraft_format::AssetContext::native(workspace);
    let mut store = nodecraft_format::AssetStore::with_context(context);
    let hash = store.insert(&png(8, 6)).unwrap();
    assert!(store.get(&hash).is_none());
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    let before = store.context().storage_usage().unwrap();
    let ctx = egui::Context::default();
    let mut previews = Previews { worker_failed: true, ..Default::default() };
    assert!(matches!(previews.request_asset(&ctx, hash, 128.0, || store.preview_read(&asset)), Preview::Unavailable("Preview not available here")));
    assert_eq!(store.context().storage_usage().unwrap(), before);
    assert_eq!(previews.bytes(), 0);
    drop((previews, store));
    std::fs::remove_dir_all(root).unwrap();
}

pub(super) fn panic_attempt(entered: &std::sync::mpsc::SyncSender<()>, proceed: &std::sync::mpsc::Receiver<()>) -> ! {
    assert_eq!(std::thread::current().name(), Some("nodecraft-previews"));
    entered.send(()).unwrap();
    proceed.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    panic!("injected whole-attempt guard control, not a corrupt-file exploit");
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_worker_contains_an_injected_attempt_panic_and_accepts_the_next_job() {
    let ctx = egui::Context::default();
    let mut previews = Previews::default();
    let (entered, ready) = std::sync::mpsc::sync_channel(1);
    let (go, proceed) = std::sync::mpsc::sync_channel(1);
    let hash = Blake3::from_bytes([60; 32]);
    let _ = previews.request_input(&ctx, (hash, 128, 0), &BUCKETS, || Ok(Input::Panic { entered, proceed }));
    ready.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    go.send(()).unwrap();
    for _ in 0..200 {
        previews.poll(&ctx);
        if previews.in_flight == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(previews.request(&ctx, hash, 128.0, || None), Preview::Unavailable("Can't preview this image")));
    assert_eq!(previews.restarts, 0);
    let hash = Blake3::from_bytes([61; 32]);
    let _ = previews.request(&ctx, hash, 128.0, || Some(png(8, 6)));
    for _ in 0..200 {
        previews.poll(&ctx);
        if previews.in_flight == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(previews.request(&ctx, hash, 128.0, || None), Preview::Ready { .. }));
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn cancellation_drops_queued_native_capabilities_without_decoding_or_leaking_pins() {
    let root = std::env::temp_dir().join(format!("nodecraft-preview348-queue-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let workspace = nodecraft_format::native_assets::NativeAssetWorkspace::new(&root, Default::default()).unwrap();
    let context = nodecraft_format::AssetContext::native(workspace);
    let mut store = nodecraft_format::AssetStore::with_context(context.clone());
    let ctx = egui::Context::default();
    let mut previews = Previews::default();
    let (entered, ready) = std::sync::mpsc::sync_channel(1);
    let (go, proceed) = std::sync::mpsc::sync_channel(1);
    let _ = previews.request_input(&ctx, (Blake3::from_bytes([62; 32]), 128, 0), &BUCKETS, || Ok(Input::Panic { entered, proceed }));
    ready.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    for width in 9..12 {
        let hash = store.insert(&png(width, 6)).unwrap();
        let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
        let _ = previews.request_asset(&ctx, hash, 128.0, || store.preview_read(&asset));
    }
    assert_eq!(previews.in_flight, MAX_IN_FLIGHT);
    drop(store);
    assert_eq!(context.retained_usage().unwrap().0, 3);
    previews.worker.as_ref().unwrap().cancel();
    go.send(()).unwrap();
    drop(previews);
    assert_eq!(context.retained_usage().unwrap().0, 0);
    nodecraft_format::AssetStore::with_context(context.clone()).collect_garbage().unwrap();
    assert_eq!(context.storage_usage().unwrap().verified, 0);
    drop(context);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn source_dimensions_survive_inline_thumbnail_sizes_and_ready_hits_without_reads() {
    let ctx = egui::Context::default();
    let bytes = png(1024, 768);
    let hash = Blake3::from_bytes([91; 32]);
    for (edge, thumbnail) in [(128, [128, 96]), (512, [512, 384])] {
        let mut previews = Previews::inline(&ctx, hash, edge, &bytes);
        let mut reads = 0;
        match previews.request(&ctx, hash, edge as f32, || {
            reads += 1;
            Some(bytes.clone())
        }) {
            Preview::Ready { size, source_size, .. } => {
                assert_eq!(size, thumbnail);
                assert_eq!(source_size, [1024, 768]);
            }
            _ => panic!("ready"),
        }
        assert_eq!(reads, 0, "Ready hits do not read or probe again");
        assert_eq!(previews.bytes(), thumbnail[0] as usize * thumbnail[1] as usize * 4);
    }
}

#[test]
fn smaller_pending_fallback_retains_source_dimensions_without_another_read() {
    let ctx = egui::Context::default();
    let hash = Blake3::from_bytes([92; 32]);
    let mut previews = Previews::inline(&ctx, hash, 128, &png(1024, 768));
    previews.entries.insert((hash, 512, 0), Entry::Pending);
    let mut reads = 0;
    let shown = previews.request(&ctx, hash, 512.0, || {
        reads += 1;
        None
    });
    assert!(matches!(shown, Preview::Ready { size: [128, 96], source_size: [1024, 768], .. }));
    assert_eq!(reads, 0);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn native_worker_keeps_decoded_source_dimensions_in_ready_and_fallback() {
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("nodecraft-native-dimensions-{}-{unique}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let workspace = nodecraft_format::native_assets::NativeAssetWorkspace::new(&root, Default::default()).unwrap();
    let mut store = nodecraft_format::AssetStore::with_context(nodecraft_format::AssetContext::native(workspace));
    let hash = store.insert(&png(1024, 768)).unwrap();
    assert!(store.get(&hash).is_none(), "exercise nonresident native capability, not a resident shortcut");
    let asset = nodecraft_types::AssetRef { hash, mime: "image/png".parse().unwrap(), meta: Default::default() };
    let ctx = egui::Context::default();
    let mut previews = Previews::default();
    let _ = previews.request_asset(&ctx, hash, 128.0, || store.preview_read(&asset));
    for _ in 0..200 {
        previews.poll(&ctx);
        if previews.in_flight == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(previews.in_flight, 0);
    let ready = previews.request_asset(&ctx, hash, 128.0, || panic!("Ready must not capture or reread"));
    assert!(matches!(ready, Preview::Ready { size: [128, 96], source_size: [1024, 768], .. }));
    previews.entries.insert((hash, 512, 0), Entry::Pending);
    let fallback = previews.request_asset(&ctx, hash, 512.0, || panic!("Pending fallback must not reread"));
    assert!(matches!(fallback, Preview::Ready { size: [128, 96], source_size: [1024, 768], .. }));
    drop((previews, store));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_pdf_previews_as_its_first_page_and_a_broken_one_says_so() {
    let ctx = egui::Context::default();
    let pdf = nodecraft_media::pdf_fixture::pdf(&["BT /F1 12 Tf 20 150 Td (Hello) Tj ET", ""]);
    let hash = Blake3::from_bytes([6; 32]);
    let mut previews = Previews::inline(&ctx, hash, 128, &pdf);
    // The fixture's pages are 300 × 200 points.
    assert!(matches!(previews.request(&ctx, hash, 100.0, || None), Preview::Ready { size: [128, 85], .. }));
    let broken = Blake3::from_bytes([7; 32]);
    assert!(matches!(previews.request(&ctx, broken, 100.0, || Some(b"%PDF-1.7 broken".to_vec())), Preview::Unavailable("Can't preview this PDF")));
}
