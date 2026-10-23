use super::*;
use nodecraft_graph::{Canvas, Change, Changeset, Node};
use nodecraft_sync::Role;
use nodecraft_sync::{Out, Peer, Room};
use nodecraft_types::{AssetRef, CanvasId, CommentId, MediaMeta, NodeId, Value};
use serde_json::json;
use std::collections::BTreeMap;

fn canvas() -> CanvasId {
    CanvasId::from_bytes([2; 16])
}
fn nid(n: u8) -> NodeId {
    NodeId::from_bytes([n; 16])
}
fn room_id() -> RoomId {
    RoomId::from_bytes([5; 32])
}
fn cid(n: u8) -> CommentId {
    CommentId::from_bytes([n; 16])
}
/// The relay's clock, in seconds since 1970.
const NOW: u64 = 1_800_000_000;

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
    /// Why each person who was put out, or gave up, left.
    ended: BTreeMap<Peer, String>,
    /// Where joiners keep their projects, when they keep them on disk and so take large assets.
    disk: Option<std::path::PathBuf>,
}

impl Net {
    fn new() -> Self {
        Self { room: Room::new(), people: Vec::new(), queue: Vec::new(), ended: BTreeMap::new(), disk: None }
    }

    fn enter(&mut self, mut person: Person) {
        let id = person.id;
        match person.collab.hello() {
            Frame::Hello { .. } => self.queue.extend(self.room.join(id)),
            Frame::HelloLink { link, .. } => self.queue.extend(self.room.join_link(id, link.hash(), NOW)),
            other => panic!("not a first frame: {other:?}"),
        }
        // A sharer answers the room's question (and sends its links); a joiner starts empty.
        let sent = person.session.as_ref().map(|session| person.collab.publish(session).unwrap()).unwrap_or_default();
        assert!(sent.iter().all(|frame| matches!(frame, Frame::LinkPut { .. } | Frame::LinkRevoke(_))));
        self.people.push(person);
        for frame in sent {
            self.queue.extend(self.room.handle(id, frame));
        }
        self.settle();
    }

    /// Asks person `id`'s share links service, as the host would.
    fn links(&mut self, id: Peer, request: serde_json::Value) -> Result<serde_json::Value, Error> {
        let answer = self.people.iter_mut().find(|p| p.id == id).unwrap().collab.links_request(&request, NOW);
        self.settle();
        answer
    }

    /// Person `id`'s connection drops, with whatever was on its way to them.
    fn drop_out(&mut self, id: Peer) -> Person {
        self.room.leave(id);
        self.queue.retain(|out| !matches!(out, Out::Send(to, _) | Out::Close(to, _) if *to == id));
        let at = self.people.iter().position(|p| p.id == id).unwrap();
        self.people.remove(at)
    }

    fn collab(&mut self, id: Peer) -> &mut Collab {
        &mut self.people.iter_mut().find(|p| p.id == id).unwrap().collab
    }

    fn person(&self, id: Peer) -> &Person {
        self.people.iter().find(|p| p.id == id).unwrap()
    }

    /// Delivers every frame until nobody has anything more to send.
    fn settle(&mut self) {
        for _ in 0..64 {
            let mut sent = Vec::new();
            for out in std::mem::take(&mut self.queue) {
                let (Out::Send(to, frame) | Out::Close(to, frame)) = out;
                let Some(person) = self.people.iter_mut().find(|p| p.id == to) else { continue };
                match person.collab.receive(frame) {
                    Ok(replies) => sent.extend(replies.into_iter().map(|reply| (to, reply))),
                    Err(error) => {
                        self.ended.insert(to, error.to_string());
                        self.room.leave(to);
                        self.people.retain(|p| p.id != to);
                    }
                }
            }
            for person in &mut self.people {
                if person.session.is_none()
                    && let Some(id) = person.collab.project_id().unwrap()
                {
                    person.session = Some(match &self.disk {
                        Some(dir) => on_disk(joining(id).unwrap(), &dir.join(person.id.to_string())),
                        None => Session::new(joining(id).unwrap()).unwrap(),
                    });
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
    let mut session = Session::new(empty()).unwrap();
    let hash = session.import_asset(b"not really a png").unwrap();
    let asset = AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() };
    let mut card = Node::new(nid(1), "text");
    card.params.insert("image".into(), Value::Media(asset.clone()));
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: card }])).unwrap();
    (session, asset)
}

/// A project with one empty canvas.
fn empty() -> nodecraft_graph::Project {
    let mut project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([1; 16]), "Shared").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(canvas(), "Main") }]).apply(&mut project).unwrap();
    project
}

/// A session of `project` that keeps its assets on disk under `dir`, as the desktop's do.
fn on_disk(project: nodecraft_graph::Project, dir: &std::path::Path) -> Session {
    std::fs::create_dir_all(dir).unwrap();
    let snapshot = nodecraft_format::Snapshot { project, assets: Default::default(), epoch: 0 };
    Session::with_storage(snapshot, Box::new(nodecraft_format::NativeStorage::new(dir.join("recovery")).unwrap())).unwrap()
}

fn add(session: &mut Session, n: u8) {
    session.dispatch("node.add", &json!({"canvas": canvas(), "node": Node::new(nid(n), "text")})).unwrap();
}

#[test]
fn three_people_end_with_one_project_and_its_images() {
    let (session, asset) = sharer();
    let mut net = Net::new();
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

fn note(n: u8, text: &str) -> serde_json::Value {
    json!({"canvas": canvas(), "id": cid(n), "text": text, "pos": {"x": 10, "y": 20}})
}

fn reply(root: u8, n: u8, text: &str) -> serde_json::Value {
    json!({"canvas": canvas(), "id": cid(root), "reply": cid(n), "text": text})
}

/// The owner, sharing, with a link for Ana in `role`; Ana is person 2.
fn with_link(role: &str) -> (Net, serde_json::Value) {
    let (session, _) = sharer();
    let mut net = Net::new();
    let collab = Collab::share(&session, room_id()).unwrap();
    net.enter(Person { id: 1, collab, session: Some(session) });
    let link = net.links(1, json!({"op": "create", "role": role, "name": "Ana"})).unwrap();
    let key: LinkKey = link["key"].as_str().unwrap().parse().unwrap();
    net.enter(Person { id: 2, collab: Collab::join_link(key), session: None });
    (net, link)
}

#[test]
fn a_commenter_discusses_through_the_relay_and_edits_nothing() {
    let (mut net, _) = with_link("comment");
    let owner = net.session(1).document().clone();
    assert_eq!(net.session(2).document(), &owner);
    assert_eq!(net.person(2).collab.role(), Some(Role::Comment));
    assert_eq!(net.session(2).access().author(), Some("Ana"));

    let refused = net.session(2).dispatch("node.add", &json!({"canvas": canvas(), "node": Node::new(nid(7), "text")})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Forbidden);
    assert!(net.session(2).dispatch("run.selected", &json!({"canvas": canvas()})).is_err());

    // Ana's discussion reaches the owner in her name.
    net.session(2).dispatch("comment.create", &note(10, "Lovely light")).unwrap();
    net.settle();
    let thread = &net.session(1).document().canvases()[&canvas()].comments()[&cid(10)];
    assert_eq!(thread.author.as_deref(), Some("Ana"));
    assert!(net.person(2).collab.pending.is_empty());

    // Both answer at once; both answers stay, in both copies.
    net.session(1).dispatch("comment.reply", &reply(10, 11, "Thanks")).unwrap();
    net.session(2).dispatch("comment.reply", &reply(10, 12, "More of it")).unwrap();
    net.settle();
    let both = net.session(1).document().clone();
    assert_eq!(net.session(2).document(), &both);
    let replies: Vec<CommentId> = both.canvases()[&canvas()].comments()[&cid(10)].replies.iter().map(|r| r.id).collect();
    assert_eq!(replies.len(), 2);
    assert!(replies.contains(&cid(11)) && replies.contains(&cid(12)));

    // The owner's reply isn't Ana's to change; her own is.
    assert!(net.session(2).dispatch("comment.edit", &json!({"canvas": canvas(), "id": cid(10), "reply": cid(11), "text": "Mine now"})).is_err());
    net.session(2).dispatch("comment.edit", &json!({"canvas": canvas(), "id": cid(10), "reply": cid(12), "text": "Much more"})).unwrap();
    net.session(2).dispatch("comment.resolve", &json!({"canvas": canvas(), "id": cid(10), "resolved": true})).unwrap();
    net.settle();
    let thread = &net.session(1).document().canvases()[&canvas()].comments()[&cid(10)];
    assert!(thread.resolved);
    assert_eq!(thread.replies.iter().find(|r| r.id == cid(12)).unwrap().text, "Much more");
    assert!(net.ended.is_empty(), "{:?}", net.ended);
}

#[test]
fn a_viewer_sees_edits_changes_nothing_and_is_put_out_when_revoked() {
    let (mut net, link) = with_link("view");
    assert_eq!(net.person(2).collab.role(), Some(Role::View));
    for (id, params) in [("node.add", json!({"canvas": canvas(), "node": Node::new(nid(7), "text")})), ("comment.create", note(10, "Hmm"))] {
        assert_eq!(net.session(2).dispatch(id, &params).unwrap_err().kind, ErrorKind::Forbidden, "{id}");
    }
    add(net.session(1), 5);
    net.settle();
    assert!(net.session(2).document().canvases()[&canvas()].nodes().contains_key(&nid(5)));

    // Only the owner manages links.
    let listed = net.links(1, json!({"op": "list"})).unwrap();
    assert_eq!(listed["links"][0]["id"], link["id"]);
    assert!(listed["links"][0].get("key").is_none());
    let refused = net.people.iter_mut().find(|p| p.id == 2).unwrap().collab.links_request(&json!({"op": "list"}), NOW).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Forbidden);

    net.links(1, json!({"op": "revoke", "id": link["id"]})).unwrap();
    assert!(net.ended[&2].contains("revoked"), "{:?}", net.ended);
    assert!(net.links(1, json!({"op": "list"})).unwrap()["links"].as_array().unwrap().is_empty());
}

#[test]
fn a_changed_link_brings_its_holder_back_on_new_terms() {
    let (mut net, link) = with_link("view");
    let mut changed = net.links(1, json!({"op": "update", "id": link["id"], "role": "edit", "name": "Ana"})).unwrap();
    assert_eq!(changed["role"], "edit");
    assert!(net.ended[&2].contains("changed"));
    // Ana comes back with the same key and may now edit.
    let key: LinkKey = link["key"].as_str().unwrap().parse().unwrap();
    net.enter(Person { id: 3, collab: Collab::join_link(key), session: None });
    assert_eq!(net.person(3).collab.role(), Some(Role::Edit));
    add(net.session(3), 6);
    net.settle();
    assert!(net.session(1).document().canvases()[&canvas()].nodes().contains_key(&nid(6)));

    // A link that has run out lets nobody in.
    changed = net.links(1, json!({"op": "update", "id": link["id"], "role": "view", "name": "Ana", "expires": NOW + 1})).unwrap();
    assert_eq!(changed["expires"], NOW + 1);
    assert!(net.links(1, json!({"op": "create", "role": "view", "name": "Bo", "expires": NOW})).is_err());
    net.queue.extend(net.room.expire(NOW + 1));
    net.settle();
    net.enter(Person { id: 4, collab: Collab::join_link(link["key"].as_str().unwrap().parse().unwrap()), session: None });
    assert!(net.ended[&4].contains("share link"), "{:?}", net.ended);
}

#[test]
fn a_relay_frame_out_of_turn_ends_the_session() {
    let (session, _) = sharer();
    let mut owner = Collab::share(&session, room_id()).unwrap();
    assert!(owner.receive(Frame::Welcome { role: Role::Edit, name: None }).is_err());
    let mut joiner = Collab::join_link(LinkKey::from_bytes([3; 32]));
    let _ = joiner.hello();
    assert!(joiner.receive(Frame::Welcome { role: Role::Owner, name: None }).is_err());
    let mut joiner = Collab::join_link(LinkKey::from_bytes([3; 32]));
    let _ = joiner.hello();
    assert!(joiner.receive(Frame::Welcome { role: Role::Comment, name: None }).is_err());
    let mut viewer = Collab::join_link(LinkKey::from_bytes([3; 32]));
    let _ = viewer.hello();
    assert!(matches!(&viewer.receive(Frame::Welcome { role: Role::View, name: Some("Ana".into()) }).unwrap()[..], [Frame::Step1(_)]));
    assert!(viewer.receive(Frame::Step1(Vec::new())).is_err());
    assert!(viewer.receive(Frame::CommentDone { seq: 1, refused: None }).is_err());
    let ended = viewer.receive(Frame::Refused("the share link was revoked\u{7}".into())).unwrap_err();
    assert_eq!(ended.message, "the relay ended the shared session: the share link was revoked");
}

#[test]
fn a_restarted_relay_learns_the_owners_links_and_revokes_again() {
    let (mut net, kept) = with_link("view");
    let gone = net.links(1, json!({"op": "create", "role": "edit", "name": "Bo"})).unwrap();
    net.links(1, json!({"op": "revoke", "id": gone["id"]})).unwrap();
    // The relay restarts: it holds no rooms and no links.
    let owner = net.drop_out(1);
    net.drop_out(2);
    net.room = Room::new();
    net.enter(owner);
    for (id, link) in [(3, &kept), (4, &gone)] {
        net.enter(Person { id, collab: Collab::join_link(link["key"].as_str().unwrap().parse().unwrap()), session: None });
    }
    assert_eq!(net.person(3).collab.role(), Some(Role::View));
    assert!(net.ended.contains_key(&4), "{:?}", net.ended);
}

#[test]
fn a_commenters_unanswered_thread_goes_again_on_reconnect_and_keeps_what_came_since() {
    let (mut net, _) = with_link("comment");
    net.session(2).dispatch("comment.create", &note(10, "Lovely light")).unwrap();
    // The relay writes the thread, but Ana's connection drops before she hears so.
    let session = net.people.iter_mut().find(|p| p.id == 2).unwrap().session.take().unwrap();
    for frame in net.collab(2).publish(&session).unwrap() {
        let out = net.room.handle(2, frame);
        net.queue.extend(out);
    }
    let mut ana = net.drop_out(2);
    ana.session = Some(session);
    net.settle();
    net.session(1).dispatch("comment.reply", &reply(10, 11, "Thanks")).unwrap();
    net.settle();
    assert!(!ana.collab.pending.is_empty());

    net.enter(ana);
    assert!(net.ended.is_empty(), "{:?}", net.ended);
    assert!(net.person(2).collab.pending.is_empty());
    assert_eq!(net.collab(2).take_refusal(), None);
    let both = net.session(1).document().clone();
    assert_eq!(net.session(2).document(), &both);
    assert_eq!(both.canvases()[&canvas()].comments()[&cid(10)].replies.len(), 1);
}

#[test]
fn a_commenter_cant_undo_back_other_peoples_replies_and_a_refused_change_only_drops() {
    let (mut net, _) = with_link("comment");
    net.session(2).dispatch("comment.create", &note(10, "Lovely light")).unwrap();
    net.settle();
    net.session(1).dispatch("comment.reply", &reply(10, 11, "Thanks")).unwrap();
    net.settle();
    net.session(2).dispatch("comment.remove", &json!({"canvas": canvas(), "id": cid(10)})).unwrap();
    net.settle();
    assert!(!net.session(1).document().canvases()[&canvas()].comments().contains_key(&cid(10)));
    assert_eq!(net.session(2).dispatch("edit.undo", &json!({})).unwrap_err().kind, ErrorKind::Forbidden);

    // A change the relay won't make is dropped, and says why; Ana stays in.
    let mut forged = nodecraft_graph::Comment::new(cid(20), "Bo's", nodecraft_graph::Position { x: 0.0, y: 0.0 });
    forged.author = Some("Bo".into());
    let put = Frame::CommentPut { seq: 1, canvas: canvas(), id: cid(20), base: None, comment: Some(Box::new(forged)) };
    let out = net.room.handle(2, put);
    net.queue.extend(out);
    net.settle();
    assert!(net.ended.is_empty(), "{:?}", net.ended);
    assert!(net.collab(2).take_refusal().is_some());
    assert!(!net.session(1).document().canvases()[&canvas()].comments().contains_key(&cid(20)));
}

fn private_canvas() -> CanvasId {
    CanvasId::from_bytes([3; 16])
}

/// `sharer`'s project with a second canvas, kept private, whose card shows a file of its own.
fn sharer_with_a_private_canvas() -> (Session, Blake3) {
    let (mut session, _) = sharer();
    session.apply_remote(Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(private_canvas(), "Mine") }])).unwrap();
    let hash = session.import_asset(b"my own notes").unwrap();
    let mut card = Node::new(nid(7), "text");
    card.params.insert("image".into(), Value::Media(AssetRef { hash, mime: "text/plain".parse().unwrap(), meta: MediaMeta::default() }));
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: private_canvas(), node: card }])).unwrap();
    session.dispatch("canvas.set_private", &json!({"canvas": private_canvas(), "private": true})).unwrap();
    (session, hash)
}

#[test]
fn a_private_canvas_and_its_files_stay_with_who_keeps_it() {
    let (session, secret) = sharer_with_a_private_canvas();
    let mut net = Net::new();
    let collab = Collab::share(&session, room_id()).unwrap();
    net.enter(Person { id: 1, collab, session: Some(session) });
    net.enter(Person { id: 2, collab: Collab::join(room_id()), session: None });
    assert!(!net.session(2).document().canvases().contains_key(&private_canvas()));
    assert!(net.session(1).document().canvases()[&private_canvas()].is_private());
    assert!(!net.session(2).assets().contains(&secret));

    // Asked for by hash, the file still doesn't go out.
    net.collab(1).receive(Frame::AssetGet(vec![*secret.as_bytes()])).unwrap();
    let person = net.people.iter_mut().find(|p| p.id == 1).unwrap();
    let sent = person.collab.publish(person.session.as_ref().unwrap()).unwrap();
    assert!(!sent.iter().any(|frame| matches!(frame, Frame::AssetPut { .. })), "{sent:?}");

    // Shared again, it reaches everyone with its file; private again, it leaves them.
    net.session(1).dispatch("canvas.set_private", &json!({"canvas": private_canvas(), "private": false})).unwrap();
    net.settle();
    assert_eq!(net.session(2).document().canvases()[&private_canvas()].nodes().len(), 1);
    assert!(net.session(2).assets().contains(&secret));
    net.session(1).dispatch("canvas.set_private", &json!({"canvas": private_canvas(), "private": true})).unwrap();
    net.settle();
    assert!(!net.session(2).document().canvases().contains_key(&private_canvas()));
    assert!(net.session(1).document().canvases()[&private_canvas()].is_private());
    // Edits on it stay home too.
    net.session(1).dispatch("node.add", &json!({"canvas": private_canvas(), "node": Node::new(nid(8), "text")})).unwrap();
    net.settle();
    assert_eq!(net.session(1).document().canvases()[&private_canvas()].nodes().len(), 2);
    assert!(!net.session(2).document().canvases().contains_key(&private_canvas()));
}

#[test]
fn a_canvas_made_private_while_someone_changes_it_leaves_the_room_anyway() {
    let (mut session, _) = sharer();
    session.apply_remote(Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(private_canvas(), "Ours") }])).unwrap();
    let mut net = Net::new();
    let collab = Collab::share(&session, room_id()).unwrap();
    net.enter(Person { id: 1, collab, session: Some(session) });
    net.enter(Person { id: 2, collab: Collab::join(room_id()), session: None });
    assert!(net.session(2).document().canvases().contains_key(&private_canvas()));

    // Both act before either hears of the other.
    net.session(2).apply_remote(Changeset::new(vec![Change::SetCanvasTitle { canvas: private_canvas(), title: "Renamed".into() }])).unwrap();
    net.session(2).dispatch("node.add", &json!({"canvas": private_canvas(), "node": Node::new(nid(9), "text")})).unwrap();
    net.session(1).dispatch("canvas.set_private", &json!({"canvas": private_canvas(), "private": true})).unwrap();
    let mut sent = Vec::new();
    for person in &mut net.people {
        sent.extend(person.collab.publish(person.session.as_ref().unwrap()).unwrap().into_iter().map(|frame| (person.id, frame)));
    }
    for (from, frame) in sent {
        net.queue.extend(net.room.handle(from, frame));
    }
    net.settle();
    assert!(!net.session(2).document().canvases().contains_key(&private_canvas()));
    let kept = &net.session(1).document().canvases()[&private_canvas()];
    assert!(kept.is_private());
    assert_eq!(kept.title(), "Ours");
}

#[test]
fn only_a_role_that_edits_makes_a_canvas_private() {
    let (mut net, _) = with_link("comment");
    let error = net.session(2).dispatch("canvas.set_private", &json!({"canvas": canvas(), "private": true})).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Forbidden);
}

#[test]
fn a_room_change_that_clashes_with_a_private_canvas_waits_and_drops_nothing() {
    let (session, _) = sharer_with_a_private_canvas();
    let mut net = Net::new();
    let collab = Collab::share(&session, room_id()).unwrap();
    net.enter(Person { id: 1, collab, session: Some(session) });
    net.enter(Person { id: 2, collab: Collab::join(room_id()), session: None });
    // The editor can't see the private card, and reuses its id on the shared canvas.
    net.session(2).dispatch("node.add", &json!({"canvas": canvas(), "node": Node::new(nid(7), "text")})).unwrap();
    net.settle();
    assert!(net.ended.is_empty(), "{:?}", net.ended);
    assert!(net.collab(1).take_clash().is_some_and(|why| why.contains("Mine")));
    net.settle();
    assert_eq!(net.collab(1).take_clash(), None, "said once, until something changes");
    assert!(net.session(1).document().canvases()[&private_canvas()].nodes().contains_key(&nid(7)), "the private card is kept");
    assert!(!net.session(1).document().canvases()[&canvas()].nodes().contains_key(&nid(7)));
    assert!(net.session(2).document().canvases()[&canvas()].nodes().contains_key(&nid(7)), "the editor's card is kept");

    // Once the private canvas no longer holds that id, the room's change comes in.
    net.session(1).dispatch("node.remove", &json!({"canvas": private_canvas(), "id": nid(7)})).unwrap();
    net.settle();
    assert!(net.session(1).document().canvases()[&canvas()].nodes().contains_key(&nid(7)));
    assert!(net.session(1).document().canvases()[&private_canvas()].is_private());
    assert!(!net.session(2).document().canvases().contains_key(&private_canvas()));
}

/// `a` and `b` share `session`'s project, and `b`'s session holds it, its image too.
fn a_pair() -> (Session, Collab, Session, Collab) {
    let (mut session, _) = sharer();
    add(&mut session, 2);
    let mut a = Collab::share(&session, room_id()).unwrap();
    let mut b = Collab::join(room_id());
    let _ = b.hello();
    let replies = b.receive(Frame::Step1(Mirror::join().state_vector().unwrap())).unwrap();
    let Some(Frame::Step1(b_has)) = replies.into_iter().nth(1) else { panic!("b asks back") };
    let Some(Frame::Step2(everything)) = a.receive(Frame::Step1(b_has)).unwrap().into_iter().next() else { panic!("a answers") };
    b.receive(Frame::Step2(everything)).unwrap();
    let mut joined = Session::new(joining(b.project_id().unwrap().unwrap()).unwrap()).unwrap();
    for frame in b.settle(&mut joined).unwrap() {
        a.receive(frame).unwrap();
    }
    for frame in a.publish(&session).unwrap() {
        b.receive(frame).unwrap();
    }
    assert!(b.settle(&mut joined).unwrap().is_empty());
    assert_eq!(joined.document(), session.document());
    (session, a, joined, b)
}

/// What `collab` plans from a copy of `session`, as a host settling on another thread would.
fn plan_copy(collab: &mut Collab, session: &Session) -> (Vec<Frame>, Plan) {
    let copy = session.document().clone();
    let held: BTreeSet<Blake3> = session.assets().hashes().copied().collect();
    let has = |hash: &Blake3| held.contains(hash);
    let (out, plan) = collab.plan(&View { revision: session.revision(), document: &copy, has: &has }).unwrap();
    (out.into_iter().flat_map(|out| out.frames(session)).collect(), plan)
}

#[test]
fn a_plan_made_from_a_copy_is_taken_only_if_nothing_changed_meanwhile() {
    let (mut session, mut a, mut joined, mut b) = a_pair();
    add(&mut joined, 5);
    for frame in b.publish(&joined).unwrap() {
        a.receive(frame).unwrap();
    }
    let (out, plan) = plan_copy(&mut a, &session);
    assert!(out.is_empty() && plan.settles());
    // An edit lands before the plan comes back: the plan would undo it, so it isn't taken.
    add(&mut session, 8);
    assert!(matches!(plan.take(&mut session).unwrap(), Taken::Stale));
    assert!(!session.document().canvases()[&canvas()].nodes().contains_key(&nid(5)));
    assert!(a.taken(session.document(), session.revision(), false).unwrap().is_empty());

    // The next plan sends the edit and brings in the other's.
    let (out, plan) = plan_copy(&mut a, &session);
    assert!(matches!(&out[..], [Frame::Update(_)]), "{out:?}");
    assert!(matches!(plan.take(&mut session).unwrap(), Taken::Applied(Ok(()))));
    assert!(a.taken(session.document(), session.revision(), true).unwrap().is_empty());
    for frame in out {
        b.receive(frame).unwrap();
    }
    assert!(b.settle(&mut joined).unwrap().is_empty());
    assert_eq!(joined.document(), session.document());
    let cards = session.document().canvases()[&canvas()].nodes().keys().copied().collect::<Vec<_>>();
    assert_eq!(cards, vec![nid(1), nid(2), nid(5), nid(8)]);
    // Taken in, nothing more goes out.
    assert!(plan_copy(&mut a, &session).0.is_empty());
}

#[test]
fn a_joiner_may_edit_only_once_a_plan_brought_it_the_project() {
    let (session, mut a) = {
        let (session, _) = sharer();
        let a = Collab::share(&session, room_id()).unwrap();
        (session, a)
    };
    let mut b = Collab::join(room_id());
    let _ = b.hello();
    let replies = b.receive(Frame::Step1(Mirror::join().state_vector().unwrap())).unwrap();
    let Some(Frame::Step1(b_has)) = replies.into_iter().nth(1) else { panic!("b asks back") };
    let Some(Frame::Step2(everything)) = a.receive(Frame::Step1(b_has)).unwrap().into_iter().next() else { panic!("a answers") };
    b.receive(Frame::Step2(everything)).unwrap();
    let mut joined = Session::new(joining(session.document().id()).unwrap()).unwrap();
    joined.set_access(Access::new(Role::View, None).unwrap());
    let (_, plan) = plan_copy(&mut b, &joined);
    // Something else moved the session on meanwhile.
    joined.apply_remote(Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([7; 16]), "Aside") }])).unwrap();
    assert!(matches!(plan.take(&mut joined).unwrap(), Taken::Stale));
    let card = json!({"canvas": CanvasId::from_bytes([7; 16]), "node": Node::new(nid(9), "text")});
    assert!(joined.dispatch("node.add", &card).is_err(), "still only looking");
    b.taken(joined.document(), joined.revision(), false).unwrap();
    let (_, plan) = plan_copy(&mut b, &joined);
    assert!(matches!(plan.take(&mut joined).unwrap(), Taken::Applied(Ok(()))));
    assert!(joined.document().canvases().contains_key(&canvas()));
    let card = json!({"canvas": canvas(), "node": Node::new(nid(9), "text")});
    joined.dispatch("node.add", &card).unwrap();
}

#[test]
fn share_links_kept_apart_answer_and_say_again_what_they_hold() {
    let mut links = Links::default();
    assert_eq!(links.request(Some(Role::Edit), &json!({"op": "list"}), NOW).unwrap_err().kind, ErrorKind::Forbidden);
    let made = links.request(Some(Role::Owner), &json!({"op": "create", "role": "view", "name": "Ana"}), NOW).unwrap();
    assert!(matches!(&links.take()[..], [Frame::LinkPut { .. }]));
    assert!(links.take().is_empty());
    links.request(Some(Role::Owner), &json!({"op": "revoke", "id": made["id"]}), NOW).unwrap();
    links.request(Some(Role::Owner), &json!({"op": "create", "role": "edit", "name": "Bo"}), NOW).unwrap();
    // A new connection hears the revoke and the live link, and nothing twice.
    let again = links.again();
    assert!(matches!(&again[..], [Frame::LinkRevoke(_), Frame::LinkPut { .. }]), "{again:?}");
    assert!(links.take().is_empty());
}

#[test]
fn a_joiner_takes_the_shared_project_whole_and_only_into_its_empty_start() {
    let (session, _, mut joined, _) = a_pair();
    // The first catch-up replaced the empty start in one step (#714), with nothing to undo.
    assert!(joined.events_since(0).unwrap().iter().any(|event| event.command == "share.adopt"));
    assert!(!joined.can_undo());
    let whole = || SharedProject::new(session.document().clone()).unwrap();
    let held = joined.document().clone();
    assert_eq!(joined.adopt_shared(whole()).unwrap_err().kind, ErrorKind::Conflict, "a session with a document keeps it");
    assert_eq!(joined.document(), &held);
    let mut other = Session::new(joining(ProjectId::from_bytes([9; 16])).unwrap()).unwrap();
    assert_eq!(other.adopt_shared(whole()).unwrap_err().kind, ErrorKind::Conflict, "another project's start");
    // The shared project has an image this session hasn't been given.
    let mut empty = Session::new(joining(session.document().id()).unwrap()).unwrap();
    assert_eq!(empty.adopt_shared(whole()).unwrap_err().kind, ErrorKind::NotFound);
    assert_eq!(empty.document(), &joining(session.document().id()).unwrap());
}

/// Bytes that don't repeat, so a part out of place changes the hash.
fn noise(len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len);
    let mut seed = 0x9e37_79b9_u32;
    while bytes.len() < len {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        bytes.extend_from_slice(&seed.to_le_bytes());
    }
    bytes.truncate(len);
    bytes
}

#[test]
fn an_asset_too_large_for_one_frame_comes_in_parts() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), &dir.path().join("sharer"));
    // Larger than a window, and not a whole number of parts.
    let bytes: std::sync::Arc<[u8]> = noise(nodecraft_sync::WINDOW_BYTES as usize + 5 * nodecraft_sync::PART_BYTES / 2).into();
    let len = bytes.len() as u64;
    let range = nodecraft_format::asset_source::AssetRange::new(std::sync::Arc::new(nodecraft_format::asset_source::MemorySource::new(bytes)), 0, len).unwrap();
    let hash = session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap();
    let mut card = Node::new(nid(1), "text");
    card.params.insert("image".into(), Value::Media(AssetRef { hash, mime: "application/octet-stream".parse().unwrap(), meta: MediaMeta::default() }));
    session.apply_remote(Changeset::new(vec![Change::AddNode { canvas: canvas(), node: card }])).unwrap();

    let mut net = Net::new();
    net.disk = Some(dir.path().join("joiners"));
    net.enter(Person { id: 1, collab: Collab::share(&session, room_id()).unwrap(), session: Some(session) });
    let joiner = Collab::join(room_id()).with_spool(Box::new(MemorySpool::new(64 << 20)));
    net.enter(Person { id: 2, collab: joiner, session: None });
    let shared = net.session(1).document().clone();
    assert_eq!(net.session(2).document(), &shared);
    assert_eq!(net.session(2).assets().descriptor(&hash).map(|d| d.bytes), Some(len));
    let collab = &net.person(2).collab;
    assert_eq!((collab.waiting_for_assets(), collab.unusable_assets(), collab.asset_progress()), (0, 0, (0, 0)));
}

/// `b` has the project but not its image, which it has asked `a`'s session for.
fn asking() -> (Session, Collab, Session, Blake3) {
    let (mut session, asset) = sharer();
    add(&mut session, 2);
    let mut a = Collab::share(&session, room_id()).unwrap();
    let mut b = Collab::join(room_id());
    let _ = b.hello();
    let replies = b.receive(Frame::Step1(Mirror::join().state_vector().unwrap())).unwrap();
    let Some(Frame::Step1(b_has)) = replies.into_iter().nth(1) else { panic!("b asks back") };
    let Some(Frame::Step2(everything)) = a.receive(Frame::Step1(b_has)).unwrap().into_iter().next() else { panic!("a answers") };
    b.receive(Frame::Step2(everything)).unwrap();
    let mut joined = Session::new(joining(b.project_id().unwrap().unwrap()).unwrap()).unwrap();
    assert_eq!(b.settle(&mut joined).unwrap(), vec![Frame::AssetGet(vec![*asset.hash.as_bytes()])]);
    (session, b, joined, asset.hash)
}

fn part(hash: Blake3, total: u64, offset: u64, bytes: &[u8]) -> Frame {
    Frame::AssetPart { hash: *hash.as_bytes(), total, offset, bytes: bytes.to_vec() }
}

#[test]
fn parts_are_taken_in_order_and_a_new_connection_asks_for_the_rest() {
    let (session, mut b, mut joined, hash) = asking();
    let bytes = b"not really a png";
    let (head, tail) = bytes.split_at(6);
    // One that doesn't start at the beginning, or wasn't asked for, isn't taken.
    assert!(b.receive(part(hash, 16, 6, tail)).unwrap().is_empty());
    assert!(b.receive(part(Blake3::from_bytes([1; 32]), 16, 0, head)).unwrap().is_empty());
    assert_eq!(b.asset_progress(), (0, 0));
    b.receive(part(hash, 16, 0, head)).unwrap();
    assert_eq!(b.asset_progress(), (6, 16));
    // A copy of the same part, or one of another size, is someone else's and is dropped.
    b.receive(part(hash, 16, 0, head)).unwrap();
    b.receive(part(hash, 17, 6, tail)).unwrap();
    assert_eq!(b.asset_progress(), (6, 16));
    // The connection drops; the next asks only for the rest.
    let _ = b.hello();
    assert_eq!(b.settle(&mut joined).unwrap(), vec![Frame::AssetFrom { hash: *hash.as_bytes(), offset: 6, len: 10 }]);
    b.receive(part(hash, 16, 6, tail)).unwrap();
    assert!(b.settle(&mut joined).unwrap().is_empty());
    assert_eq!(joined.document(), session.document());
    assert_eq!(b.asset_progress(), (0, 0));
}

#[test]
fn an_asset_not_as_named_or_too_large_is_given_up_and_its_card_waits() {
    for total in [16, 64 << 20] {
        let (_, mut b, mut joined, hash) = asking();
        b.receive(part(hash, total, 0, b"not really")).unwrap();
        b.receive(part(hash, total, 10, b" a gif")).unwrap();
        assert_eq!((b.unusable_assets(), b.waiting_for_assets()), (1, 0), "{total}");
        // Not asked for again; the card stays out.
        assert!(b.settle(&mut joined).unwrap().is_empty());
        assert!(!joined.document().canvases()[&canvas()].nodes().contains_key(&nid(1)));
    }
}

#[test]
fn a_large_asset_is_sent_a_stretch_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = on_disk(empty(), dir.path());
    let len = nodecraft_sync::WINDOW_BYTES as usize + 3;
    let bytes: std::sync::Arc<[u8]> = noise(len).into();
    let range =
        nodecraft_format::asset_source::AssetRange::new(std::sync::Arc::new(nodecraft_format::asset_source::MemorySource::new(bytes.clone())), 0, len as u64)
            .unwrap();
    let hash = session.import_asset_from(&range, &Default::default(), &Default::default()).unwrap();
    // Asked whole: the first part, then as asked, a window at most.
    let mut frames = super::Out::Asset(hash, Stretch::Whole).frames(&session);
    assert_eq!(frames.len(), 1);
    let window = Stretch::From { offset: nodecraft_sync::PART_BYTES as u64, len: 3 * nodecraft_sync::WINDOW_BYTES };
    frames.extend(super::Out::Asset(hash, window).frames(&session));
    assert_eq!(frames.len(), 1 + nodecraft_sync::WINDOW_BYTES as usize / nodecraft_sync::PART_BYTES);
    let mut at = 0;
    for frame in &frames {
        let Frame::AssetPart { total, offset, bytes: part, .. } = frame else { panic!("{frame:?}") };
        assert_eq!((*total, *offset), (len as u64, at));
        assert_eq!(&part[..], &bytes[at as usize..at as usize + part.len()]);
        at += part.len() as u64;
    }
    // The rest, as asked; nothing past the end, nor of an asset it doesn't hold.
    assert_eq!(at, len as u64);
    let [Frame::AssetPart { offset, bytes: rest, .. }] = &super::Out::Asset(hash, Stretch::From { offset: at - 3, len: 100 }).frames(&session)[..] else {
        panic!()
    };
    assert_eq!((*offset, &rest[..]), (at - 3, &bytes[len - 3..]));
    assert!(super::Out::Asset(hash, Stretch::From { offset: len as u64, len: 1 }).frames(&session).is_empty());
    assert!(super::Out::Asset(Blake3::from_bytes([4; 32]), Stretch::Whole).frames(&session).is_empty());
}

#[test]
fn many_asks_are_answered_a_few_at_a_time() {
    let (session, asset) = sharer();
    let mut a = Collab::share(&session, room_id()).unwrap();
    for offset in 0..10 {
        assert!(a.receive(Frame::AssetFrom { hash: *asset.hash.as_bytes(), offset, len: 1 }).unwrap().is_empty());
    }
    let mut answered = 0;
    while a.owes_assets() {
        let sent = a.publish(&session).unwrap();
        assert!((1..=2).contains(&sent.len()), "{sent:?}");
        answered += sent.len();
    }
    assert_eq!(answered, 10);
    assert!(a.publish(&session).unwrap().is_empty());
}
