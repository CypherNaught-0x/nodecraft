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
    previews.put((hash, 512, 0), Entry::Pending { ticket: 0, reserved: 0 });
    assert!(matches!(previews.request(&ctx, hash, 400.0, || None), Preview::Ready { size: [128, 128], .. }));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_dead_worker_releases_its_pending_previews_and_a_fresh_one_starts() {
    let mut previews = Previews::default();
    let hash = Blake3::from_bytes([6; 32]);
    previews.put((hash, 256, 0), Entry::Pending { ticket: 0, reserved: 0 });
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
    previews.put((hash, 512, 0), Entry::Pending { ticket: 0, reserved: 0 });
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
    previews.put((hash, 512, 0), Entry::Pending { ticket: 0, reserved: 0 });
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

/// A cache that decodes inline (no worker), so one frame's requests finish in that frame.
fn inline_cache() -> Previews {
    let mut previews = Previews::default();
    #[cfg(not(target_arch = "wasm32"))]
    {
        previews.worker_failed = true;
    }
    previews
}

/// One egui pass at `time` seconds: polls the cache first, as the app does, then runs `requests`.
fn frame(ctx: &egui::Context, previews: &mut Previews, time: f64, mut requests: impl FnMut(&egui::Context, &mut Previews)) -> std::time::Duration {
    let mut output = ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
        previews.poll(ui.ctx());
        requests(ui.ctx(), previews);
    });
    output.textures_delta.clear();
    output.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay)
}

#[test]
fn ninety_seven_same_frame_previews_stay_within_the_texture_budget() {
    // Compressible 512×512 images: 1 MiB of texture each, a tiny PNG to read.
    let ctx = egui::Context::default();
    let body = png(512, 512);
    let mut previews = inline_cache();
    let hashes: Vec<_> = (0..97u32).map(|n| Blake3::from_bytes([[200, 1, 2, 3], n.to_le_bytes()].concat().repeat(4).try_into().unwrap())).collect();
    let mut reads = 0;
    let mut full = 0;
    frame(&ctx, &mut previews, 0.0, |ctx, previews| {
        for hash in &hashes {
            if let Preview::Unavailable("Preview cache full") = previews.request(ctx, *hash, 512.0, || {
                reads += 1;
                Some(body.clone())
            }) {
                full += 1;
            }
        }
    });
    assert!(previews.bytes() <= MAX_TEXTURE_BYTES, "same-frame admission overflowed: {} bytes", previews.bytes());
    assert_eq!(full, 1, "the 97th is refused with a truthful placeholder");
    assert_eq!(reads, 96, "a refused preview is never read or decoded");
    // Later frames keep the bound and rotate through the set without exceeding it.
    for step in 1..4 {
        frame(&ctx, &mut previews, f64::from(step), |ctx, previews| {
            for hash in &hashes {
                let _ = previews.request(ctx, *hash, 512.0, || Some(body.clone()));
            }
        });
        assert!(previews.bytes() <= MAX_TEXTURE_BYTES, "{}", previews.bytes());
    }
}

#[test]
fn negative_cache_churn_is_capped_and_idle_failures_expire_without_input() {
    let ctx = egui::Context::default();
    let mut previews = inline_cache();
    let broken = b"\x89PNG\r\n\x1a\nbroken".to_vec();
    // Document churn: 20 frames of 100 never-seen-again broken images each.
    for step in 0..20u32 {
        frame(&ctx, &mut previews, f64::from(step) * 0.1, |ctx, previews| {
            for n in 0..100u32 {
                let hash = Blake3::from_bytes([step.to_le_bytes(), n.to_le_bytes()].concat().repeat(4).try_into().unwrap());
                let _ = previews.request(ctx, hash, 128.0, || Some(broken.clone()));
            }
        });
        assert!(previews.test_pending_and_failed().1 <= MAX_FAILED, "failed metadata grew to {}", previews.test_pending_and_failed().1);
    }
    // Nothing asks for them any more: a repaint is scheduled and, a second later, they're gone.
    let delay = frame(&ctx, &mut previews, 2.0, |_, _| {});
    assert!(delay <= std::time::Duration::from_secs(1), "idle failures schedule their own expiry: {delay:?}");
    let _ = frame(&ctx, &mut previews, 3.5, |_, _| {});
    assert_eq!(previews.test_pending_and_failed().1, 0, "idle failures expire");
    assert!(previews.entries.is_empty());
    // An expired failure is tried again when it is next asked for (its asset may have changed).
    let again = Blake3::from_bytes([19u32.to_le_bytes(), 0u32.to_le_bytes()].concat().repeat(4).try_into().unwrap());
    let mut reads = 0;
    frame(&ctx, &mut previews, 4.0, |ctx, previews| {
        assert!(matches!(
            previews.request(ctx, again, 128.0, || {
                reads += 1;
                Some(png(4, 4))
            }),
            Preview::Ready { .. }
        ));
    });
    assert_eq!(reads, 1);
}

#[test]
fn a_visible_broken_image_is_decoded_once_and_settles_without_repainting_forever() {
    let ctx = egui::Context::default();
    let mut previews = inline_cache();
    let broken = Blake3::from_bytes([77; 32]);
    let mut reads = 0;
    let mut last = std::time::Duration::ZERO;
    // Twenty seconds of a visible broken image, repainted every half second.
    for step in 0..40u32 {
        last = frame(&ctx, &mut previews, f64::from(step) * 0.5, |ctx, previews| {
            let shown = previews.request(ctx, broken, 128.0, || {
                reads += 1;
                Some(b"not an image".to_vec())
            });
            assert!(matches!(shown, Preview::Unavailable("Can't preview this image")));
        });
    }
    assert_eq!(reads, 1, "a visible broken image is never decoded again");
    // Driven only by its own schedule (no input), a stable view stops asking for frames.
    let mut time = 20.0;
    for _ in 0..4 {
        if last == std::time::Duration::MAX {
            break;
        }
        time += last.as_secs_f64().max(0.0);
        last = frame(&ctx, &mut previews, time, |ctx, previews| {
            let _ = previews.request(ctx, broken, 128.0, || {
                reads += 1;
                None
            });
        });
    }
    assert_eq!(last, std::time::Duration::MAX, "finite repaint scheduling");
    assert_eq!(reads, 1);
}

#[test]
fn tiny_ready_previews_cannot_grow_metadata_without_bound() {
    let ctx = egui::Context::default();
    let body = png(1, 1);
    let mut previews = inline_cache();
    for step in 0..12u32 {
        frame(&ctx, &mut previews, f64::from(step), |ctx, previews| {
            for n in 0..200u32 {
                let hash = Blake3::from_bytes([step.to_le_bytes(), n.to_le_bytes()].concat().repeat(4).try_into().unwrap());
                let _ = previews.request(ctx, hash, 128.0, || Some(body.clone()));
            }
        });
        assert!(previews.entries.len() <= MAX_ENTRIES, "{} entries", previews.entries.len());
    }
}

#[test]
fn more_failures_than_the_cap_in_one_frame_are_refused_before_reading() {
    let ctx = egui::Context::default();
    let mut previews = inline_cache();
    let mut reads = 0;
    let mut full = 0;
    frame(&ctx, &mut previews, 0.0, |ctx, previews| {
        for n in 0..(MAX_FAILED as u32 + 10) {
            let hash = Blake3::from_bytes([9, 9, 9, 9].into_iter().chain(n.to_le_bytes()).cycle().take(32).collect::<Vec<_>>().try_into().unwrap());
            if let Preview::Unavailable(CACHE_FULL) = previews.request(ctx, hash, 128.0, || {
                reads += 1;
                Some(b"broken".to_vec())
            }) {
                full += 1;
            }
        }
    });
    assert_eq!((reads, full), (MAX_FAILED, 10), "saturated failure metadata refuses before reading");
    assert_eq!(previews.test_pending_and_failed().1, MAX_FAILED);
}

#[test]
fn a_ready_fallback_is_pinned_before_a_sharper_size_is_admitted() {
    let ctx = egui::Context::default();
    let body = png(512, 512);
    let hash = Blake3::from_bytes([150; 32]);
    let mut previews = inline_cache();
    // The 128 one is the least recently used texture when the cache is full.
    frame(&ctx, &mut previews, 0.0, |ctx, previews| {
        assert!(matches!(previews.request(ctx, hash, 128.0, || Some(body.clone())), Preview::Ready { size: [128, 128], .. }));
    });
    frame(&ctx, &mut previews, 0.1, |ctx, previews| {
        for n in 0..95u32 {
            let other = Blake3::from_bytes([151, 0, 0, 0].into_iter().chain(n.to_le_bytes()).cycle().take(32).collect::<Vec<_>>().try_into().unwrap());
            let _ = previews.request(ctx, other, 512.0, || Some(body.clone()));
        }
    });
    assert_eq!(previews.bytes(), 95 * 1024 * 1024 + 128 * 128 * 4);
    // A frame later, the 95 are no longer on screen (nor the 128 one, until it's asked for).
    frame(&ctx, &mut previews, 0.15, |_, _| {});
    frame(&ctx, &mut previews, 0.2, |ctx, previews| {
        assert!(matches!(previews.request(ctx, hash, 512.0, || Some(body.clone())), Preview::Ready { size: [512, 512], .. }));
    });
    assert!(matches!(previews.entries.get(&(hash, 128, 0)), Some(Entry::Ready { .. })), "the fallback drawn this frame stays");
    assert!(previews.bytes() <= MAX_TEXTURE_BYTES);
}

#[test]
fn pending_reservations_count_against_the_budget_and_missing_data_reserves_nothing() {
    let ctx = egui::Context::default();
    let mut previews = inline_cache();
    let held = Blake3::from_bytes([160; 32]);
    previews.put((held, 2048, 0), Entry::Pending { ticket: 7, reserved: MAX_TEXTURE_BYTES });
    let wanted = Blake3::from_bytes([161; 32]);
    let mut reads = 0;
    frame(&ctx, &mut previews, 0.0, |ctx, previews| {
        let shown = previews.request(ctx, wanted, 128.0, || {
            reads += 1;
            Some(png(4, 4))
        });
        assert!(matches!(shown, Preview::Unavailable(CACHE_FULL)));
    });
    assert_eq!(reads, 0, "refused before reading");
    previews.remove(&(held, 2048, 0));
    assert_eq!((previews.reserved, previews.in_flight), (0, 0));
    frame(&ctx, &mut previews, 0.1, |ctx, previews| {
        // Missing data: admitted, but nothing is reserved or remembered.
        assert!(matches!(previews.request(ctx, wanted, 128.0, || None), Preview::Unavailable("This image's data is missing")));
        assert!(previews.entries.is_empty() && previews.reserved == 0);
        // And once it's back, it previews.
        assert!(matches!(previews.request(ctx, wanted, 128.0, || Some(png(4, 4))), Preview::Ready { .. }));
    });
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_stale_completion_cannot_consume_another_requests_reservation() {
    let ctx = egui::Context::default();
    let mut previews = Previews::default();
    let key = (Blake3::from_bytes([170; 32]), 128, 0);
    previews.put(key, Entry::Pending { ticket: 2, reserved: reservation(128) });
    let decoded = || decode(&png(8, 8), 128, 0);
    // Ticket 1's request is gone; its late result changes nothing.
    previews.complete(&ctx, key, 1, Completion::Finished(decoded()));
    assert!(matches!(previews.entries.get(&key), Some(Entry::Pending { ticket: 2, .. })));
    assert_eq!((previews.reserved, previews.in_flight, previews.bytes), (reservation(128), 1, 0));
    previews.complete(&ctx, key, 2, Completion::Finished(decoded()));
    assert!(matches!(previews.entries.get(&key), Some(Entry::Ready { .. })));
    assert_eq!((previews.reserved, previews.in_flight, previews.bytes), (0, 0, 8 * 8 * 4));
    // A busy answer releases its reservation and holds nothing.
    let busy = (Blake3::from_bytes([171; 32]), 512, 0);
    previews.put(busy, Entry::Pending { ticket: 3, reserved: reservation(512) });
    previews.complete(&ctx, busy, 3, Completion::Busy);
    assert!(!previews.entries.contains_key(&busy));
    assert_eq!((previews.reserved, previews.in_flight), (0, 0));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_real_worker_disconnect_releases_reservations_and_a_fresh_worker_recovers() {
    let ctx = egui::Context::default();
    let mut previews = Previews::default();
    let (entered, ready) = std::sync::mpsc::sync_channel(1);
    let (go, proceed) = std::sync::mpsc::sync_channel(1);
    let blocked = Blake3::from_bytes([180; 32]);
    let queued = Blake3::from_bytes([181; 32]);
    let _ = previews.request_input(&ctx, (blocked, 128, 0), &BUCKETS, || Ok(Input::Panic { entered, proceed }));
    ready.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(matches!(previews.request(&ctx, queued, 512.0, || Some(png(64, 64))), Preview::Pending));
    assert_eq!((previews.in_flight, previews.reserved), (2, reservation(128) + reservation(512)));
    // The thread finishes its current job, skips the queued one and ends: its channel disconnects.
    let worker = previews.worker.as_mut().unwrap();
    worker.cancel();
    worker.close_queue();
    go.send(()).unwrap();
    for _ in 0..400 {
        previews.poll(&ctx);
        if previews.worker.is_none() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(previews.worker.is_none(), "the disconnect was seen");
    assert_eq!(previews.restarts, 1);
    assert_eq!((previews.in_flight, previews.reserved), (0, 0), "reservations rolled back");
    assert!(!previews.entries.values().any(|entry| matches!(entry, Entry::Pending { .. })));
    // The skipped image is asked for again and decodes on a fresh worker.
    let _ = previews.request(&ctx, queued, 512.0, || Some(png(64, 64)));
    assert!(previews.worker.is_some());
    for _ in 0..400 {
        previews.poll(&ctx);
        if previews.in_flight == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(matches!(previews.request(&ctx, queued, 512.0, || None), Preview::Ready { size: [64, 64], .. }));
    assert_eq!((previews.reserved, previews.bytes), (0, 64 * 64 * 4));
}

#[test]
fn expiry_repaints_stay_delayed_even_when_due_within_a_frame() {
    // egui makes a repaint due within `predicted_dt` immediate; kittest's `run` (and a held
    // pointer) would then step through the whole second, turning a click into a long press.
    let ctx = egui::Context::default();
    let mut previews = inline_cache();
    let broken = Blake3::from_bytes([190; 32]);
    for step in 0..24u32 {
        let mut output = ctx.run_ui(egui::RawInput { time: Some(f64::from(step) * 0.25), predicted_dt: 0.25, ..Default::default() }, |ui| {
            previews.poll(ui.ctx());
            let _ = previews.request(ui.ctx(), broken, 128.0, || Some(b"broken".to_vec()));
        });
        output.textures_delta.clear();
        let delay = output.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay);
        assert!(step < 2 || delay > std::time::Duration::ZERO, "frame {step} asked for an immediate repaint");
    }
}

#[test]
fn a_newly_visible_card_drawn_first_does_not_evict_textures_still_on_screen() {
    let ctx = egui::Context::default();
    let body = png(512, 512);
    let mut previews = inline_cache();
    let visible: Vec<_> = (0..96u32)
        .map(|n| Blake3::from_bytes([210, 0, 0, 0].into_iter().chain(n.to_le_bytes()).cycle().take(32).collect::<Vec<_>>().try_into().unwrap()))
        .collect();
    let newcomer = Blake3::from_bytes([211; 32]);
    let draw = |previews: &mut Previews, ctx: &egui::Context, reads: &mut usize| {
        for hash in &visible {
            let _ = previews.request(ctx, *hash, 512.0, || {
                *reads += 1;
                Some(body.clone())
            });
        }
    };
    let mut reads = 0;
    frame(&ctx, &mut previews, 0.0, |ctx, previews| draw(previews, ctx, &mut reads));
    assert_eq!((reads, previews.bytes()), (96, MAX_TEXTURE_BYTES));
    // The new card is drawn before the 96 still on screen: it must not push one of them out.
    for step in 1..4u32 {
        reads = 0;
        let mut shown = None;
        frame(&ctx, &mut previews, f64::from(step), |ctx, previews| {
            shown = Some(previews.request(ctx, newcomer, 512.0, || Some(body.clone())));
            draw(previews, ctx, &mut reads);
        });
        assert!(matches!(shown, Some(Preview::Unavailable(CACHE_FULL))));
        assert_eq!(reads, 0, "frame {step}: a texture still on screen was evicted and decoded again");
    }
    // Frames 1 and 3 asked for a retry; frame 2 was frame 1's retry and, with nothing changed and
    // no input, asked for nothing. Frame 4 is frame 3's retry: a stable view stops there.
    let stable = |previews: &mut Previews, ctx: &egui::Context, reads: &mut usize| {
        let _ = previews.request(ctx, newcomer, 512.0, || Some(body.clone()));
        draw(previews, ctx, reads);
    };
    assert_eq!(frame(&ctx, &mut previews, 4.0, |ctx, previews| stable(previews, ctx, &mut reads)), std::time::Duration::MAX, "finite retries");
    let next_frame = std::time::Duration::from_millis(5);
    assert!(frame(&ctx, &mut previews, 5.0, |ctx, previews| stable(previews, ctx, &mut reads)) < next_frame);
    // Frame 6 is frame 5's retry, but input moved the view: the old cards were drawn last frame,
    // so it refuses and asks for one more frame, in which they are evictable.
    let mut output =
        ctx.run_ui(egui::RawInput { time: Some(6.0), events: vec![egui::Event::PointerMoved(egui::pos2(1.0, 1.0))], ..Default::default() }, |ui| {
            previews.poll(ui.ctx());
            assert!(matches!(previews.request(ui.ctx(), newcomer, 512.0, || Some(body.clone())), Preview::Unavailable(CACHE_FULL)));
        });
    output.textures_delta.clear();
    assert!(output.viewport_output.get(&egui::ViewportId::ROOT).is_some_and(|v| v.repaint_delay < next_frame));
    frame(&ctx, &mut previews, 6.1, |ctx, previews| {
        assert!(matches!(previews.request(ctx, newcomer, 512.0, || Some(body.clone())), Preview::Ready { size: [512, 512], .. }));
    });
    assert_eq!(reads, 0);
}
