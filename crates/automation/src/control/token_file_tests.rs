use super::*;
use std::cell::Cell;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

fn bytes(token: &str) -> Vec<u8> {
    serde_json::to_vec(&TokenFile { port: 1234, token: token.into(), pid: 1 }).unwrap()
}
fn admit(input: impl Read, connects: &Cell<usize>) -> Result<(), Error> {
    read_token_file_with(input, Path::new("public-test-control.json"), |_, _| {
        connects.set(connects.get() + 1);
        Ok(())
    })
}

#[test]
fn token_file_read_stops_at_limit_plus_one_without_connecting() {
    struct Stream {
        remaining: usize,
        read: usize,
    }
    impl Read for Stream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let n = self.remaining.min(buffer.len());
            buffer.get_mut(..n).unwrap().fill(b' ');
            self.remaining -= n;
            self.read += n;
            Ok(n)
        }
    }
    // A reader with no metadata/Seek, like a changing or non-regular input source.
    let mut stream = Stream { remaining: MAX_TOKEN_FILE_BYTES * 16, read: 0 };
    let connects = Cell::new(0);
    assert_eq!(admit(&mut stream, &connects).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(stream.read, MAX_TOKEN_FILE_BYTES + 1);
    assert_eq!(connects.get(), 0);
}

#[test]
fn token_file_malformed_input_is_rejected_before_connector() {
    let invalid =
        [vec![0xff], b"not JSON".to_vec(), b"{}".to_vec(), bytes(&"a".repeat(63)), bytes(&"a".repeat(65)), bytes(&"x".repeat(64)), bytes(&"é".repeat(64))];
    for input in invalid {
        let connects = Cell::new(0);
        let error = admit(Cursor::new(input), &connects).unwrap_err();
        assert_eq!(error.kind, ErrorKind::InvalidInput);
        assert_eq!(connects.get(), 0);
        assert!(!error.message.contains(&"a".repeat(63)) && !error.message.contains(&"x".repeat(64)));
    }
}

#[test]
fn token_file_exact_byte_boundary_and_hex_case_are_preserved() {
    // Shape validation preserves hex case; server authentication still compares exact text.
    let token = "aB".repeat(32);
    let mut input = bytes(&token);
    input.resize(MAX_TOKEN_FILE_BYTES, b' ');
    let mut observed = None;
    read_token_file_with(Cursor::new(&input), Path::new("public-test-control.json"), |addr, value| {
        observed = Some((addr, value.to_owned()));
        Ok(())
    })
    .unwrap();
    assert_eq!(observed, Some((SocketAddr::from(([127, 0, 0, 1], 1234)), token)));
    input.push(b' ');
    let connects = Cell::new(0);
    assert_eq!(admit(Cursor::new(input), &connects).unwrap_err().kind, ErrorKind::InvalidInput);
    assert_eq!(connects.get(), 0);
}

struct TempToken(PathBuf);
impl TempToken {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!("nodecraft-token271-{}-{}.json", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempToken {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn token_file_public_sparse_and_missing_files_fail_without_network() {
    let path = TempToken::new();
    let file = std::fs::OpenOptions::new().write(true).open(&path.0).unwrap();
    file.set_len(32 * 1024 * 1024).unwrap();
    let error = Client::from_token_file(&path.0).err().unwrap();
    assert_eq!(error.kind, ErrorKind::InvalidInput);
    assert!(error.message.contains("larger than 4096 bytes"));
    std::fs::remove_file(&path.0).unwrap();
    let error = Client::from_token_file(&path.0).err().unwrap();
    assert_eq!(error.kind, ErrorKind::NotFound);
    assert!(error.message.contains("--control"));
}

struct HelloServer {
    addr: SocketAddr,
    task: Option<std::thread::JoinHandle<(TcpListener, TcpStream)>>,
    connection: Option<(TcpListener, TcpStream)>,
}
impl HelloServer {
    fn complete_probe(&mut self) {
        self.connection = Some(self.task.take().unwrap().join().unwrap());
    }
}
impl Drop for HelloServer {
    fn drop(&mut self) {
        // Clean up even when caller setup or an assertion unwinds. The existing accept
        // deadline and socket timeouts bound the join; never panic while unwinding.
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn hello_server(token: String) -> HelloServer {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let task = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("bounded mock accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let hello: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(hello["method"], "hello");
        assert_eq!(hello["params"]["token"], token);
        writeln!(reader.get_mut(), "{}", json!({"id":hello["id"],"result":{"protocol":PROTOCOL}})).unwrap();
        // Prove the public client's transport works while the server is actively serving,
        // rather than inspecting peer metadata after its worker has completed.
        line.clear();
        reader.read_line(&mut line).unwrap();
        let probe: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(probe["method"], "commands.list");
        assert_eq!(probe["params"], json!({}));
        assert_eq!(probe["id"], json!(2));
        writeln!(reader.get_mut(), "{}", json!({"id":probe["id"],"result":{"port":addr.port()}})).unwrap();
        // Keep the listener bound too, so the replacement must use another port.
        (listener, reader.into_inner())
    });
    HelloServer { addr, task: Some(task), connection: None }
}

#[test]
fn token_file_public_connect_and_reconnect_reread_current_file() {
    let path = TempToken::new();
    let mut first_server = hello_server("ab".repeat(32));
    std::fs::write(&path.0, serde_json::to_vec(&TokenFile { port: first_server.addr.port(), token: "ab".repeat(32), pid: 1 }).unwrap()).unwrap();
    let mut client = Client::from_token_file(&path.0).unwrap();
    assert_eq!(client.call("commands.list", Map::new()).unwrap(), json!({"port":first_server.addr.port()}));
    first_server.complete_probe();
    std::fs::write(&path.0, bytes("invalid-public-test-token")).unwrap();
    assert_eq!(client.reconnect().unwrap_err().kind, ErrorKind::InvalidInput);
    let mut second_server = hello_server("cd".repeat(32));
    assert_ne!(first_server.addr.port(), second_server.addr.port(), "the token file must select a different listener");
    std::fs::write(&path.0, serde_json::to_vec(&TokenFile { port: second_server.addr.port(), token: "cd".repeat(32), pid: 2 }).unwrap()).unwrap();
    client.reconnect().unwrap();
    assert_eq!(client.call("commands.list", Map::new()).unwrap(), json!({"port":second_server.addr.port()}));
    second_server.complete_probe();
    assert!(matches!(&client.origin, Origin::File(origin) if origin == &path.0));
    drop(second_server);
    drop(first_server);
}

#[test]
fn token_file_invalid_hex_never_reaches_connector() {
    let connects = Cell::new(0);
    let result = admit(Cursor::new(bytes(&"x".repeat(64))), &connects);
    assert_eq!(connects.get(), 0, "invalid token must fail before attempting a socket");
    assert_eq!(result.unwrap_err().kind, ErrorKind::InvalidInput);
}
