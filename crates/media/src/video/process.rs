//! Direct-child supervisor. Pipe workers never own cancellation or child wait.
use super::{Result, VideoError};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
type Writer = Box<dyn FnOnce(&mut dyn Write) -> Result<()> + Send>;
pub(super) struct Input {
    pub write: Writer,
    pub stop: Arc<AtomicBool>,
}
#[derive(Default)]
pub(super) struct Counters {
    pub spawned: std::sync::atomic::AtomicUsize,
    pub captured: std::sync::atomic::AtomicUsize,
}
fn increment(value: &std::sync::atomic::AtomicUsize, n: usize) {
    let mut old = value.load(Ordering::Relaxed);
    loop {
        match value.compare_exchange_weak(old, old.saturating_add(n), Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(now) => old = now,
        }
    }
}
pub(super) struct Reply {
    pub bytes: Vec<u8>,
}
fn drain(mut read: impl Read, cap: usize, refusal: Option<Arc<AtomicBool>>, counters: Option<Arc<Counters>>) -> Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    out.try_reserve_exact(cap).map_err(|_| VideoError::Limit)?;
    if out.capacity() != cap {
        return Err(VideoError::Limit);
    }
    if let Some(c) = counters {
        increment(&c.captured, out.capacity());
    }
    let mut buf = [0u8; 16384];
    let mut overflow = false;
    loop {
        let n = read.read(&mut buf).map_err(|_| VideoError::Io)?;
        if n == 0 {
            break;
        }
        let left = cap.saturating_sub(out.len());
        let keep = n.min(left);
        out.extend_from_slice(buf.get(..keep).ok_or(VideoError::Io)?);
        overflow |= keep < n;
        if overflow && let Some(flag) = &refusal {
            flag.store(true, Ordering::Release);
        }
    }
    Ok((out, overflow))
}
pub(super) struct Watchdog<'a> {
    pub cancel: &'a Arc<AtomicBool>,
    pub closed: &'a Arc<AtomicBool>,
    pub deadline: Instant,
    pub counters: Option<Arc<Counters>>,
}
pub(super) fn run(
    program: &Path,
    args: &[OsString],
    cwd: &Path,
    input: Option<Input>,
    cap: usize,
    watchdog: Watchdog<'_>,
    tick: &mut dyn FnMut(),
) -> Result<Reply> {
    let Watchdog { cancel, closed, deadline, counters } = watchdog;
    if cancel.load(Ordering::Acquire) || closed.load(Ordering::Acquire) {
        return Err(VideoError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(VideoError::Timeout);
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("HOME", cwd)
        .env("TMPDIR", cwd)
        .env("TEMP", cwd)
        .env("TMP", cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name)
            && value.len() <= 4096
        {
            command.env(name, value);
        }
    }
    command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let refused = Arc::new(AtomicBool::new(false));
    let output_refused = Arc::clone(&refused);
    let writer_stop = input.as_ref().map(|i| Arc::clone(&i.stop));
    let mut child = command.spawn().map_err(|_| VideoError::Unavailable)?;
    if let Some(c) = &counters {
        increment(&c.spawned, 1);
    }
    let workers = (|| {
        let stdout = child.stdout.take().ok_or(VideoError::Process)?;
        let stderr = child.stderr.take().ok_or(VideoError::Process)?;
        let out = thread::Builder::new()
            .name("video-output".into())
            .spawn(move || drain(stdout, cap, Some(output_refused), counters))
            .map_err(|_| VideoError::Unavailable)?;
        let err = match thread::Builder::new().name("video-errors".into()).spawn(move || drain(stderr, 16384, None, None)) {
            Ok(h) => h,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                return Err(VideoError::Unavailable);
            }
        };
        let writer = if let Some(write) = input {
            let Some(mut stdin) = child.stdin.take() else {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                return Err(VideoError::Process);
            };
            match thread::Builder::new().name("video-input".into()).spawn(move || (write.write)(&mut stdin)) {
                Ok(h) => Some(h),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out.join();
                    let _ = err.join();
                    return Err(VideoError::Unavailable);
                }
            }
        } else {
            None
        };
        Ok((out, err, writer))
    })();
    let (out, err, writer) = match workers {
        Ok(w) => w,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let mut failure = None;
    let status = loop {
        tick();
        if cancel.load(Ordering::Acquire) || closed.load(Ordering::Acquire) {
            failure = Some(VideoError::Cancelled);
        } else if refused.load(Ordering::Acquire) {
            failure = Some(VideoError::Limit);
        } else if Instant::now() >= deadline {
            failure = Some(VideoError::Timeout);
        }
        if failure.is_some() {
            let _ = child.kill();
        }
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => {
                failure = Some(VideoError::Process);
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
        thread::sleep(Duration::from_millis(10));
    };
    if let Some(stop) = writer_stop {
        stop.store(true, Ordering::Release);
    }
    // Supported tools do not leave descendants holding these pipes. Reap precedes joining.
    let joined = join_workers(out, err, writer)?;
    if let Some(e) = failure {
        return Err(e);
    }
    if !status.is_some_and(|s| s.success()) {
        return Err(VideoError::Process);
    }
    joined.written?;
    let (bytes, overflow) = joined.stdout?;
    if overflow {
        return Err(VideoError::Limit);
    }
    Ok(Reply { bytes })
}
type DrainResult = Result<(Vec<u8>, bool)>;
struct JoinedWorkers {
    stdout: DrainResult,
    written: Result<()>,
}
fn join_workers(
    out: thread::JoinHandle<DrainResult>,
    err: thread::JoinHandle<DrainResult>,
    writer: Option<thread::JoinHandle<Result<()>>>,
) -> Result<JoinedWorkers> {
    // Collect ALL acknowledgements before inspecting any error or panic.
    let stdout = out.join();
    let stderr = err.join();
    let written = writer.map(|handle| handle.join());
    let stdout = stdout.map_err(|_| VideoError::Process)?;
    let _stderr = stderr.map_err(|_| VideoError::Process)??;
    let written = written.transpose().map_err(|_| VideoError::Process)?.unwrap_or(Ok(()));
    Ok(JoinedWorkers { stdout, written })
}
pub(super) fn args(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The file a fixture's working directory holds when a test launched it. `run` clears the
    /// environment, so this is how a fixture tells it was launched rather than picked up by
    /// `cargo test -- --ignored`, where it returns at once.
    const LAUNCHED: &str = "nodecraft-process-fixture";
    fn launched() -> bool {
        Path::new(LAUNCHED).is_file()
    }
    fn fixture(name: &str, input: Option<Input>, cap: usize, cancel: Arc<AtomicBool>) -> Result<Reply> {
        let exe = std::env::current_exe().unwrap();
        let dir = std::env::temp_dir().join(format!("{LAUNCHED}-{}-{}", std::process::id(), name.rsplit("::").next().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(LAUNCHED), b"").unwrap();
        let reply = run(
            &exe,
            &args(&["--ignored", "--exact", name, "--nocapture"]),
            &dir,
            input,
            cap,
            Watchdog { cancel: &cancel, closed: &Arc::new(AtomicBool::new(false)), deadline: Instant::now() + Duration::from_millis(250), counters: None },
            &mut || {},
        );
        let _ = std::fs::remove_dir_all(&dir);
        reply
    }
    #[test]
    #[ignore = "subprocess fixture, never run directly"]
    fn child_flood() {
        if !launched() {
            return;
        }
        let buf = [b'x'; 65536];
        let mut out = std::io::stdout().lock();
        while out.write_all(&buf).is_ok() {}
    }
    #[test]
    #[ignore = "subprocess fixture, never run directly"]
    fn child_wait() {
        if !launched() {
            return;
        }
        thread::sleep(Duration::from_secs(10));
    }
    #[test]
    fn overflow_kills_and_reaps_instead_of_retaining_or_waiting() {
        let start = Instant::now();
        assert!(matches!(fixture("video::process::tests::child_flood", None, 4096, Arc::new(AtomicBool::new(false))), Err(VideoError::Limit)));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn blocked_writer_and_full_pipe_are_cancelled_by_supervisor() {
        let stop = Arc::new(AtomicBool::new(false));
        let input = Input {
            stop,
            write: Box::new(|out| {
                let buf = [0; 65536];
                loop {
                    out.write_all(&buf).map_err(|_| VideoError::Io)?;
                }
            }),
        };
        let start = Instant::now();
        assert!(matches!(fixture("video::process::tests::child_wait", Some(input), 65536, Arc::new(AtomicBool::new(false))), Err(VideoError::Timeout)));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
    #[test]
    fn already_cancelled_does_not_launch_even_missing_program() {
        let flag = Arc::new(AtomicBool::new(true));
        let result = run(
            Path::new("/not/a/tool"),
            &[],
            &std::env::temp_dir(),
            None,
            0,
            Watchdog { cancel: &flag, closed: &flag, deadline: Instant::now(), counters: None },
            &mut || {},
        );
        assert!(matches!(result, Err(VideoError::Cancelled)));
    }
}

#[cfg(test)]
mod join_tests {
    use super::*;
    #[test]
    fn reader_error_and_panic_acknowledge_all_other_workers_before_return() {
        for panic in [false, true] {
            let err_done = Arc::new(AtomicBool::new(false));
            let writer_done = Arc::new(AtomicBool::new(false));
            let err_flag = Arc::clone(&err_done);
            let writer_flag = Arc::clone(&writer_done);
            let out = thread::spawn(move || {
                if panic {
                    panic!("authored read worker panic");
                }
                Err(VideoError::Io)
            });
            let err = thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                err_flag.store(true, Ordering::Release);
                Ok((Vec::new(), false))
            });
            let writer = thread::spawn(move || {
                thread::sleep(Duration::from_millis(30));
                writer_flag.store(true, Ordering::Release);
                Ok(())
            });
            let result = join_workers(out, err, Some(writer));
            assert!(err_done.load(Ordering::Acquire));
            assert!(writer_done.load(Ordering::Acquire));
            if panic {
                assert!(matches!(result, Err(VideoError::Process)));
            } else {
                assert!(matches!(result.unwrap().stdout, Err(VideoError::Io)));
            }
        }
    }
}
