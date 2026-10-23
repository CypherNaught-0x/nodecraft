use super::*;
use crate::action_exec::video_staging::{Provisional, Videos};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

fn actions(limit: u64) -> LocalActions {
    let mut actions = LocalActions::new(AssetStore::default());
    actions.videos = Mutex::new(Videos::limited(limit, 8));
    actions
}

fn value(hash: nodecraft_types::Blake3) -> Value {
    Value::Media(AssetRef { hash, mime: "video/webm".parse().unwrap(), meta: MediaMeta::default() })
}

fn pending<'a>(actions: &'a LocalActions, bytes: &[u8]) -> (Value, Provisional<'a>) {
    let held = actions.retain_video_pending(bytes.len() as u64, |_, _| Ok(bytes.to_vec()), Vec::as_slice, &|| false).unwrap();
    (value(held.hash()), held)
}

fn accepted(actions: &LocalActions, bytes: &[u8]) -> Value {
    value(actions.retain_video(bytes.len() as u64, |_, _| Ok(bytes.to_vec()), Vec::as_slice, &|| false).unwrap())
}

#[test]
fn rollback806_later_successful_duplicate_survives_split_failure_or_cancel_for_host_and_chain() {
    for cancel in [false, true] {
        let actions = actions(12);
        let cancelled = AtomicBool::new(false);
        let mut successful = None;
        let error = render_parts(&[(0, 1), (1, 2)], &|| cancelled.load(Ordering::SeqCst), |n, _, _| {
            if n == 2 {
                return Err("authored later part failure".into());
            }
            let part = pending(&actions, b"part");
            assert!(actions.staged_videos().is_empty(), "the incomplete split is not host-visible");
            // A real sibling thread publishes identical content while the split holds its receipt.
            successful = Some(std::thread::scope(|scope| scope.spawn(|| accepted(&actions, b"part")).join().unwrap()));
            cancelled.store(cancel, Ordering::SeqCst);
            Ok(part)
        })
        .unwrap_err();
        assert!(error.contains(if cancel { "cancelled" } else { "later part failure" }));
        let successful = successful.unwrap();
        assert_eq!(actions.video_bytes(Some(&successful), false, "missing").unwrap().as_ref(), b"part", "the actual chain reader still resolves it");
        assert_eq!(actions.staged_videos().len(), 1, "the host still imports the successful output");
        accepted(&actions, b"12345678");
        assert_eq!(actions.staged_videos().len(), 2, "the duplicate did not charge bytes twice");
    }
}

#[test]
fn rollback806_failed_or_cancelled_split_without_a_consumer_releases_its_parts_and_quota() {
    for cancel in [false, true] {
        let actions = actions(8);
        let cancelled = AtomicBool::new(false);
        let mut first = None;
        let error = render_parts(&[(0, 1), (1, 2)], &|| cancelled.load(Ordering::SeqCst), |n, _, _| {
            if n == 2 {
                return Err("authored later part failure".into());
            }
            let part = pending(&actions, b"part");
            first = Some(part.0.clone());
            cancelled.store(cancel, Ordering::SeqCst);
            Ok(part)
        })
        .unwrap_err();
        assert!(error.contains(if cancel { "cancelled" } else { "later part failure" }));
        assert!(actions.staged_videos().is_empty());
        assert!(actions.video_bytes(first.as_ref(), false, "missing").is_err());
        accepted(&actions, b"new-full");
        assert_eq!(actions.staged_videos().len(), 1, "all eight bytes are available again");
    }
}

#[test]
fn rollback806_a_successful_split_shares_a_failed_siblings_part() {
    let actions = actions(12);
    let mut successful = None;
    assert!(
        render_parts(&[(0, 1), (1, 2)], &|| false, |n, _, _| {
            if n == 2 {
                return Err("later failure".into());
            }
            let part = pending(&actions, b"part");
            successful = Some(render_parts(&[(0, 1)], &|| false, |_, _, _| Ok(pending(&actions, b"part"))).unwrap());
            Ok(part)
        })
        .is_err()
    );
    let Value::List(parts) = successful.unwrap() else { panic!("split parts") };
    assert_eq!(actions.video_bytes(parts.first(), false, "missing").unwrap().as_ref(), b"part");
    assert_eq!(actions.staged_videos().len(), 1);
    accepted(&actions, b"12345678");
}

#[test]
fn rollback806_a_failed_split_cannot_remove_another_unpublished_split_part() {
    let actions = actions(8);
    let output = render_parts(&[(0, 1)], &|| false, |_, _, _| {
        let held = pending(&actions, b"part");
        assert!(
            render_parts(&[(0, 1), (1, 2)], &|| false, |n, _, _| { if n == 2 { Err("later failure".into()) } else { Ok(pending(&actions, b"part")) } })
                .is_err()
        );
        assert!(actions.staged_videos().is_empty(), "the outer split is still incomplete");
        Ok(held)
    })
    .unwrap();
    let Value::List(parts) = output else { panic!("split parts") };
    assert_eq!(actions.video_bytes(parts.first(), false, "missing").unwrap().as_ref(), b"part");
}

#[test]
fn rollback806_success_publishes_every_part_and_last_moment_cancel_publishes_none() {
    let actions = actions(12);
    let output = render_parts(&[(0, 1), (1, 2)], &|| false, |n, _, _| {
        assert!(actions.staged_videos().is_empty(), "publication waits for the whole result");
        Ok(pending(&actions, if n == 1 { b"part" } else { b"more" }))
    })
    .unwrap();
    let Value::List(parts) = output else { panic!("split parts") };
    assert_eq!(actions.staged_videos().len(), 2);
    for part in &parts {
        assert_eq!(actions.video_bytes(Some(part), false, "missing").unwrap().len(), 4);
    }

    let cancelled = AtomicBool::new(false);
    assert!(
        render_parts(&[(0, 1)], &|| cancelled.load(Ordering::SeqCst), |_, _, _| {
            let part = pending(&actions, b"last");
            cancelled.store(true, Ordering::SeqCst);
            Ok(part)
        })
        .unwrap_err()
        .contains("cancelled")
    );
    assert_eq!(actions.staged_videos().len(), 2, "the last unpublished part was reclaimed");
    accepted(&actions, b"next");
}

#[test]
#[ignore = "actual ffmpeg Split/Extract fixture with owned service cleanup"]
fn rollback806_real_split_and_extract_keep_their_published_results() {
    use crate::action_exec::timeline::native::{detach_test_service, service};
    use std::sync::Arc;
    use std::time::Duration;
    let tools = (
        std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFMPEG").unwrap()),
        std::path::PathBuf::from(std::env::var_os("NODECRAFT_VIDEO_TEST_FFPROBE").unwrap()),
    );
    let open = service(&tools, &|| false).unwrap();
    let outcome = std::panic::catch_unwind(super::split_parts_add_up_and_extract_writes_the_sound);
    detach_test_service(&open);
    let cleanup = match Arc::try_unwrap(open) {
        Ok(own) => {
            let result = own.begin_shutdown().wait_blocking(Duration::from_secs(30));
            if result.is_ok() {
                eprintln!("ROLLBACK806_CLEANUP: owned service cleaned up");
            }
            result.map(|_| ())
        }
        Err(shared) => {
            drop(shared);
            eprintln!("ROLLBACK806_CLEANUP: service shared; normal lifetime retained");
            Ok(())
        }
    };
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
    assert!(cleanup.is_ok(), "owned test service did not clean up");
    eprintln!("ROLLBACK806_REAL: split durations/audio and extracted sound passed");
}
