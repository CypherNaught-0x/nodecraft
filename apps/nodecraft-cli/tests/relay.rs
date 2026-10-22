//! `nodecraft-cli relay` (#119): sessions shared through a real relay process end with the same
//! project, and a stranger's frame is turned away.
use std::io::{BufRead, BufReader, ErrorKind};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nodecraft_engine::Session;
use nodecraft_engine::collab::{Collab, joining};
use nodecraft_graph::{Canvas, Change, Changeset, Node, Project};
use nodecraft_sync::{Frame, RoomId};
use nodecraft_types::{CanvasId, NodeId, ProjectId};
use serde_json::json;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

struct Relay {
    child: Child,
    url: String,
    stderr: BufReader<std::process::ChildStderr>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn relay() -> Relay {
    let mut child = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli"))
        .args(["relay", "--listen", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    let url = line.split_whitespace().find(|word| word.starts_with("ws://")).unwrap().trim_end_matches('.').to_string();
    Relay { child, url, stderr }
}

fn canvas() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}

fn room() -> RoomId {
    RoomId::from_bytes([7; 32])
}

struct Person {
    socket: Socket,
    collab: Collab,
    session: Option<Session>,
}

impl Person {
    fn connect(relay: &Relay, collab: Collab, session: Option<Session>) -> Self {
        let (mut socket, _) = tungstenite::connect(&relay.url).unwrap();
        if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
            stream.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
        }
        socket.send(Message::binary(collab.hello().encode().unwrap())).unwrap();
        Self { socket, collab, session }
    }

    fn send(&mut self, frames: Vec<Frame>) {
        for frame in frames {
            self.socket.send(Message::binary(frame.encode().unwrap())).unwrap();
        }
    }

    /// One turn of a host's loop: read what came, take it in, send what that makes.
    fn turn(&mut self) {
        loop {
            match self.socket.read() {
                Ok(Message::Binary(bytes)) => {
                    let replies = self.collab.receive(Frame::decode(&bytes).unwrap()).unwrap();
                    self.send(replies);
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
                Err(error) => panic!("{error}"),
            }
        }
        if self.session.is_none()
            && let Some(id) = self.collab.project_id().unwrap()
        {
            self.session = Some(Session::new(joining(id).unwrap()).unwrap());
        }
        if let Some(session) = &mut self.session {
            let frames = self.collab.settle(session).unwrap();
            self.send(frames);
        }
    }

    fn document(&self) -> Option<&Project> {
        self.session.as_ref().map(Session::document)
    }
}

/// Turns everyone until `done` holds.
fn until(people: &mut [Person], done: impl Fn(&[Person]) -> bool) {
    let started = Instant::now();
    while !done(people) {
        assert!(started.elapsed() < Duration::from_secs(20), "the shared session never settled");
        for person in people.iter_mut() {
            person.turn();
        }
    }
}

fn same(people: &[Person], cards: usize) -> bool {
    let Some(first) = people.first().and_then(Person::document) else { return false };
    first.canvases().get(&canvas()).is_some_and(|c| c.nodes().len() == cards) && people.iter().all(|p| p.document() == Some(first))
}

fn add(person: &mut Person, n: u8) {
    let node = Node::new(NodeId::from_bytes([n; 16]), "text");
    person.session.as_mut().unwrap().dispatch("node.add", &json!({"canvas": canvas(), "node": node})).unwrap();
}

#[test]
fn people_sharing_through_the_relay_end_with_one_project() {
    let mut relay = relay();
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Shared").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "Main") }]).apply(&mut project).unwrap();
    let session = Session::new(project).unwrap();
    let sharer = Person::connect(&relay, Collab::share(&session, room()).unwrap(), Some(session));
    let joiner = Person::connect(&relay, Collab::join(room()), None);
    let mut people = vec![sharer, joiner];
    until(&mut people, |p| same(p, 0));

    // Both add a card at once; both keep both.
    add(&mut people[0], 5);
    add(&mut people[1], 6);
    until(&mut people, |p| same(p, 2));

    // Someone later gets it all, though the sharer has gone.
    let sharer = people.remove(0);
    drop(sharer);
    people.push(Person::connect(&relay, Collab::join(room()), None));
    until(&mut people, |p| same(p, 2));

    // The room id is the room's key: the relay never writes it down.
    relay.child.kill().unwrap();
    let mut rest = String::new();
    while relay.stderr.read_line(&mut rest).is_ok_and(|n| n > 0) {}
    assert!(!rest.contains(&room().to_string()), "{rest}");
}

#[test]
fn a_connection_that_doesnt_start_with_a_hello_is_refused() {
    let relay = relay();
    let (mut socket, _) = tungstenite::connect(&relay.url).unwrap();
    socket.send(Message::binary(Frame::Step1(Vec::new()).encode().unwrap())).unwrap();
    let reply = loop {
        match socket.read().unwrap() {
            Message::Binary(bytes) => break Frame::decode(&bytes).unwrap(),
            _ => continue,
        }
    };
    assert!(matches!(reply, Frame::Refused(_)), "{reply:?}");
}

#[test]
fn relay_takes_only_listen() {
    let out = Command::new(env!("CARGO_BIN_EXE_nodecraft-cli")).args(["relay", "--listen", "not an address"]).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--listen"));
}
