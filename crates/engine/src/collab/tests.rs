use super::*;
use nodecraft_graph::{Canvas, Change, Changeset, Node};
use nodecraft_sync::{Out, Peer, Room};
use nodecraft_types::{AssetRef, CanvasId, MediaMeta, NodeId, Value};
use serde_json::json;

fn canvas() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}
fn nid(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn room_id() -> RoomId {
    RoomId::from_bytes([5; 32])
}

struct Person {
    id: Peer,
    collab: Collab,
    session: Option<Session>,
}

/// A room and the people in it, passing frames in memory as the relay would.
struct Net {
    room: Room,
    people: Vec<Person>,
    queue: Vec<Out>,
}

impl Net {
    fn enter(&mut self, mut person: Person) {
        assert!(matches!(person.collab.hello(), Frame::Hello { .. }));
        self.queue.extend(self.room.join(person.id));
        // A sharer answers the room's question; a joiner starts empty.
        if let Some(session) = &person.session {
            assert!(person.collab.publish(session).unwrap().is_empty());
        }
        self.people.push(person);
        self.settle();
    }

    /// Delivers every frame until nobody has anything more to send.
    fn settle(&mut self) {
        for _ in 0..64 {
            let mut sent = Vec::new();
            for out in std::mem::take(&mut self.queue) {
                let (Out::Send(to, frame) | Out::Close(to, frame)) = out;
                let person = self.people.iter_mut().find(|p| p.id == to).unwrap();
                for reply in person.collab.receive(frame).unwrap() {
                    sent.push((to, reply));
                }
            }
            for person in &mut self.people {
                if person.session.is_none()
                    && let Some(id) = person.collab.project_id().unwrap()
                {
                    person.session = Some(Session::new(joining(id).unwrap()).unwrap());
                }
                if let Some(session) = &mut person.session {
                    for frame in person.collab.settle(session).unwrap() {
                        sent.push((person.id, frame));
                    }
                }
            }
            if sent.is_empty() && self.queue.is_empty() {
                return;
            }
            for (from, frame) in sent {
                // Every frame goes over the wire, as bytes.
                let frame = Frame::decode(&frame.encode().unwrap()).unwrap();
                self.queue.extend(self.room.handle(from, frame));
            }
        }
        panic!("the room never went quiet");
    }

    fn session(&mut self, id: Peer) -> &mut Session {
        self.people.iter_mut().find(|p| p.id == id).and_then(|p| p.session.as_mut()).unwrap()
    }
}

/// A session whose project has one card showing a file.
fn sharer() -> (Session, AssetRef) {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "Shared").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "Main") }]).apply(&mut project).unwrap();
    let mut session = Session::new(project).unwrap();
    let hash = session.import_asset(b"not really a png").unwrap();
    let asset = AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() };
    let mut card = Node::new(nid(1), "text");
    card.params.insert("image".into(), Value::Media(asset.clone()));
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: card }])).unwrap();
    (session, asset)
}

fn add(session: &mut Session, n: u8) {
    session.dispatch("node.add", &json!({"canvas": canvas(), "node": Node::new(nid(n), "text")})).unwrap();
}

#[test]
fn three_people_end_with_one_project_and_its_images() {
    let (session, asset) = sharer();
    let mut net = Net { room: Room::new(), people: Vec::new(), queue: Vec::new() };
    let collab = Collab::share(&session, room_id()).unwrap();
    net.enter(Person { id: 1, collab, session: Some(session) });
    net.enter(Person { id: 2, collab: Collab::join(room_id()), session: None });
    let first = net.session(1).document().clone();
    assert_eq!(net.session(2).document(), &first);
    assert_eq!(net.session(2).assets().get(&asset.hash), Some(&b"not really a png"[..]));

    // Both add a card at once; both keep both.
    add(net.session(1), 5);
    add(net.session(2), 6);
    net.settle();
    let both = net.session(1).document().clone();
    assert_eq!(net.session(2).document(), &both);
    assert!([5, 6].iter().all(|n| both.canvases()[&canvas()].nodes().contains_key(&nid(*n))));

    // Undo is each person's own: person 2 takes back only their card.
    net.session(2).dispatch("edit.undo", &json!({})).unwrap();
    net.settle();
    let nodes = net.session(1).document().canvases()[&canvas()].nodes().keys().copied().collect::<Vec<_>>();
    assert_eq!(nodes, vec![nid(1), nid(5)]);
    assert!(net.session(1).can_undo());

    // Someone who comes later gets all of it, the image too, even with its sharer gone.
    net.room.leave(1);
    net.people.retain(|p| p.id != 1);
    net.enter(Person { id: 3, collab: Collab::join(room_id()), session: None });
    let second = net.session(2).document().clone();
    assert_eq!(net.session(3).document(), &second);
    assert!(net.session(3).assets().contains(&asset.hash));
}

#[test]
fn a_session_of_another_project_is_refused() {
    let (session, _) = sharer();
    let mut collab = Collab::share(&session, room_id()).unwrap();
    let (_, update) = Mirror::share(&nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([9; 16]), "").unwrap()).unwrap();
    let mut joiner = Collab::join(room_id());
    joiner.receive(Frame::Update(update)).unwrap();
    let mut session = session;
    assert!(joiner.settle(&mut session).is_err());
    assert!(collab.receive(Frame::Refused("bye".into())).is_err());
}

#[test]
fn a_card_waits_for_its_image_and_nothing_else_does() {
    let (mut session, asset) = sharer();
    add(&mut session, 2);
    let mut a = Collab::share(&session, room_id()).unwrap();
    let mut b = Collab::join(room_id());
    let _ = b.hello();
    // The handshake, by hand: b says what it has, a sends the rest.
    let replies = b.receive(Frame::Step1(Mirror::join().state_vector().unwrap())).unwrap();
    let Some(Frame::Step1(b_has)) = replies.into_iter().nth(1) else { panic!("b asks back") };
    let Some(Frame::Step2(everything)) = a.receive(Frame::Step1(b_has)).unwrap().into_iter().next() else { panic!("a answers") };
    b.receive(Frame::Step2(everything)).unwrap();
    let mut joined = Session::new(joining(b.project_id().unwrap().unwrap()).unwrap()).unwrap();

    // The image hasn't come: card 2 is in, card 1 waits, and b asks for the image once.
    let sent = b.settle(&mut joined).unwrap();
    assert_eq!(sent, vec![Frame::AssetGet(vec![*asset.hash.as_bytes()])]);
    let cards = joined.document().canvases()[&canvas()].nodes().keys().copied().collect::<Vec<_>>();
    assert_eq!(cards, vec![nid(2)]);
    assert!(b.settle(&mut joined).unwrap().is_empty());

    // It comes; card 1 follows.
    assert!(a.receive(Frame::AssetGet(vec![*asset.hash.as_bytes()])).unwrap().is_empty());
    let put = a.publish(&session).unwrap();
    assert!(matches!(&put[..], [Frame::AssetPut { .. }]));
    for frame in put {
        b.receive(frame).unwrap();
    }
    assert!(b.settle(&mut joined).unwrap().is_empty());
    assert_eq!(joined.document(), session.document());
}
