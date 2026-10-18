use super::*;
use std::net::TcpListener;

#[test]
fn oversized_admission_reply_is_not_a_screenshot() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut hello = String::new();
        BufReader::new(socket.try_clone().unwrap()).read_line(&mut hello).unwrap();
        let reply = json!({"id":1,"result":{"protocol":PROTOCOL,"padding":"x".repeat(MAX_HELLO_BYTES)}});
        let _ = writeln!(socket, "{reply}");
    });
    let result = crate::control::Client::connect(addr, &token().to_hex());
    peer.join().unwrap();
    assert!(result.is_err(), "hello incorrectly used the screenshot-sized reply allowance");
}

fn drip_peer(replacement: bool) -> (std::net::SocketAddr, std::thread::JoinHandle<usize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let peer = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(2);
        let accept = || loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < end => std::thread::sleep(Duration::from_millis(1)),
                other => panic!("bounded test accept failed: {other:?}"),
            }
        };
        let greet = |socket: &TcpStream| {
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            socket.set_write_timeout(Some(Duration::from_millis(300))).unwrap();
            let mut hello = String::new();
            BufReader::new(socket.try_clone().unwrap()).read_line(&mut hello).unwrap();
            assert!(hello.contains("hello"));
        };
        if replacement {
            let mut socket = accept();
            greet(&socket);
            writeln!(socket, "{}", json!({"id":1,"result":{"protocol":PROTOCOL}})).unwrap();
        }
        let mut socket = accept();
        greet(&socket);
        let reply = format!("{}\n", json!({"id":1,"result":{"protocol":PROTOCOL}}));
        // Active bytes every 20ms, much shorter than the requested total 250ms bound.
        // Stop on closure; even an incorrect implementation has a finite peer lifetime.
        for byte in reply.bytes() {
            if socket.write_all(&[byte]).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut request = String::new();
        let _ = BufReader::new(socket).read_line(&mut request);
        usize::from(!request.is_empty())
    });
    (addr, peer)
}

#[test]
fn initial_drip_hello_expires_with_one_absolute_deadline() {
    let (addr, peer) = drip_peer(false);
    let start = Instant::now();
    let result = crate::control::Client::connect_until(addr, &token().to_hex(), start + Duration::from_millis(250));
    assert!(result.is_err(), "actively arriving bytes must not extend admission");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(peer.join().unwrap(), 0, "no requested operation");
}

#[test]
fn replacement_drip_hello_expires_without_sending_the_operation() {
    let (addr, peer) = drip_peer(true);
    let mut client = crate::control::Client::connect(addr, &token().to_hex()).unwrap();
    client.set_reconnect_after(Duration::ZERO);
    let start = Instant::now();
    let result = client.reconnect_until(start + Duration::from_millis(250));
    assert!(result.is_err());
    assert!(client.broken);
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(client);
    assert_eq!(peer.join().unwrap(), 0);
}

#[test]
fn busy_retries_and_backoff_share_the_admission_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = stop.clone();
    let peer = std::thread::spawn(move || {
        let end = Instant::now() + Duration::from_secs(2);
        let mut attempts = 0;
        while !stopped.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < end {
            let (mut socket, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                other => panic!("{other:?}"),
            };
            socket.set_nonblocking(false).unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            socket.set_write_timeout(Some(Duration::from_millis(200))).unwrap();
            let mut hello = String::new();
            BufReader::new(socket.try_clone().unwrap()).read_line(&mut hello).unwrap();
            assert!(hello.contains("hello"));
            let reply = if attempts == 0 { json!({"id":1,"result":{"protocol":PROTOCOL}}) } else { error_reply(Value::Null, "busy", "busy") };
            let _ = writeln!(socket, "{reply}");
            attempts += 1;
        }
        attempts
    });
    let mut client = crate::control::Client::connect(addr, &token().to_hex()).unwrap();
    let result = client.reconnect_until(Instant::now() + Duration::from_millis(60));
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let attempts = peer.join().unwrap();
    assert!(result.is_err());
    assert!(attempts > 1 && attempts <= RECONNECT_ATTEMPTS as usize, "{attempts}");
    assert!(client.broken);
    // Expired admission may not even send another hello, much less a requested operation.
    assert!(client.reconnect_until(Instant::now()).is_err());
}
