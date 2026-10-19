use super::*;
use std::sync::atomic::AtomicUsize;
struct Input {
    length: u64,
    regular: bool,
    remaining: usize,
    extra: bool,
    largest: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    interrupts: usize,
    invalid: bool,
}
impl Read for Input {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.largest.fetch_max(out.len(), Ordering::SeqCst);
        if self.interrupts > 0 {
            self.interrupts -= 1;
            return Err(io::ErrorKind::Interrupted.into());
        }
        if self.invalid {
            return Ok(out.len() + 1);
        }
        let n = out.len().min(self.remaining);
        out[..n].fill(b'x');
        self.remaining -= n;
        if n == 0 && self.extra {
            out[0] = 1;
            self.extra = false;
            return Ok(1);
        }
        Ok(n)
    }
}
impl OpenedFile for Input {
    fn metadata(&self) -> io::Result<(bool, u64)> {
        Ok((self.regular, self.length))
    }
}
fn input(len: usize) -> Input {
    Input { length: len as u64, regular: true, remaining: len, extra: false, largest: Arc::default(), calls: Arc::default(), interrupts: 0, invalid: false }
}
fn state() -> State {
    State { cancelled: AtomicBool::new(false), deadline: Instant::now() + DEADLINE, clock: Arc::new(Monotonic) }
}
#[test]
fn complete_materialization_uses_capacity_and_bounded_requests_with_exact_eof() {
    let mut i = input(CHUNK + 13);
    let mut work = Work::default();
    let bytes = body(&mut i, CHUNK + 13, MAX_DROP_BYTES, &state(), &mut work).ok().unwrap();
    assert_eq!(bytes.len(), CHUNK + 13);
    assert!(bytes.capacity() <= MAX_DROP_BYTES);
    assert_eq!(i.largest.load(Ordering::SeqCst), CHUNK);
    assert_eq!((work.body, work.probes), (CHUNK + 13, 1));
    for (len, remaining, extra, invalid) in [(2, 1, false, false), (1, 1, true, false), (1, 1, false, true)] {
        let mut i = input(len);
        i.remaining = remaining;
        i.extra = extra;
        i.invalid = invalid;
        assert!(body(&mut i, len, MAX_DROP_BYTES, &state(), &mut Work::default()).is_err());
    }
}
#[test]
fn interruption_budget_is_shared_across_files_and_includes_eof() {
    let mut work = Work::default();
    let mut a = input(1);
    a.interrupts = 524;
    assert!(body(&mut a, 1, MAX_DROP_BYTES, &state(), &mut work).is_ok());
    let mut b = input(0);
    b.interrupts = 500;
    assert!(body(&mut b, 0, MAX_DROP_BYTES, &state(), &mut work).is_ok());
    assert_eq!(work.interrupted, 1024);
    let mut c = input(0);
    c.interrupts = 1;
    assert!(matches!(body(&mut c, 0, MAX_DROP_BYTES, &state(), &mut work), Err(ReadError::File(Reason::Interrupted))));
}
#[test]
fn returned_work_is_not_refunded_when_a_file_later_fails() {
    let mut work = Work::default();
    let mut i = input(MAX_DROP_BYTES);
    i.extra = true;
    assert!(body(&mut i, MAX_DROP_BYTES, MAX_DROP_BYTES, &state(), &mut work).is_err());
    assert_eq!(work.body, MAX_DROP_BYTES);
    assert_eq!(work.probes, 1);
    let mut retained = Vec::new();
    for _ in 0..2 {
        let mut i = input(MAX_DROP_BYTES);
        retained.push(body(&mut i, MAX_DROP_BYTES, MAX_DROP_BYTES, &state(), &mut work).ok().unwrap());
    }
    assert_eq!(work.body, MAX_DROP_TOTAL_BYTES);
    assert_eq!(retained.iter().map(Vec::capacity).sum::<usize>(), 2 * MAX_DROP_BYTES);
}
#[test]
fn opened_regular_object_and_length_are_checked_before_read() {
    struct Backend {
        regular: bool,
        length: u64,
        calls: Arc<AtomicUsize>,
    }
    impl Acquirer for Backend {
        fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
            let mut i = input(0);
            i.regular = self.regular;
            i.length = self.length;
            i.calls = self.calls.clone();
            Ok(Box::new(i))
        }
    }
    for (regular, length, reason) in [(false, 1, Reason::Nonregular), (true, MAX_DROP_BYTES as u64 + 1, Reason::TooBig)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let shared = Shared {
            mailbox: Mutex::new(None),
            wake: Condvar::new(),
            busy: Arc::default(),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            backend: Arc::new(Backend { regular, length, calls: calls.clone() }),
            clock: Arc::new(Monotonic),
        };
        assert!(matches!(one(&shared,Path::new("regular-before-open"),MAX_DROP_BYTES,&mut Work::default(),&state()),Err(ReadError::File(r)) if r==reason));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn deadline_and_cancel_are_checked_after_body_and_final_eof_calls() {
    struct TestClock {
        base: Instant,
        ms: AtomicUsize,
    }
    impl Clock for TestClock {
        fn now(&self) -> Instant {
            self.base + Duration::from_millis(self.ms.load(Ordering::SeqCst) as u64)
        }
    }
    struct Boundary<'a> {
        input: Input,
        clock: Arc<TestClock>,
        state: &'a State,
        cancel: bool,
        eof: bool,
    }
    impl Read for Boundary<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let result = self.input.read(out);
            if self.eof == matches!(result, Ok(0)) {
                if self.cancel {
                    self.state.cancelled.store(true, Ordering::Release);
                } else {
                    self.clock.ms.store(10000, Ordering::SeqCst);
                }
            }
            result
        }
    }
    impl OpenedFile for Boundary<'_> {
        fn metadata(&self) -> io::Result<(bool, u64)> {
            self.input.metadata()
        }
    }
    for eof in [false, true] {
        for cancel in [false, true] {
            let clock = Arc::new(TestClock { base: Instant::now(), ms: AtomicUsize::new(0) });
            let state = State { cancelled: AtomicBool::new(false), deadline: clock.now() + DEADLINE, clock: clock.clone() };
            let mut file = Boundary { input: input(1), clock, state: &state, cancel, eof };
            let expected = if cancel { Failure::Cancelled } else { Failure::Timeout };
            assert!(matches!(body(&mut file,1,MAX_DROP_BYTES,&state,&mut Work::default()),Err(ReadError::Whole(e)) if e == expected));
        }
    }
}

#[test]
fn interruption_exhaustion_stops_all_later_file_io() {
    struct Interrupting {
        opens: Arc<AtomicUsize>,
        reads: Arc<AtomicUsize>,
    }
    impl Acquirer for Interrupting {
        fn open(&self, _: &Path) -> io::Result<Box<dyn OpenedFile>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let mut file = input(0);
            file.interrupts = 1025;
            file.calls = self.reads.clone();
            Ok(Box::new(file))
        }
    }
    let opens = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(AtomicUsize::new(0));
    let shared = Shared {
        mailbox: Mutex::new(None),
        wake: Condvar::new(),
        busy: Arc::default(),
        alive: AtomicBool::new(true),
        stop: AtomicBool::new(false),
        backend: Arc::new(Interrupting { opens: opens.clone(), reads: reads.clone() }),
        clock: Arc::new(Monotonic),
    };
    let control = Control { state: Arc::new(state()), lease: Arc::new(Lease(shared.busy.clone())), result: Mutex::new(None) };
    let files = (0..3).map(|n| FileRequest { name: format!("file{n}"), source: Source::Path("interrupting".into()) }).collect();
    let batch = acquire(&shared, files, 0, &control).unwrap();
    assert!(batch.files.is_empty());
    assert_eq!(batch.skipped.count, 3);
    assert_eq!(opens.load(Ordering::SeqCst), 1, "exhaustion must stop later opens");
    assert_eq!(reads.load(Ordering::SeqCst), 1025, "1024 tolerated plus one refused detection only");
}
