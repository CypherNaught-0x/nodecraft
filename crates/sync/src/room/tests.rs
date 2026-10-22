use super::*;
use crate::Mirror;
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
