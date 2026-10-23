use super::*;
use std::sync::mpsc::{channel, sync_channel};

struct Device(Arc<AtomicBool>);
impl Drop for Device {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

#[test]
fn successful_failed_and_panicking_captures_drop_the_device_before_retiring() {
    for failure in [0, 1, 2] {
        let captures = Arc::new(Captures::default());
        let group = Arc::clone(&captures);
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&dropped);
        let worker = std::thread::spawn(move || {
            group.with(&|| false, |slot, _| {
                *slot = Some(Device(flag));
                match failure {
                    0 => Ok(()),
                    1 => Err("authored capture failure".into()),
                    _ => panic!("authored capture panic"),
                }
            })
        });
        let result = worker.join();
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(captures.shutdown(Duration::ZERO), Shutdown { remaining: 0, panicked: usize::from(failure == 2) });
        if failure == 0 {
            assert!(result.unwrap().is_ok());
        } else if failure == 1 {
            assert!(result.unwrap().is_err());
        } else {
            assert!(result.is_err());
        }
    }
}

#[test]
fn close_cancels_every_origin_and_queued_capture_and_gives_up_at_its_bound() {
    let captures = Arc::new(Captures::default());
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = channel();
    let group = Arc::clone(&captures);
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&dropped);
    let live_worker = std::thread::spawn(move || {
        group.with(&|| false, |slot, stopping| {
            *slot = Some(Device(flag));
            entered_tx.send(()).unwrap();
            // Models a driver which never returns until the test releases it, beyond close's bound.
            release_rx.recv().unwrap();
            assert!(stopping(), "close requests cancellation before the next sheet view");
            Ok(())
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let group = Arc::clone(&captures);
    let run_worker = std::thread::spawn(move || group.with::<Device, ()>(&|| false, |_, _| panic!("a queued explicit-run capture must not start after close")));
    let deadline = Instant::now() + Duration::from_secs(2);
    while captures.state.lock().unwrap().active != 2 {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    let group = Arc::clone(&captures);
    let (closed_tx, closed_rx) = sync_channel(1);
    let closer = std::thread::spawn(move || closed_tx.send(group.shutdown(Duration::from_millis(20))).unwrap());
    let report = closed_rx.recv_timeout(Duration::from_secs(2));
    // Always release and join the authored stuck worker, even if close failed its bound.
    let admission = report.as_ref().ok().map(|_| captures.with::<Device, ()>(&|| false, |_, _| Ok(())));
    release_tx.send(()).unwrap();
    live_worker.join().unwrap().unwrap();
    assert!(run_worker.join().unwrap().is_err());
    closer.join().unwrap();
    assert_eq!(report.unwrap(), Shutdown { remaining: 2, panicked: 0 });
    assert!(admission.unwrap().is_err(), "a timed-out drain keeps admission closed until its workers actually retire");
    assert!(dropped.load(Ordering::Acquire));
    assert!(captures.with::<Device, _>(&|| false, |_, _| Ok(())).is_ok(), "new owners may work after the old capture actually retires");
}

#[test]
fn device_teardown_is_still_counted_after_the_capture_body_returns() {
    struct SlowDevice(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
    impl Drop for SlowDevice {
        fn drop(&mut self) {
            self.0.send(()).unwrap();
            self.1.recv().unwrap();
        }
    }
    let captures = Arc::new(Captures::default());
    let group = Arc::clone(&captures);
    let (dropping_tx, dropping_rx) = channel();
    let (release_tx, release_rx) = channel();
    let worker = std::thread::spawn(move || {
        group.with(&|| false, |slot, _| {
            *slot = Some(SlowDevice(dropping_tx, release_rx));
            Ok(())
        })
    });
    dropping_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(captures.shutdown(Duration::ZERO).remaining, 1, "close must not report success while device teardown is still blocked");
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert_eq!(captures.shutdown(Duration::ZERO), Shutdown::default());
}

#[test]
fn one_owner_cancelling_does_not_close_another_owner() {
    let captures = Captures::default();
    assert!(captures.with::<Device, ()>(&|| true, |_, _| panic!("cancelled owner must not acquire the GPU")).is_err());
    assert!(
        captures
            .with::<Device, _>(&|| false, |_, stopping| {
                assert!(!stopping());
                Ok(())
            })
            .is_ok()
    );
}
