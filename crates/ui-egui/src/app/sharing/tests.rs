//! Live sharing from the app (#698), against a small relay on this machine: one room, run by
//! `nodecraft_sync::Room` as `nodecraft-cli relay` runs it.
use super::*;
use nodecraft_engine::commands::Services;
use nodecraft_nodes::NodeKind;
use nodecraft_sync::{Out, Peer, Room};
use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

type Peers = Arc<Mutex<(Room, HashMap<Peer, Sender<Vec<u8>>>)>>;

/// A relay holding one room, whatever the hello names; its address.
fn relay() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("ws://{}/", listener.local_addr().unwrap());
    let peers: Peers = Arc::new(Mutex::new((Room::new(), HashMap::new())));
    std::thread::spawn(move || {
        for (peer, stream) in listener.incoming().enumerate() {
            let peers = Arc::clone(&peers);
            std::thread::spawn(move || serve(peer as Peer, stream.unwrap(), &peers));
        }
    });
    address
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
                peers.1.remove(&peer);
                peers.0.leave(peer);
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
    until(&mut [&mut owner, &mut joiner], |apps| has_note(apps[1]) && chip(apps[1]) == "Live");
    assert_eq!(joiner.session.document().id(), owner.session.document().id());
    let canvases = |app: &NodeCraftApp| app.session.document().canvases().keys().copied().collect::<Vec<_>>();
    assert_eq!(canvases(&joiner), canvases(&owner));
    assert!(joiner.session.document().canvases().contains_key(&joiner.canvas), "the joiner shows a shared canvas");
    assert!(joiner.sharing.as_ref().unwrap().collab.borrow().as_ref().is_some_and(|collab| collab.role() == Some(Role::Owner)), "a room id joins as its owner");

    // The joiner leaves; the shared project stays open in its window, the person's own.
    joiner.session.dispatch("share.stop", &json!({})).unwrap();
    until(&mut [&mut joiner], |apps| chip(apps[0]) == "Share");
    assert_eq!(joiner.session.document().id(), owner.session.document().id());
    assert_eq!(joiner.session.dispatch("share.status", &json!({})).unwrap(), json!({"state": "off"}));
    assert!(joiner.notice.as_deref().is_some_and(|notice| notice.contains("stays open")));
    assert!(opened.borrow().is_empty(), "nothing here opened a window");
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
