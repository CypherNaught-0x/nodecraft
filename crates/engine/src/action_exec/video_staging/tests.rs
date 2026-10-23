use super::*;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

struct NeverCancelled;
impl nodecraft_providers::CancelSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn cancelled(&self) -> nodecraft_providers::BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
}

#[derive(Default)]
struct InstantClock(AtomicU64);
impl nodecraft_providers::Clock for InstantClock {
    fn sleep(&self, ms: u64) -> nodecraft_providers::BoxFuture<'static, ()> {
        self.0.fetch_add(ms, Ordering::SeqCst);
        Box::pin(async {})
    }

    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn stage(videos: &Mutex<Videos>, bytes: &[u8]) -> Result<Blake3, String> {
    read_back(
        videos,
        bytes.len() as u64,
        |offset, length| {
            let start = usize::try_from(offset).unwrap();
            Ok(bytes.get(start..start + length).unwrap().to_vec())
        },
        Vec::as_slice,
        &|| false,
    )
}

#[test]
fn an_aggregate_refusal_happens_before_reading_and_keeps_prior_outputs() {
    let videos = Mutex::new(Videos::limited(10, 8));
    let first = stage(&videos, b"12345678").unwrap();
    let reads = AtomicUsize::new(0);
    let error = read_back(
        &videos,
        3,
        |_, _| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(vec![9; 3])
        },
        Vec::as_slice,
        &|| false,
    )
    .unwrap_err();
    assert!(error.contains("staging limit") && error.contains("render fewer nodes"), "{error}");
    assert_eq!(reads.load(Ordering::SeqCst), 0, "admit before allocation or requests");
    assert_eq!(videos.lock().unwrap().get(&first).unwrap().as_ref(), b"12345678");
    stage(&videos, b"90").unwrap();
    assert_eq!(videos.lock().unwrap().retained, 10);
}

#[test]
fn failed_short_and_oversized_reads_release_their_budget() {
    for fault in 0..3 {
        let videos = Mutex::new(Videos::limited(7, 2));
        let prior = stage(&videos, b"old").unwrap();
        let error = read_back(
            &videos,
            4,
            |_, _| match fault {
                0 => Err("authored read failure".into()),
                1 => Ok(Vec::new()),
                _ => Ok(vec![0; 5]),
            },
            Vec::as_slice,
            &|| false,
        )
        .unwrap_err();
        assert!(!error.is_empty());
        {
            let state = videos.lock().unwrap();
            assert_eq!((state.retained, state.reserved, state.pending, state.entries.len()), (3, 0, 0, 1));
            assert!(state.get(&prior).is_some());
        }
        stage(&videos, b"next").unwrap();
    }
}

#[test]
fn cancellation_including_the_final_chunk_releases_unpublished_bytes() {
    for cancel_before_read in [false, true] {
        let videos = Mutex::new(Videos::limited(7, 2));
        let prior = stage(&videos, b"old").unwrap();
        let cancelled = AtomicBool::new(cancel_before_read);
        let reads = AtomicUsize::new(0);
        let error = read_back(
            &videos,
            4,
            |_, _| {
                reads.fetch_add(1, Ordering::SeqCst);
                cancelled.store(true, Ordering::SeqCst);
                Ok(b"next".to_vec())
            },
            Vec::as_slice,
            &|| cancelled.load(Ordering::SeqCst),
        )
        .unwrap_err();
        assert_eq!(error, CANCELLED);
        assert_eq!(reads.load(Ordering::SeqCst), usize::from(!cancel_before_read));
        let state = videos.lock().unwrap();
        assert_eq!((state.retained, state.reserved, state.pending, state.entries.len()), (3, 0, 0, 1));
        assert!(state.get(&prior).is_some());
        drop(state);
        stage(&videos, b"next").unwrap();
    }
}

#[test]
fn duplicates_and_split_rollback_do_not_consume_staging_again() {
    let videos = Mutex::new(Videos::limited(12, 3));
    let prior = stage(&videos, b"old").unwrap();
    let first = stage(&videos, b"part").unwrap();
    assert_eq!(stage(&videos, b"part").unwrap(), first);
    let second = stage(&videos, b"more").unwrap();
    {
        let mut state = videos.lock().unwrap();
        assert_eq!((state.retained, state.reserved, state.pending, state.entries.len()), (11, 0, 0, 3));
        // Split's error path removes only newly produced parts; the reader's prior source stays.
        state.remove(&first).unwrap();
        state.remove(&second).unwrap();
        assert_eq!(state.retained, 3);
        assert_eq!(state.keys().copied().collect::<Vec<_>>(), [prior]);
    }
    stage(&videos, b"replacement").unwrap_err();
    stage(&videos, b"123456789").unwrap();
}

#[test]
fn the_result_count_bounds_tiny_outputs_and_removal_frees_a_slot() {
    let videos = Mutex::new(Videos::limited(100, 2));
    let first = stage(&videos, b"a").unwrap();
    stage(&videos, b"b").unwrap();
    assert!(stage(&videos, b"c").unwrap_err().contains("staging limit"));
    videos.lock().unwrap().remove(&first).unwrap();
    stage(&videos, b"c").unwrap();
}

#[test]
fn invalid_lengths_never_request_chunks_or_take_a_reservation() {
    let videos = Mutex::new(Videos::limited(10, 1));
    for length in [0, super::super::timeline::MAX_RENDER_BYTES.saturating_add(1), u64::MAX] {
        assert!(
            read_back(&videos, length, |_, _| -> Result<Vec<u8>, String> { panic!("an invalid length must not be read") }, Vec::as_slice, &|| false).is_err()
        );
    }
    stage(&videos, b"valid").unwrap();
    assert_eq!(videos.lock().unwrap().pending, 0);
}

#[test]
fn concurrent_reads_reserve_against_the_same_remaining_capacity() {
    use std::sync::mpsc;
    use std::time::Duration;
    let videos = Arc::new(Mutex::new(Videos::limited(8, 3)));
    let (ready_tx, ready_rx) = mpsc::channel();
    let mut releases = Vec::new();
    let mut workers = Vec::new();
    for byte in [1u8, 2] {
        let (release_tx, release_rx) = mpsc::channel();
        releases.push(release_tx);
        let (videos, ready_tx) = (Arc::clone(&videos), ready_tx.clone());
        workers.push(std::thread::spawn(move || {
            read_back(
                &videos,
                4,
                |_, _| {
                    ready_tx.send(()).map_err(|_| "the test ended")?;
                    release_rx.recv_timeout(Duration::from_secs(20)).map_err(|_| "the test ended")?;
                    Ok(vec![byte; 4])
                },
                Vec::as_slice,
                &|| false,
            )
        }));
    }
    for _ in 0..2 {
        ready_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    }
    let third = stage(&videos, b"x");
    for release in releases {
        release.send(()).unwrap();
    }
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    assert!(third.unwrap_err().contains("staging limit"));
    let state = videos.lock().unwrap();
    assert_eq!((state.retained, state.reserved, state.pending, state.entries.len()), (8, 0, 0, 2));
}

#[test]
fn mock_provider_outputs_share_the_same_readback_quota_and_drop_with_the_run() {
    use nodecraft_providers::{JobInput, Output, ProviderJob, RunLimits, mock::MockProvider, run_to_completion};
    let provider = MockProvider::standard();
    let mut outputs = Vec::new();
    for n in 0..3 {
        let job = ProviderJob {
            model: "mock-video".into(),
            params: Default::default(),
            inputs: vec![JobInput::Text { role: "prompt".into(), text: format!("public render fixture {n}") }],
            idempotency_key: format!("staging584-{n}"),
        };
        let done =
            futures::executor::block_on(run_to_completion(&provider, &job, &NeverCancelled, &InstantClock::default(), RunLimits::default(), |_| {})).unwrap();
        let Some(Output::Bytes { bytes, .. }) = done.outputs.into_iter().next() else { panic!("the mock video output is encoded bytes") };
        outputs.push(bytes);
    }
    // Mock video fixtures carry signatures and digests, not decodable frames. This exercises the
    // production staging reader; the opt-in real-tool fixture separately proves Timeline encoding.
    let one = outputs.first().unwrap().len();
    assert!(outputs.iter().all(|bytes| bytes.len() == one));
    let videos = Mutex::new(Videos::limited((one * 2) as u64, 8));
    let first = stage(&videos, outputs.first().unwrap()).unwrap();
    stage(&videos, outputs.get(1).unwrap()).unwrap();
    assert!(stage(&videos, outputs.get(2).unwrap()).unwrap_err().contains("staging limit"));
    let held = Arc::downgrade(videos.lock().unwrap().get(&first).unwrap());
    assert_eq!(videos.lock().unwrap().retained, (one * 2) as u64);
    drop(videos);
    assert!(held.upgrade().is_none(), "the ended run retains none of its staged copies");
    assert_eq!(provider.requests().len(), 3);
}

#[cfg(not(target_arch = "wasm32"))]
mod real;
