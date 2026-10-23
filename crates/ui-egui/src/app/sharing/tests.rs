//! Live sharing from the app (#698), against a small relay on this machine: one room, run by
//! `nodecraft_sync::Room` as `nodecraft-cli relay` runs it.
use super::*;
use nodecraft_engine::commands::Services;
use nodecraft_nodes::NodeKind;
use nodecraft_sync::{Frame, Out, Peer, Room};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

type Peers = Arc<Mutex<(Room, HashMap<Peer, Sender<Vec<u8>>>)>>;

/// A relay holding one room, whatever the hello names; its address.
fn relay() -> String {
    relay_peers().0
}

/// [`relay`], and who is in its room.
fn relay_peers() -> (String, Peers) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("ws://{}/", listener.local_addr().unwrap());
    let peers: Peers = Arc::new(Mutex::new((Room::new(), HashMap::new())));
    let room = Arc::clone(&peers);
    std::thread::spawn(move || {
        for (peer, stream) in listener.incoming().enumerate() {
            let peers = Arc::clone(&peers);
            std::thread::spawn(move || serve(peer as Peer, stream.unwrap(), &peers));
        }
    });
    (address, room)
}

fn deliver(outs: Vec<Out>, to: &HashMap<Peer, Sender<Vec<u8>>>) {
    for out in outs {
        let (Out::Send(peer, frame) | Out::Close(peer, frame)) = out;
        if let Some(sender) = to.get(&peer) {
            let _ = sender.send(frame.encode().unwrap());
        }
    }
}

fn serve(peer: Peer, stream: TcpStream, peers: &Peers) {
    let mut socket = tungstenite::accept(stream).unwrap();
    socket.get_ref().set_read_timeout(Some(Duration::from_millis(5))).unwrap();
    let (sender, inbox) = channel();
    let mut joined = false;
    loop {
        while let Ok(bytes) = inbox.try_recv() {
            if socket.send(tungstenite::Message::binary(bytes)).is_err() {
                return;
            }
        }
        match socket.read() {
            Ok(tungstenite::Message::Binary(bytes)) => {
                let frame = Frame::decode(&bytes).unwrap();
                let mut peers = peers.lock().unwrap();
                let (room, to) = &mut *peers;
                let outs = if joined {
                    room.handle(peer, frame)
                } else {
                    assert!(matches!(frame, Frame::Hello { .. }), "a connection starts with a hello");
                    joined = true;
                    to.insert(peer, sender.clone());
                    room.join(peer)
                };
                deliver(outs, to);
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => {
                let mut peers = peers.lock().unwrap();
                let (room, to) = &mut *peers;
                to.remove(&peer);
                deliver(room.leave(peer), to);
                return;
            }
        }
    }
}

/// An app that shares live, recording the windows it would open.
fn app(opened: &Rc<RefCell<Vec<String>>>) -> NodeCraftApp {
    let opened = Rc::clone(opened);
    let mut services = Services::default();
    let open: OpenWindow = Box::new(move |link| {
        opened.borrow_mut().push(link.to_owned());
        Ok(())
    });
    let sharing = NodeCraftApp::register_sharing(&mut services, open).unwrap();
    let mut app = NodeCraftApp::with_services(services).unwrap();
    app.sharing = Some(sharing);
    app
}

fn chip(app: &NodeCraftApp) -> String {
    app.sharing.as_ref().unwrap().chip()
}

/// Ticks `apps` until `done` holds.
fn until(apps: &mut [&mut NodeCraftApp], done: impl Fn(&[&mut NodeCraftApp]) -> bool) {
    let ctx = egui::Context::default();
    let started = Instant::now();
    while !done(apps) {
        assert!(started.elapsed() < Duration::from_secs(10), "never came: {:?}", apps.iter().map(|app| chip(app)).collect::<Vec<_>>());
        for app in apps.iter_mut() {
            app.tick_sharing(&ctx);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn link(app: &NodeCraftApp) -> String {
    app.sharing.as_ref().unwrap().live.as_ref().unwrap().link.reveal()
}

#[test]
fn a_shared_project_reaches_who_joins_and_stopping_keeps_it_there() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let relay = relay();
    let mut owner = app(&opened);
    assert_eq!(chip(&owner), "Share");
    assert_eq!(owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap(), json!({"sharing": true}));
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live");
    assert!(owner.sharing.as_ref().unwrap().panel.open, "the panel opens to show the join link");
    let status = owner.session.dispatch("share.status", &json!({})).unwrap();
    assert_eq!(status["state"], "live");
    let link = link(&owner);
    let room = link.rsplit('=').next().unwrap();
    assert!(!status.to_string().contains(room), "the status never holds the room id");
    // Sharing again is refused while this one lasts.
    assert_eq!(owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap_err().kind, ErrorKind::Conflict);

    // An edit after sharing reaches who joins later.
    let note = NodeId::from_bytes([0x5a; 16]);
    let node = nodecraft_nodes::text::kind().descriptor().instantiate(note, &BTreeMap::new()).unwrap();
    owner.session.dispatch("node.add", &json!({"canvas": owner.canvas, "node": node})).unwrap();

    let mut joiner = app(&opened);
    let ctx = egui::Context::default();
    joiner.join_live(&ctx, &link).unwrap();
    let has_note = |app: &NodeCraftApp| app.session.document().canvases().values().any(|canvas| canvas.nodes().contains_key(&note));
    assert!(!has_note(&joiner));
    // Nothing can be made in the joining window before the shared project comes: it would be lost.
    let early = nodecraft_nodes::text::kind().descriptor().instantiate(NodeId::from_bytes([0x5b; 16]), &BTreeMap::new()).unwrap();
    assert!(joiner.session.dispatch("node.add", &json!({"canvas": joiner.canvas, "node": early})).is_err());
    until(&mut [&mut owner, &mut joiner], |apps| has_note(apps[1]) && chip(apps[1]) == "Live");

    // And the joiner's edits reach the owner.
    let reply = NodeId::from_bytes([0x5c; 16]);
    let node = nodecraft_nodes::text::kind().descriptor().instantiate(reply, &BTreeMap::new()).unwrap();
    joiner.session.dispatch("node.add", &json!({"canvas": joiner.canvas, "node": node})).unwrap();
    let has_reply = |app: &NodeCraftApp| app.session.document().canvases().values().any(|canvas| canvas.nodes().contains_key(&reply));
    until(&mut [&mut owner, &mut joiner], |apps| has_reply(apps[0]));
    assert_eq!(joiner.session.document().id(), owner.session.document().id());
    let canvases = |app: &NodeCraftApp| app.session.document().canvases().keys().copied().collect::<Vec<_>>();
    assert_eq!(canvases(&joiner), canvases(&owner));
    assert!(joiner.session.document().canvases().contains_key(&joiner.canvas), "the joiner shows a shared canvas");
    assert_eq!(joiner.sharing.as_ref().unwrap().live.as_ref().unwrap().status.role, Some(Role::Owner), "a room id joins as its owner");

    // The joiner leaves; the shared project stays open in its window, the person's own.
    joiner.session.dispatch("share.stop", &json!({})).unwrap();
    until(&mut [&mut joiner], |apps| chip(apps[0]) == "Share");
    assert_eq!(joiner.session.document().id(), owner.session.document().id());
    assert_eq!(joiner.session.dispatch("share.status", &json!({})).unwrap(), json!({"state": "off"}));
    assert!(joiner.notice.as_deref().is_some_and(|notice| notice.contains("stays open")));
    assert!(opened.borrow().is_empty(), "nothing here opened a window");
    // Stopping again has nothing to stop.
    assert_eq!(joiner.session.dispatch("share.stop", &json!({})).unwrap_err().kind, ErrorKind::Conflict);
}

fn text_note(app: &mut NodeCraftApp, id: NodeId) {
    let node = nodecraft_nodes::text::kind().descriptor().instantiate(id, &BTreeMap::new()).unwrap();
    app.session.dispatch("node.add", &json!({"canvas": app.canvas, "node": node})).unwrap();
}

fn holds(app: &NodeCraftApp, id: NodeId) -> bool {
    app.session.document().canvases().values().any(|canvas| canvas.nodes().contains_key(&id))
}

fn live(app: &mut NodeCraftApp) -> &mut Live {
    app.sharing.as_mut().unwrap().live.as_mut().unwrap()
}

/// Review of #714: a plan taken while the connection was down, then an edit before it came back,
/// once settled the edit as the room's, and the next merge took it away.
#[test]
fn an_edit_made_while_disconnected_after_taking_a_plan_still_goes_out() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let relay = relay();
    let mut owner = app(&opened);
    owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap();
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live");
    let mut joiner = app(&opened);
    joiner.join_live(&egui::Context::default(), &link(&owner)).unwrap();
    until(&mut [&mut owner, &mut joiner], |apps| chip(apps[1]) == "Live" && !apps[1].session.document().canvases().is_empty());

    // The owner's edit comes in as a plan the joiner takes while its connection looks down.
    let first = NodeId::from_bytes([0x61; 16]);
    text_note(&mut owner, first);
    let ctx = egui::Context::default();
    let started = Instant::now();
    let connected = loop {
        assert!(started.elapsed() < Duration::from_secs(10), "the owner's edit never came");
        owner.tick_sharing(&ctx);
        joiner.tick_sharing(&ctx);
        if live(&mut joiner).in_flight {
            let State::Connected(n) = live(&mut joiner).state else { continue };
            live(&mut joiner).state = State::Reconnecting { why: "test".into(), at: Instant::now() + Duration::from_secs(60) };
            while live(&mut joiner).in_flight {
                joiner.tick_sharing(&ctx);
                std::thread::sleep(Duration::from_millis(2));
            }
            if holds(&joiner, first) {
                break State::Connected(n);
            }
            live(&mut joiner).state = State::Connected(n);
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    // Offline, the joiner adds its own; then the connection is back.
    let offline = NodeId::from_bytes([0x62; 16]);
    text_note(&mut joiner, offline);
    joiner.tick_sharing(&ctx);
    live(&mut joiner).state = connected;
    until(&mut [&mut owner, &mut joiner], |apps| holds(apps[0], offline));
    // And it stays, as the owner's next edit comes through.
    let second = NodeId::from_bytes([0x63; 16]);
    text_note(&mut owner, second);
    until(&mut [&mut owner, &mut joiner], |apps| holds(apps[1], second));
    assert!(holds(&joiner, offline) && holds(&owner, offline));
}

/// Review of #714: stopping left the worker thread and its connection running.
#[test]
fn stopping_closes_the_connection() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let (relay, peers) = relay_peers();
    let mut owner = app(&opened);
    owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap();
    let in_room = || peers.lock().unwrap().1.len();
    // The relay may count the owner in a moment after the owner counts itself live.
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live" && in_room() == 1);
    // Quiet first, so nothing from the relay is left to find the window gone.
    let ctx = egui::Context::default();
    let settled = Instant::now();
    while settled.elapsed() < Duration::from_millis(300) {
        owner.tick_sharing(&ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    owner.session.dispatch("share.stop", &json!({})).unwrap();
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Share");
    let started = Instant::now();
    while in_room() != 0 {
        assert!(started.elapsed() < Duration::from_secs(10), "the connection stayed open");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn opening_another_copy_of_the_project_ends_sharing_and_sends_nothing() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let relay = relay();
    let mut owner = app(&opened);
    owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap();
    assert_eq!(owner.session.dispatch("share.status", &json!({})).unwrap()["state"], "connecting", "status says so at once");
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live");
    // Another document with the same project id, as an older copy of the file would be.
    let id = owner.session.document().id();
    owner.session.adopt_joined(nodecraft_engine::collab::joining(id).unwrap()).unwrap();
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Share");
    assert!(owner.notice.as_deref().is_some_and(|notice| notice.contains("another project was opened")), "{:?}", owner.notice);
}

#[test]
fn joining_opens_a_window_with_the_link_and_never_hands_it_back() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut app = app(&opened);
    let link = format!("ws://relay.example:7070/#room={}", "ab".repeat(32));
    assert_eq!(app.session.dispatch("share.join", &json!({"link": link})).unwrap(), json!({"opening": true}));
    app.tick_sharing(&egui::Context::default());
    assert_eq!(*opened.borrow(), vec![link]);
    assert_eq!(chip(&app), "Share", "this window isn't shared by opening another");
    let bad = app.session.dispatch("share.join", &json!({"link": "ws://relay/#room=12"})).unwrap_err();
    assert_eq!(bad.kind, ErrorKind::InvalidInput);
}

#[test]
fn a_relay_that_isnt_there_leaves_the_project_as_it_was() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let relay = format!("ws://{}/", listener.local_addr().unwrap());
    drop(listener);
    let mut app = app(&opened);
    let before = app.session.document().clone();
    // No relay given and none in settings.
    assert_eq!(app.session.dispatch("share.start", &json!({})).unwrap_err().kind, ErrorKind::InvalidInput);
    app.sharing.as_mut().unwrap().set_relay(Some(relay));
    app.session.dispatch("share.start", &json!({})).unwrap();
    until(&mut [&mut app], |apps| chip(apps[0]) == "Reconnecting…");
    assert_eq!(app.session.dispatch("share.status", &json!({})).unwrap()["state"], "reconnecting");
    assert_eq!(*app.session.document(), before);
    app.session.dispatch("share.stop", &json!({})).unwrap();
    until(&mut [&mut app], |apps| chip(apps[0]) == "Share");
}

#[test]
fn the_share_chip_opens_the_panel() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut h = egui_kittest::Harness::builder()
        .with_size(Vec2::new(1200.0, 800.0))
        .with_max_steps(60)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), app(&opened));
    h.run();
    use egui_kittest::kittest::Queryable;
    h.get_by_label("Share").click();
    h.run();
    assert!(h.state().sharing.as_ref().unwrap().panel.open);
    h.get_by_label("Join a shared project in a new window");
    assert!(h.query_by_label("Stop sharing").is_none());
}

#[test]
#[ignore = "manual public-safe capture; SHARE698_RENDER_DIR"]
fn share698_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("SHARE698_RENDER_DIR").unwrap());
    let opened = Rc::new(RefCell::new(Vec::new()));
    let relay = relay();
    let mut owner = app(&opened);
    owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap();
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live");
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "share698-dark.png"), (crate::theme::ThemeKind::Light, "share698-light.png")] {
        owner.theme = theme;
        let mut shot = egui_kittest::Harness::builder()
            .wgpu()
            .with_size(Vec2::new(1200.0, 800.0))
            .with_max_steps(60)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), owner);
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        owner = shot.into_state();
    }
}

fn people(app: &NodeCraftApp) -> Vec<nodecraft_sync::Person> {
    app.sharing.as_ref().and_then(|sharing| sharing.live.as_ref()).map(|live| live.status.people.clone()).unwrap_or_default()
}

#[test]
fn each_person_sees_where_the_others_are_until_they_leave() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let relay = relay();
    let mut owner = app(&opened);
    owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap();
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live");
    let note = NodeId::from_bytes([0x6a; 16]);
    text_note(&mut owner, note);
    let mut joiner = app(&opened);
    joiner.join_live(&egui::Context::default(), &link(&owner)).unwrap();
    until(&mut [&mut owner, &mut joiner], |apps| holds(apps[1], note) && chip(apps[1]) == "Live");
    // The owner selects the note: the joiner sees it, under the name the relay gives.
    owner.selection.only(note);
    until(&mut [&mut owner, &mut joiner], |apps| people(apps[1]).first().is_some_and(|person| person.presence.selection == vec![note]));
    let seen = joiner.session.dispatch("share.status", &json!({})).unwrap()["people"].clone();
    assert_eq!(seen, json!([{"peer": seen[0]["peer"], "name": "Owner", "role": "owner", "canvas": owner.canvas, "cursor": null, "selected": 1}]));
    // And the owner sees the joiner, on the canvas it opened.
    let canvas = joiner.canvas;
    until(&mut [&mut owner, &mut joiner], |apps| people(apps[0]).first().is_some_and(|person| person.presence.canvas == Some(canvas)));
    assert_eq!(people(&owner).len(), 1);
    // On a canvas made private, nothing more is said about where the owner is.
    owner.session.dispatch("canvas.set_private", &json!({"canvas": owner.canvas, "private": true})).unwrap();
    until(&mut [&mut owner, &mut joiner], |apps| people(apps[1]).first().is_some_and(|person| person.presence == nodecraft_sync::Presence::default()));
    // Once the joiner leaves, it is gone for the owner.
    joiner.session.dispatch("share.stop", &json!({})).unwrap();
    until(&mut [&mut owner, &mut joiner], |apps| people(apps[0]).is_empty());
}

/// An owner sharing live, seeing a person called Robin pointing at `at` with `note` selected.
fn seen_by_owner(opened: &Rc<RefCell<Vec<String>>>, note: NodeId, at: [f32; 2]) -> NodeCraftApp {
    let mut owner = app(opened);
    owner.session.dispatch("share.start", &json!({"relay": relay()})).unwrap();
    until(&mut [&mut owner], |apps| chip(apps[0]) == "Live");
    text_note(&mut owner, note);
    let presence = nodecraft_sync::Presence { canvas: Some(owner.canvas), cursor: Some(at), selection: vec![note] };
    // The harness doesn't tick sharing, so nothing reported replaces this.
    live(&mut owner).status.people = vec![nodecraft_sync::Person { peer: 3, name: "Robin".into(), role: Role::Edit, presence }];
    owner
}

#[test]
fn another_persons_initial_takes_you_to_their_pointer() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let at = [4000.0, -2500.0];
    let owner = seen_by_owner(&opened, NodeId::from_bytes([0x6b; 16]), at);
    let mut h = egui_kittest::Harness::builder()
        .with_size(Vec2::new(1200.0, 800.0))
        .with_max_steps(60)
        .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), owner);
    h.run();
    use egui_kittest::kittest::Queryable;
    h.get_by_label("Robin").click();
    h.run();
    let app = h.state();
    let centre = app.view.to_screen(app.viewport.min, egui::pos2(at[0], at[1]));
    assert!((centre - app.viewport.center()).length() < 1.0, "{centre:?} isn't the middle of {:?}", app.viewport);
}

#[test]
#[ignore = "manual public-safe capture; PRESENCE120_RENDER_DIR"]
fn presence120_visual() {
    let dir = std::path::PathBuf::from(std::env::var_os("PRESENCE120_RENDER_DIR").unwrap());
    let opened = Rc::new(RefCell::new(Vec::new()));
    let note = NodeId::from_bytes([0x6c; 16]);
    let mut owner = seen_by_owner(&opened, note, [0.0, 0.0]);
    for (theme, name) in [(crate::theme::ThemeKind::Dark, "presence120-dark.png"), (crate::theme::ThemeKind::Light, "presence120-light.png")] {
        owner.theme = theme;
        let mut shot = egui_kittest::Harness::builder()
            .wgpu()
            .with_size(Vec2::new(1200.0, 800.0))
            .with_max_steps(60)
            .build_ui_state(|ui, app: &mut NodeCraftApp| app.show(ui), owner);
        crate::theme::apply(&shot.ctx, theme);
        shot.run();
        // Robin points just past the note's corner.
        let card = shot.state().painted.get(&note).copied().unwrap();
        let people = &mut live(shot.state_mut()).status.people;
        people[0].presence.cursor = Some([card.max.x + 30.0, card.center().y]);
        shot.run();
        shot.render().unwrap().save(dir.join(name)).unwrap();
        owner = shot.into_state();
    }
}

/// Cards in the large project.
const LARGE: usize = 3000;

/// `app`'s project with [`LARGE`] more cards on its canvas, each with a little text.
fn fill(app: &mut NodeCraftApp) {
    let canvas = app.canvas;
    let changes = (0..LARGE)
        .map(|n| {
            let id = NodeId::from_bytes(*blake3::hash(&n.to_le_bytes()).as_bytes().first_chunk::<16>().unwrap());
            let mut node = nodecraft_graph::Node::new(id, "source.text");
            node.params.insert("text".into(), nodecraft_types::Value::Text(format!("Card {n}: a line of words to carry")));
            nodecraft_graph::Change::AddNode { canvas, node }
        })
        .collect();
    app.session.apply_remote(nodecraft_graph::Changeset::new(changes)).unwrap();
}

#[test]
fn a_large_project_comes_in_without_a_frame_over_budget() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let relay = relay();
    let mut owner = app(&opened);
    fill(&mut owner);
    let cards = |app: &NodeCraftApp| app.session.document().canvases().values().map(|canvas| canvas.nodes().len()).sum::<usize>();
    let all = cards(&owner);
    let ctx = egui::Context::default();
    let mut slowest = Duration::ZERO;
    let mut tick = |app: &mut NodeCraftApp| {
        let started = Instant::now();
        app.tick_sharing(&ctx);
        slowest = slowest.max(started.elapsed());
    };
    owner.session.dispatch("share.start", &json!({"relay": relay})).unwrap();
    let started = Instant::now();
    while chip(&owner) != "Live" {
        assert!(started.elapsed() < Duration::from_secs(60), "never shared: {}", chip(&owner));
        tick(&mut owner);
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut joiner = app(&opened);
    joiner.join_live(&ctx, &link(&owner)).unwrap();
    let started = Instant::now();
    while !(cards(&joiner) == all && chip(&joiner) == "Live") {
        assert!(started.elapsed() < Duration::from_secs(60), "never came: {} of {all} cards, {}", cards(&joiner), chip(&joiner));
        tick(&mut owner);
        tick(&mut joiner);
        std::thread::sleep(Duration::from_millis(2));
    }
    let caught_up = started.elapsed();
    assert_eq!(joiner.session.document(), owner.session.document());

    // The same catch-up as changesets in one frame, as it was before: far over the budget.
    let mut session = nodecraft_engine::session::Session::new(joining(owner.session.document().id()).unwrap()).unwrap();
    let started = Instant::now();
    for set in nodecraft_graph::diff::diff(session.document(), owner.session.document()).unwrap() {
        session.apply_remote(set).unwrap();
    }
    let in_one_frame = started.elapsed();
    assert_eq!(session.document(), owner.session.document());
    eprintln!("slowest frame {slowest:?}; catch-up {caught_up:?}; in one frame {in_one_frame:?}");
    // Both measurements do the same catch-up work; comparing them avoids a machine-load-sensitive
    // wall-clock limit while still catching frames that consume a substantial share of that work.
    assert!(slowest.as_secs_f64() < in_one_frame.as_secs_f64() * 0.1, "a frame took {slowest:?}, at least 10% of the catch-up in one frame ({in_one_frame:?})");
}

#[test]
fn the_desktop_refuses_a_wss_relay_when_asked() {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut app = app(&opened);
    let refused = app.session.dispatch("share.start", &json!({"relay": "wss://relay.example/"})).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Unsupported);
    assert_eq!(app.session.dispatch("share.status", &json!({})).unwrap()["state"], "off");
    let link = format!("wss://relay.example/#room={}", "ab".repeat(32));
    assert_eq!(app.session.dispatch("share.join", &json!({"link": link})).unwrap_err().kind, ErrorKind::Unsupported);
    app.tick_sharing(&egui::Context::default());
    assert!(opened.borrow().is_empty(), "no window is opened for a relay it can't reach");
}

#[test]
fn an_https_page_reaches_wss_or_this_machine() {
    for relay in ["wss://relay.example/", "ws://localhost:7070/", "ws://127.0.0.1:7070/", "ws://[::1]:7070/", "ws://app.localhost/"] {
        assert!(can_reach(relay, true, true).is_ok(), "{relay}");
    }
    for relay in ["ws://relay.example:7070/", "ws://localhost.example/", "ws://128.0.0.1/"] {
        assert_eq!(can_reach(relay, true, true).unwrap_err().kind, ErrorKind::Unsupported, "{relay}");
        assert!(can_reach(relay, true, false).is_ok(), "{relay}");
    }
    assert!(can_reach("wss://relay.example/", false, false).is_err());
    assert!(can_reach("ws://relay.example/", false, false).is_ok());
}
