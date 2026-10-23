use super::*;
use crate::Mirror;
use crate::{Grant, OWNER_NAME, Person, Presence, Role};
use nodecraft_graph::{Canvas, Change, Changeset, Project};
use nodecraft_types::{CanvasId, ProjectId};

fn project() -> Project {
    let mut project = Project::new(ProjectId::from_bytes([1; 16]), "Shared").unwrap();
    Changeset::new(vec![Change::AddCanvas { canvas: Canvas::new(CanvasId::from_bytes([2; 16]), "Main") }]).apply(&mut project).unwrap();
    project
}

fn sent(out: &[Out], to: Peer) -> Vec<&Frame> {
    out.iter()
        .filter_map(|o| match o {
            Out::Send(peer, frame) | Out::Close(peer, frame) if *peer == to => Some(frame),
            _ => None,
        })
        .collect()
}

#[test]
fn a_late_joiner_gets_what_the_first_peer_shared() {
    let mut room = Room::new();
    let (sharer, _) = Mirror::share(&project()).unwrap();
    // The sharer joins, is asked what it has, and answers with all of it.
    let out = room.join(1);
    let [Frame::Step1(sv)] = sent(&out, 1)[..] else { panic!("{out:?}") };
    let update = sharer.update_for(sv).unwrap();
    assert!(room.handle(1, Frame::Step2(update)).is_empty());
    // A second peer joins later and asks.
    room.join(2);
    let mut joiner = Mirror::join();
    let out = room.handle(2, Frame::Step1(joiner.state_vector().unwrap()));
    let [Frame::Step2(update)] = sent(&out, 2)[..] else { panic!("{out:?}") };
    joiner.receive(update).unwrap();
    assert_eq!(joiner.materialize().unwrap().unwrap().project, project());
}

#[test]
fn updates_go_to_everyone_else() {
    let mut room = Room::new();
    for peer in 1..=3 {
        room.join(peer);
    }
    let (_, update) = Mirror::share(&project()).unwrap();
    let out = room.handle(2, Frame::Update(update.clone()));
    assert_eq!(out, vec![Out::Send(1, Frame::Update(update.clone())), Out::Send(3, Frame::Update(update))]);
}

#[test]
fn an_asset_goes_from_who_has_it_to_who_asked() {
    let mut room = Room::new();
    room.join(1);
    room.join(2);
    let bytes = b"image".to_vec();
    let hash = *blake3::hash(&bytes).as_bytes();
    // Peer 2 asks; the room has nothing, so it asks the others.
    let out = room.handle(2, Frame::AssetGet(vec![hash]));
    assert_eq!(out, vec![Out::Send(1, Frame::AssetGet(vec![hash]))]);
    // Peer 1 sends it; it goes on to peer 2, and the room keeps it for the next to ask.
    let out = room.handle(1, Frame::AssetPut { hash, bytes: bytes.clone() });
    assert_eq!(out, vec![Out::Send(2, Frame::AssetPut { hash, bytes: bytes.clone() })]);
    room.join(3);
    let out = room.handle(3, Frame::AssetGet(vec![hash]));
    assert_eq!(out, vec![Out::Send(3, Frame::AssetPut { hash, bytes })]);
}

#[test]
fn a_bad_frame_or_a_stranger_is_turned_away() {
    let mut room = Room::new();
    room.join(1);
    assert!(matches!(room.handle(1, Frame::Update(vec![0xff, 0xff, 0xff]))[..], [Out::Close(1, Frame::Refused(_))]));
    assert!(room.is_empty());
    assert!(matches!(room.handle(9, Frame::Step1(Vec::new()))[..], [Out::Close(9, _)]));
    for peer in 0..(MAX_PEERS as u64) {
        room.join(peer);
    }
    assert!(matches!(room.join(99)[..], [Out::Close(99, _)]));
}

#[test]
fn a_newcomer_is_asked_for_what_someone_still_waits_on() {
    let mut room = Room::new();
    room.join(1);
    room.join(2);
    let bytes = b"image".to_vec();
    let hash = *blake3::hash(&bytes).as_bytes();
    // Peer 2 asks; peer 1, who had it, leaves without answering.
    room.handle(2, Frame::AssetGet(vec![hash]));
    let _ = room.leave(1);
    // Peer 3 comes with it, is asked, and it goes on to peer 2.
    let out = room.join(3);
    assert!(out.contains(&Out::Send(3, Frame::AssetGet(vec![hash]))), "{out:?}");
    assert_eq!(room.handle(3, Frame::AssetPut { hash, bytes: bytes.clone() }), vec![Out::Send(2, Frame::AssetPut { hash, bytes })]);
}

fn part(total: u64, offset: u64, len: usize) -> Frame {
    Frame::AssetPart { hash: [3; 32], total, offset, bytes: vec![1; len] }
}

#[test]
fn parts_go_in_order_from_one_sender_to_who_asked() {
    let mut room = Room::new();
    for peer in 1..=3 {
        room.join(peer);
    }
    let out = room.handle(3, Frame::AssetGet(vec![[3; 32]]));
    assert_eq!(out.len(), 2, "both others are asked: {out:?}");
    // Both send the first part; the asker takes peer 1's, and only peer 1's after that.
    let (p, total) = (PART_BYTES as u64, PART_BYTES as u64 + 10);
    assert_eq!(room.handle(1, part(total, 0, PART_BYTES)), vec![Out::Send(3, part(total, 0, PART_BYTES))]);
    assert!(room.handle(2, part(total, 0, PART_BYTES)).is_empty());
    assert!(room.handle(2, part(total, p, 10)).is_empty());
    // A part out of order isn't passed on.
    assert!(room.handle(1, part(total, p + 5, 5)).is_empty());
    // The asker asks for more of its sender alone, from where it asked to before.
    let ask = Frame::AssetFrom { hash: [3; 32], offset: p, len: 10 };
    assert_eq!(room.handle(3, ask.clone()), vec![Out::Send(1, ask)]);
    assert!(room.handle(3, Frame::AssetFrom { hash: [3; 32], offset: 0, len: 10 }).is_empty());
    assert_eq!(room.handle(1, part(total, p, 10)), vec![Out::Send(3, part(total, p, 10))]);
    // Whole: nobody waits any more, and the room kept none of it.
    assert!(room.wanted.is_empty());
    assert!(room.assets.is_empty());
}

#[test]
fn asking_again_for_the_whole_starts_it_over() {
    let mut room = Room::new();
    for peer in 1..=3 {
        room.join(peer);
    }
    room.handle(3, Frame::AssetGet(vec![[3; 32]]));
    room.handle(1, part(100, 0, 40));
    // The asker let it go, and asks again: the first part is passed on once more, from anyone.
    assert_eq!(room.handle(3, Frame::AssetGet(vec![[3; 32]])).len(), 2);
    assert!(room.handle(1, part(100, 40, 60)).is_empty());
    assert_eq!(room.handle(2, part(100, 0, 40)), vec![Out::Send(3, part(100, 0, 40))]);
}

#[test]
fn an_asker_cannot_ask_for_more_than_it_takes() {
    let mut room = Room::new();
    room.join(1);
    room.handle(1, Frame::LinkPut { hash: [9; 32], grant: grant(Role::View, "Ana") });
    room.join_link(2, [9; 32], 0);
    let w = WINDOW_BYTES;
    // A first ask is passed on, capped at a window.
    assert_eq!(
        room.handle(2, Frame::AssetFrom { hash: [3; 32], offset: 5 * w, len: 3 * w }),
        vec![Out::Send(1, Frame::AssetFrom { hash: [3; 32], offset: 5 * w, len: w })]
    );
    // Others not following on, or past two windows ahead of what came, are not.
    for offset in [0, w, 7 * w] {
        assert!(room.handle(2, Frame::AssetFrom { hash: [3; 32], offset, len: 1 }).is_empty(), "{offset}");
    }
    assert_eq!(room.handle(2, Frame::AssetFrom { hash: [3; 32], offset: 6 * w, len: w }).len(), 1);
    assert!(room.handle(2, Frame::AssetFrom { hash: [3; 32], offset: 7 * w, len: 1 }).is_empty());
}

#[test]
fn when_a_sender_leaves_the_others_are_asked_for_the_rest() {
    let mut room = Room::new();
    for peer in 1..=3 {
        room.join(peer);
    }
    room.handle(3, Frame::AssetGet(vec![[3; 32]]));
    room.handle(1, part(100, 0, 40));
    let again = Frame::AssetFrom { hash: [3; 32], offset: 40, len: 60 };
    assert_eq!(room.leave(1), vec![Out::Send(2, again)]);
    // Peer 2 sends from there on, and its parts reach the asker.
    assert_eq!(room.handle(2, part(100, 40, 60)), vec![Out::Send(3, part(100, 40, 60))]);
    assert!(room.wanted.is_empty());
}

#[test]
fn a_bad_copy_is_asked_of_everyone_else_and_its_sender_is_not_heard_again() {
    let mut room = Room::new();
    for peer in 1..=4 {
        room.join(peer);
    }
    // Before anything came, there is no copy to be bad.
    room.handle(4, Frame::AssetGet(vec![[3; 32]]));
    assert!(room.handle(4, Frame::AssetBad([3; 32])).is_empty());
    room.handle(1, part(100, 0, 40));
    room.handle(1, part(100, 40, 60));
    // Peer 4 says the copy it took from peer 1 is bad: the others are asked from the start.
    assert_eq!(room.handle(4, Frame::AssetBad([3; 32])), vec![Out::Send(2, Frame::AssetGet(vec![[3; 32]])), Out::Send(3, Frame::AssetGet(vec![[3; 32]]))]);
    assert!(room.handle(1, part(100, 0, 40)).is_empty());
    // Asked for whole again, peer 1 still isn't heard; another's copy is.
    room.handle(4, Frame::AssetGet(vec![[3; 32]]));
    assert!(room.handle(1, part(100, 0, 40)).is_empty());
    assert_eq!(room.handle(2, part(100, 0, 40)), vec![Out::Send(4, part(100, 0, 40))]);
    // When that sender leaves, the rest isn't asked of peer 1.
    assert_eq!(room.leave(2), vec![Out::Send(3, Frame::AssetFrom { hash: [3; 32], offset: 40, len: 60 })]);
    // Nor does an ask for more go to it.
    room.handle(4, Frame::AssetGet(vec![[3; 32]]));
    assert_eq!(
        room.handle(4, Frame::AssetFrom { hash: [3; 32], offset: PART_BYTES as u64, len: 10 }),
        vec![Out::Send(3, Frame::AssetFrom { hash: [3; 32], offset: PART_BYTES as u64, len: 10 })]
    );
    // A bad copy whose sender isn't known is asked of everyone left.
    room.sent.clear();
    room.wanted.clear();
    assert_eq!(room.handle(4, Frame::AssetBad([3; 32])), vec![Out::Send(1, Frame::AssetGet(vec![[3; 32]])), Out::Send(3, Frame::AssetGet(vec![[3; 32]]))]);
}

#[test]
fn a_newcomer_is_asked_for_the_rest_nobody_sends() {
    let mut room = Room::new();
    room.join(1);
    room.join(3);
    room.handle(3, Frame::AssetGet(vec![[3; 32]]));
    room.handle(1, part(100, 0, 40));
    // The only one who had it leaves; nobody is left to ask.
    assert!(room.leave(1).is_empty());
    let out = room.join(2);
    assert!(out.contains(&Out::Send(2, Frame::AssetFrom { hash: [3; 32], offset: 40, len: 60 })), "{out:?}");
    assert!(!out.iter().any(|o| matches!(o, Out::Send(_, Frame::AssetGet(_)))), "{out:?}");
}

#[test]
fn only_an_editor_sends_parts_and_a_stretch_is_capped() {
    let mut room = Room::new();
    room.join(1);
    room.handle(1, Frame::LinkPut { hash: [9; 32], grant: grant(Role::View, "Ana") });
    room.join_link(2, [9; 32], 0);
    room.handle(1, Frame::AssetGet(vec![[3; 32]]));
    assert!(room.handle(2, part(10, 0, 4)).is_empty());
    // An ask past the most an asset may be is no frame at all.
    assert!(Frame::AssetFrom { hash: [3; 32], offset: crate::MAX_SHARED_ASSET_BYTES, len: 1 }.encode().is_err());
}

fn grant(role: Role, name: &str) -> Grant {
    Grant { role, name: name.into(), expires: None }
}

/// A room the owner (peer 1) has shared the project in, with one link of each role.
fn shared() -> (Room, Mirror) {
    let mut room = Room::new();
    let (owner, update) = Mirror::share(&project()).unwrap();
    room.join(1);
    room.handle(1, Frame::Step2(update));
    for (n, role, name) in [(1, Role::View, "Vi"), (2, Role::Comment, "Cy"), (3, Role::Edit, "Ed")] {
        assert!(room.handle(1, Frame::LinkPut { hash: [n; 32], grant: grant(role, name) }).is_empty());
    }
    (room, owner)
}

fn thread(author: &str) -> nodecraft_graph::Comment {
    let mut comment = nodecraft_graph::Comment::new(nodecraft_types::CommentId::from_bytes([7; 16]), "Here", nodecraft_graph::Position { x: 1.0, y: 2.0 });
    comment.author = Some(author.into());
    comment
}

fn put(seq: u64, base: Option<nodecraft_graph::Comment>, comment: Option<nodecraft_graph::Comment>) -> Frame {
    Frame::CommentPut {
        seq,
        canvas: CanvasId::from_bytes([2; 16]),
        id: nodecraft_types::CommentId::from_bytes([7; 16]),
        base: base.map(Box::new),
        comment: comment.map(Box::new),
    }
}

#[test]
fn a_link_lets_its_holder_in_with_its_role_and_only_while_it_lasts() {
    let (mut room, _) = shared();
    // A viewer is told its role and isn't asked for a document.
    assert_eq!(room.join_link(10, [1; 32], 0), vec![Out::Send(10, Frame::Welcome { role: Role::View, name: Some("Vi".into()) })]);
    // An editor is asked, as the owner is.
    let out = room.join_link(11, [3; 32], 0);
    assert!(matches!(sent(&out, 11)[..], [Frame::Welcome { role: Role::Edit, .. }, Frame::Step1(_)]), "{out:?}");
    // An unknown link, or one that has run out, gets nobody in.
    assert!(matches!(room.join_link(12, [9; 32], 0)[..], [Out::Close(12, Frame::Refused(_))]));
    room.handle(1, Frame::LinkPut { hash: [4; 32], grant: Grant { expires: Some(100), ..grant(Role::View, "Late") } });
    assert!(matches!(room.join_link(13, [4; 32], 99)[..], [Out::Send(13, Frame::Welcome { .. })]));
    assert!(matches!(room.join_link(14, [4; 32], 100)[..], [Out::Close(14, _)]));
    // When it runs out, whoever is in through it is put out, and the link is gone.
    assert_eq!(room.expire(100), vec![Out::Close(13, Frame::Refused("the share link has expired".into()))]);
    assert!(room.grant(&[4; 32]).is_none());
}

#[test]
fn a_viewer_or_commenter_cannot_write_the_document_or_manage_links() {
    let (mut room, owner) = shared();
    let update = owner.update_for(&[0]).unwrap();
    for (peer, link) in [(10, [1; 32]), (11, [2; 32])] {
        room.join_link(peer, link, 0);
        assert!(matches!(room.handle(peer, Frame::Update(update.clone()))[..], [Out::Close(p, Frame::Refused(_))] if p == peer));
        room.join_link(peer, link, 0);
        assert!(matches!(room.handle(peer, Frame::LinkRevoke([3; 32]))[..], [Out::Close(..)]));
        room.join_link(peer, link, 0);
        // Its media is dropped, and nobody asks it for any.
        assert!(room.handle(peer, Frame::AssetPut { hash: *blake3::hash(b"x").as_bytes(), bytes: b"x".to_vec() }).is_empty());
    }
    let out = room.handle(1, Frame::AssetGet(vec![[5; 32]]));
    assert!(out.is_empty(), "{out:?}");
    // An editor can't manage links either.
    room.join_link(12, [3; 32], 0);
    assert!(matches!(room.handle(12, Frame::LinkPut { hash: [6; 32], grant: grant(Role::Edit, "Me") })[..], [Out::Close(12, _)]));
    // Nor can a link make anyone the owner.
    assert!(matches!(room.handle(1, Frame::LinkPut { hash: [6; 32], grant: grant(Role::Owner, "Me") })[..], [Out::Close(1, _)]));
}

#[test]
fn revoking_or_changing_a_link_puts_its_holders_out() {
    let (mut room, _) = shared();
    room.join_link(10, [1; 32], 0);
    room.join_link(11, [1; 32], 0);
    room.join_link(12, [2; 32], 0);
    let out = room.handle(1, Frame::LinkRevoke([1; 32]));
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out.iter().all(|o| matches!(o, Out::Close(10 | 11, Frame::Refused(_)))));
    assert!(matches!(room.join_link(10, [1; 32], 0)[..], [Out::Close(10, _)]));
    // The same terms again change nothing; new ones put the holder out to come back on them.
    assert!(room.handle(1, Frame::LinkPut { hash: [2; 32], grant: grant(Role::Comment, "Cy") }).is_empty());
    assert!(matches!(room.handle(1, Frame::LinkPut { hash: [2; 32], grant: grant(Role::View, "Cy") })[..], [Out::Close(12, _)]));
}

#[test]
fn a_commenters_discussion_is_checked_written_and_sent_to_everyone() {
    let (mut room, mut owner) = shared();
    room.join_link(10, [2; 32], 0);
    // Under another's name: refused, and nothing written, but the commenter stays.
    assert!(matches!(&room.handle(10, put(1, None, Some(thread("Ed"))))[..], [Out::Send(10, Frame::CommentDone { seq: 1, refused: Some(_) })]));
    let out = room.handle(10, put(2, None, Some(thread("Cy"))));
    let [Out::Send(1, Frame::Update(update)), Out::Send(10, Frame::Update(_)), Out::Send(10, Frame::CommentDone { seq: 2, refused: None })] = &out[..] else {
        panic!("{out:?}")
    };
    owner.receive(update).unwrap();
    let project = owner.materialize().unwrap().unwrap().project;
    let comment = project.canvases().values().next().unwrap().comments().values().next().unwrap();
    assert_eq!(comment.author.as_deref(), Some("Cy"));
    // Putting the same again changes nothing, and is still acknowledged.
    assert_eq!(room.handle(10, put(3, None, Some(thread("Cy")))), vec![Out::Send(10, Frame::CommentDone { seq: 3, refused: None })]);
    // An editor writes discussions in the document instead.
    room.join_link(11, [3; 32], 0);
    assert!(matches!(room.handle(11, put(4, None, Some(thread("Ed"))))[..], [Out::Close(11, _)]));
}

#[test]
fn a_long_session_of_replaced_text_doesnt_fill_the_room() {
    let canvas = CanvasId::from_bytes([2; 16]);
    let card = nodecraft_types::NodeId::from_bytes([3; 16]);
    let mut project = project();
    Changeset::new(vec![Change::AddNode { canvas, node: nodecraft_graph::Node::new(card, "text") }]).apply(&mut project).unwrap();
    let (mut sharer, update) = Mirror::share(&project).unwrap();
    let mut room = Room::new();
    room.limit = 1 << 20;
    room.join(1);
    room.join(2);
    assert!(room.handle(1, Frame::Update(update)).len() == 1);
    // Twenty times the room's size, each edit replacing the last: all of it is taken.
    for n in 0..80u32 {
        let text = format!("{n:08}").repeat(32 << 10);
        Changeset::new(vec![Change::SetParam { canvas, id: card, name: "prompt".into(), value: Some(nodecraft_types::Value::Text(text)) }])
            .apply(&mut project)
            .unwrap();
        let update = sharer.publish(&project).unwrap().unwrap();
        let out = room.handle(1, Frame::Update(update));
        assert!(matches!(out[..], [Out::Send(2, Frame::Update(_))]), "edit {n}: {out:?}");
    }
    assert!(room.held() <= room.limit);
    // Text that stays is still counted: the room fills, and says so.
    let mut room = Room::new();
    room.limit = 1 << 20;
    room.join(1);
    let everything = sharer.update_for(&Mirror::join().state_vector().unwrap()).unwrap();
    assert!(room.handle(1, Frame::Step2(everything)).is_empty());
    let refused = (0..8u8).find_map(|n| {
        let card = nodecraft_types::NodeId::from_bytes([10 + n; 16]);
        let mut node = nodecraft_graph::Node::new(card, "text");
        node.params.insert("prompt".into(), nodecraft_types::Value::Text("x".repeat(256 << 10)));
        Changeset::new(vec![Change::AddNode { canvas, node }]).apply(&mut project).unwrap();
        let update = sharer.publish(&project).unwrap().unwrap();
        room.handle(1, Frame::Update(update)).into_iter().find(|out| matches!(out, Out::Close(1, Frame::Refused(_))))
    });
    assert!(matches!(refused, Some(Out::Close(1, Frame::Refused(why))) if why == "the room has grown too large"));
}

#[test]
fn a_room_measured_close_to_full_measures_again_before_refusing() {
    let canvas = CanvasId::from_bytes([2; 16]);
    let mut project = project();
    let (mut sharer, update) = Mirror::share(&project).unwrap();
    let mut room = Room::new();
    room.limit = 1 << 20;
    room.join(1);
    assert!(room.handle(1, Frame::Update(update)).is_empty());
    // Text that stays, up to nine tenths of the room.
    let card = nodecraft_types::NodeId::from_bytes([3; 16]);
    let mut node = nodecraft_graph::Node::new(card, "text");
    node.params.insert("prompt".into(), nodecraft_types::Value::Text("x".repeat(900 << 10)));
    Changeset::new(vec![Change::AddNode { canvas, node }]).apply(&mut project).unwrap();
    let out = room.handle(1, Frame::Update(sharer.publish(&project).unwrap().unwrap()));
    assert!(out.is_empty(), "{out:?}");
    // It is taken away, and more comes in than was left: the room measures, and takes it.
    Changeset::new(vec![Change::RemoveNode { canvas, id: card }]).apply(&mut project).unwrap();
    assert!(room.handle(1, Frame::Update(sharer.publish(&project).unwrap().unwrap())).is_empty());
    let card = nodecraft_types::NodeId::from_bytes([4; 16]);
    let mut node = nodecraft_graph::Node::new(card, "text");
    node.params.insert("prompt".into(), nodecraft_types::Value::Text("y".repeat(300 << 10)));
    Changeset::new(vec![Change::AddNode { canvas, node }]).apply(&mut project).unwrap();
    let out = room.handle(1, Frame::Update(sharer.publish(&project).unwrap().unwrap()));
    assert!(out.is_empty(), "{out:?}");
}

fn here(x: f32) -> Presence {
    Presence { canvas: Some(CanvasId::from_bytes([2; 16])), cursor: Some([x, 0.0]), selection: Vec::new() }
}

#[test]
fn where_each_peer_is_goes_to_the_others_under_the_name_the_relay_knows() {
    let (mut room, _) = shared();
    room.join_link(10, [1; 32], 0);
    // The owner moves: the viewer hears it as "Owner".
    let owner = Person { peer: 1, name: OWNER_NAME.into(), role: Role::Owner, presence: here(1.0) };
    assert_eq!(room.handle(1, Frame::Presence(here(1.0))), vec![Out::Send(10, Frame::Person(Box::new(owner.clone())))]);
    // The same place again sends nothing.
    assert!(room.handle(1, Frame::Presence(here(1.0))).is_empty());
    // A viewer may say where it is too, under its link's name.
    let viewer = Person { peer: 10, name: "Vi".into(), role: Role::View, presence: here(2.0) };
    assert_eq!(room.handle(10, Frame::Presence(here(2.0))), vec![Out::Send(1, Frame::Person(Box::new(viewer.clone())))]);
    // A newcomer is told where everyone already is.
    let out = room.join_link(11, [2; 32], 0);
    let told = sent(&out, 11);
    assert!(told.contains(&&Frame::Person(Box::new(owner))) && told.contains(&&Frame::Person(Box::new(viewer))), "{out:?}");
    // A peer that leaves is gone for the others; one that never said where it was leaves quietly.
    let mut gone = room.leave(10);
    gone.sort_by_key(|o| format!("{o:?}"));
    assert_eq!(gone, vec![Out::Send(1, Frame::Gone(10)), Out::Send(11, Frame::Gone(10))]);
    assert!(room.leave(11).is_empty());
}

#[test]
fn a_bad_presence_or_a_relay_frame_from_a_peer_is_refused() {
    let (mut room, _) = shared();
    room.join_link(10, [1; 32], 0);
    room.handle(10, Frame::Presence(here(2.0)));
    let nowhere = Presence { cursor: Some([f32::NAN, 0.0]), ..here(0.0) };
    let out = room.handle(10, Frame::Presence(nowhere));
    assert_eq!(out, vec![Out::Close(10, Frame::Refused("the presence isn't readable".into())), Out::Send(1, Frame::Gone(10))]);
    for frame in [Frame::Gone(1), Frame::Person(Box::new(Person { peer: 2, name: "Me".into(), role: Role::Owner, presence: here(0.0) }))] {
        room.join_link(10, [1; 32], 0);
        assert!(matches!(room.handle(10, frame)[..], [Out::Close(10, Frame::Refused(_))]));
    }
}

#[test]
fn a_revoked_link_holder_who_was_shown_is_gone_for_the_rest() {
    let (mut room, _) = shared();
    room.join_link(10, [1; 32], 0);
    room.handle(10, Frame::Presence(here(2.0)));
    let out = room.handle(1, Frame::LinkRevoke([1; 32]));
    assert_eq!(out, vec![Out::Close(10, Frame::Refused("the share link was revoked".into())), Out::Send(1, Frame::Gone(10))]);
}

#[test]
fn a_link_holder_who_was_shown_is_gone_for_the_rest_when_the_link_runs_out() {
    let (mut room, _) = shared();
    room.handle(1, Frame::LinkPut { hash: [4; 32], grant: Grant { expires: Some(100), ..grant(Role::View, "Late") } });
    room.join_link(10, [4; 32], 0);
    room.handle(10, Frame::Presence(here(2.0)));
    let out = room.expire(100);
    assert_eq!(out, vec![Out::Close(10, Frame::Refused("the share link has expired".into())), Out::Send(1, Frame::Gone(10))]);
}
