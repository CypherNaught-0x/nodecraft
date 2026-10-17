use super::super::Client;
use super::*;
use std::sync::atomic::AtomicBool;

struct App {
    addr: SocketAddr,
    token: Token,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl App {
    fn new() -> Self {
        let token = Token::generate().unwrap();
        let listener = listen(token.clone(), Arc::new(|| {})).unwrap();
        let addr = listener.addr;
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut s = session();
            while !stopping.load(Ordering::SeqCst) {
                match listener.calls.recv_timeout(Duration::from_millis(20)) {
                    Ok(call) => {
                        let result = answer(&mut s, &call.request).unwrap();
                        call.reply(result);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });
        Self { addr, token, stop, worker: Some(worker) }
    }
    fn connect(&self) -> Client {
        Client::connect(self.addr, &self.token.to_hex()).unwrap()
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn age(client: &mut Client) {
    client.last_used = Instant::now().checked_sub(RECONNECT_AFTER).unwrap();
}
#[test]
fn four_live_slots_reconnect_before_idle_without_needing_a_fifth_slot() {
    let app = App::new();
    let mut clients: Vec<_> = (0..MAX_CONNECTIONS).map(|_| app.connect()).collect();
    assert_eq!(Client::connect(app.addr, &app.token.to_hex()).err().unwrap().kind, ErrorKind::Busy);
    for _ in 0..3 {
        for client in &mut clients {
            age(client); // No540second wait; server's600second idle timer is still live.
            let result = client.call("state.get", Map::new());
            assert!(result.is_ok(), "replacement must release its old slot first: {result:?}");
        }
    }
    assert_eq!(Client::connect(app.addr, &app.token.to_hex()).err().unwrap().kind, ErrorKind::Busy, "cap remains four after repeated replacement");
}

fn socket_listener() -> TcpListener {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}
fn accept(listener: &TcpListener) -> (BufReader<TcpStream>, TcpStream) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                stream.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
                return (BufReader::new(stream.try_clone().unwrap()), stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::yield_now(),
            Err(e) => panic!("bounded accept failed: {e}"),
        }
    }
}
fn read(reader: &mut BufReader<TcpStream>) -> Value {
    let mut text = String::new();
    reader.read_line(&mut text).unwrap();
    serde_json::from_str(&text).unwrap()
}
fn hello(reader: &mut BufReader<TcpStream>, writer: &mut TcpStream, protocol: u32) {
    let request = read(reader);
    assert_eq!(request["method"], "hello");
    writeln!(writer, "{}", json!({"id":request["id"],"result":{"protocol":protocol}})).unwrap();
}
fn disconnected(addr: SocketAddr) -> Client {
    Client { io: None, next: 1, origin: Origin::Addr(addr, token().to_hex()), last_used: Instant::now() }
}

#[test]
fn only_typed_busy_hello_retries_and_old_stream_is_closed_before_replacement() {
    let listener = socket_listener();
    let addr = listener.local_addr().unwrap();
    let (closed, observed) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let (mut reader, mut writer) = accept(&listener);
        hello(&mut reader, &mut writer, PROTOCOL);
        let mut eof = String::new();
        assert_eq!(reader.read_line(&mut eof).unwrap(), 0, "oldsocket must shutdown BEFORE new admission");
        closed.send(()).unwrap();
        drop((reader, writer));
        let (mut reader, mut writer) = accept(&listener);
        let request = read(&mut reader);
        assert_eq!(request["method"], "hello");
        writeln!(writer, "{}", json!({"id":Value::Null,"error":{"kind":"busy","message":"arbitrary non-matching admission text"}})).unwrap();
        drop((reader, writer));
        let (mut reader, mut writer) = accept(&listener);
        hello(&mut reader, &mut writer, PROTOCOL);
        assert_eq!(read(&mut reader)["method"], "state.get");
        writeln!(writer, "{}", json!({"id":2,"result":{"accepted":true}})).unwrap();
    });
    let mut client = Client::connect(addr, &token().to_hex()).unwrap();
    let mut waits = 0;
    client
        .reconnect_with(admission_deadline(), |delay| {
            assert!(!delay.is_zero());
            observed.recv_timeout(Duration::from_secs(3)).unwrap();
            waits += 1;
        })
        .unwrap();
    assert_eq!(waits, 1);
    assert_eq!(client.call("state.get", Map::new()).unwrap()["accepted"], true);
    worker.join().unwrap();
}

#[test]
fn repeated_busy_is_bounded_and_non_busy_or_expired_admission_never_retries() {
    let listener = socket_listener();
    let addr = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        for _ in 0..MAX_ADMISSION_ATTEMPTS {
            let (mut reader, mut writer) = accept(&listener);
            assert_eq!(read(&mut reader)["method"], "hello");
            writeln!(writer, "{}", json!({"id":Value::Null,"error":{"kind":"busy","message":"full"}})).unwrap();
        }
    });
    let mut client = disconnected(addr);
    let mut waits = 0;
    assert_eq!(client.reconnect_with(admission_deadline(), |_| waits += 1).unwrap_err().kind, ErrorKind::Busy);
    assert_eq!(waits, MAX_ADMISSION_ATTEMPTS - 1);
    assert!(client.io.is_none());
    worker.join().unwrap();
    assert_eq!(client.reconnect_with(Instant::now(), |_| panic!("no wait after deadline")).unwrap_err().kind, ErrorKind::Network);

    let listener = socket_listener();
    let addr = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut reader, mut writer) = accept(&listener);
        let request = read(&mut reader);
        writeln!(writer, "{}", json!({"id":request["id"],"error":{"kind":"authentication","message":"busy but authentication failed"}})).unwrap();
    });
    let mut client = disconnected(addr);
    assert_eq!(
        client.reconnect_with(admission_deadline(), |_| panic!("never retry authentication by message text")).unwrap_err().kind,
        ErrorKind::Authentication
    );
    assert!(client.io.is_none());
    worker.join().unwrap();

    let listener = socket_listener();
    let addr = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut reader, writer) = accept(&listener);
        assert_eq!(read(&mut reader)["method"], "hello");
        drop((reader, writer)); // Transport failure during hello is NOT typed admission Busy.
    });
    let mut client = disconnected(addr);
    assert_eq!(client.reconnect_with(admission_deadline(), |_| panic!("never retry Network/reset")).unwrap_err().kind, ErrorKind::Network);
    assert!(client.io.is_none());
    worker.join().unwrap();
}

#[test]
fn an_executed_mutation_with_lost_reply_is_never_resent() {
    let listener = socket_listener();
    let addr = listener.local_addr().unwrap();
    let (executed, observed) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut session = session();
        let (mut reader, mut writer) = accept(&listener);
        hello(&mut reader, &mut writer, PROTOCOL);
        let request = read(&mut reader);
        assert_eq!(request["method"], "command.run");
        let params = request["params"].as_object().unwrap().clone();
        answer(&mut session, &Request::RunCommand(params)).unwrap().unwrap();
        assert_eq!(session.revision(), 1);
        executed.send(()).unwrap();
        drop((reader, writer)); // Actual mutation happened, but its reply is deliberately lost.
        let (mut reader, mut writer) = accept(&listener);
        hello(&mut reader, &mut writer, PROTOCOL);
        let request = read(&mut reader);
        assert_eq!(request["method"], "state.get", "only the next explicit operation may be sent");
        let result = answer(&mut session, &Request::State).unwrap().unwrap();
        writeln!(writer, "{}", json!({"id":request["id"],"result":result})).unwrap();
        assert_eq!(session.revision(), 1);
    });
    let mut client = Client::connect(addr, &token().to_hex()).unwrap();
    let params = json!({"id":"node.add","params":{"canvas":CANVAS,"node":{"id":"11111111-1111-4111-8111-111111111111","kind":"source.text"}}})
        .as_object()
        .unwrap()
        .clone();
    assert_eq!(client.call("command.run", params).unwrap_err().kind, ErrorKind::Network);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(client.io.is_none());
    let state = client.call("state.get", Map::new()).unwrap();
    assert_eq!(state["revision"], 1);
    assert_eq!(state["canvases"][0]["nodes"], 1);
    worker.join().unwrap();
}

#[test]
fn rotated_tokens_missing_file_and_protocol_failure_leave_recoverable_disconnected_state() {
    let first = App::new();
    let second = App::new();
    let base = std::env::temp_dir().join(format!("nodecraft272-{}", std::process::id()));
    let dir = private_dir(&base).unwrap();
    let path = dir.join("control.json");
    let write = |app: &App| TokenFile { port: app.addr.port(), token: app.token.to_hex(), pid: std::process::id() }.write(&path).unwrap();
    write(&first);
    let mut client = Client::from_token_file(&path).unwrap();
    write(&second);
    age(&mut client);
    client.call("state.get", Map::new()).unwrap();
    assert_eq!(client.io.as_ref().unwrap().writer.peer_addr().unwrap(), second.addr);
    std::fs::remove_file(&path).unwrap();
    age(&mut client);
    assert_eq!(client.call("state.get", Map::new()).unwrap_err().kind, ErrorKind::NotFound);
    assert!(client.io.is_none());
    write(&second);
    client.call("state.get", Map::new()).unwrap();
    let listener = socket_listener();
    let addr = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut reader, mut writer) = accept(&listener);
        hello(&mut reader, &mut writer, PROTOCOL + 1);
    });
    client.origin = Origin::Addr(addr, token().to_hex());
    age(&mut client);
    assert_eq!(client.call("state.get", Map::new()).unwrap_err().kind, ErrorKind::Unsupported);
    assert!(client.io.is_none());
    worker.join().unwrap();
    client.origin = Origin::File(path.clone());
    client.call("state.get", Map::new()).unwrap();
    drop(client);
    std::fs::remove_dir_all(base).unwrap();
}

fn count_is(count: &AtomicUsize, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while count.load(Ordering::SeqCst) != expected {
        assert!(Instant::now() < deadline, "refusal worker count did not become {expected}");
        std::thread::yield_now();
    }
}
#[test]
fn refusal_pool_is_bounded_expires_partial_hellos_and_never_dispatches() {
    let listener = listen_with(token(), Arc::new(|| {}), Timeouts { handshake: Duration::from_secs(1), ..Timeouts::default() }).unwrap();
    let held: Vec<_> = (0..MAX_CONNECTIONS).map(|_| Client::connect(listener.addr, &token().to_hex()).unwrap()).collect();
    let mut partial: Vec<_> = (0..MAX_BUSY_HANDSHAKES)
        .map(|_| {
            let mut stream = TcpStream::connect(listener.addr).unwrap();
            stream.write_all(b"{").unwrap();
            stream
        })
        .collect();
    count_is(&listener.busy, MAX_BUSY_HANDSHAKES);
    let mut excess = TcpStream::connect(listener.addr).unwrap();
    excess.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut byte = [0];
    let closed = std::io::Read::read(&mut excess, &mut byte);
    assert!(
        matches!(closed, Ok(0)) || closed.is_err_and(|e| matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted)),
        "excess refusal must close immediately"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while listener.busy.load(Ordering::SeqCst) > 0 {
        for stream in &mut partial {
            let _ = stream.write_all(b"x");
        }
        assert!(Instant::now() < deadline, "partial trickles cannot extend the whole admission deadline");
        std::thread::park_timeout(Duration::from_millis(20));
    }
    assert_eq!(Client::connect(listener.addr, &token().to_hex()).err().unwrap().kind, ErrorKind::Busy);
    assert_eq!(Client::connect(listener.addr, "wrong-token").err().unwrap().kind, ErrorKind::Authentication);
    count_is(&listener.busy, 0);
    assert!(matches!(listener.calls.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)), "refusal workers never queue an operation");
    drop((held, partial));
}

#[test]
fn refusal_permits_release_when_spawn_closure_is_discarded_or_unwinds() {
    let count = Arc::new(AtomicUsize::new(0));
    let mut permits: Vec<_> = (0..MAX_BUSY_HANDSHAKES).map(|_| BusySlot::acquire(&count).unwrap()).collect();
    assert!(BusySlot::acquire(&count).is_none());
    let permit = permits.pop().unwrap();
    let not_spawned = move || {
        let _permit = permit;
    };
    drop(not_spawned); // Thread Builder owns/drops this closure on a spawn error too.
    assert_eq!(count.load(Ordering::SeqCst), MAX_BUSY_HANDSHAKES - 1);
    let permit = BusySlot::acquire(&count).unwrap();
    assert!(
        std::thread::spawn(move || {
            let _permit = permit;
            panic!("fixture unwind");
        })
        .join()
        .is_err()
    );
    assert_eq!(count.load(Ordering::SeqCst), MAX_BUSY_HANDSHAKES - 1);
    drop(permits);
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
