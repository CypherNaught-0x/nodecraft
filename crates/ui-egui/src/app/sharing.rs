//! Sharing a project live from the desktop (#698), through a relay (`nodecraft-cli relay`).
//!
//! `share.start` asks the `share.session` service, which only queues the request; each frame
//! [`NodeCraftApp::tick_sharing`] takes it up: it names a new room, shares the session in it
//! ([`Collab`]) and opens a [`Connection`] to the relay on its own thread. From then on each frame
//! takes in what came, settles the session, and sends what that makes. A dropped connection
//! leaves the project open and editable; the thread reconnects, and the next hello catches both
//! sides up.
//!
//! `share.join` opens a new window ([`OpenWindow`]) and hands it the join link on its standard
//! input, never its command line, where other people on the machine could read it. That window
//! joins at startup ([`NodeCraftApp::join_live`]) and takes up the shared project once the first
//! update says which it is.
//!
//! The Share chip beside API keys opens the panel: how the session is doing, the join link
//! (hidden until asked for), and Stop. The room id never reaches a log, a notice or a command's
//! result.
use super::*;
use nodecraft_engine::collab::{Collab, JoinLink, Key, LINKS_SERVICE, joining, relay_address};
use nodecraft_engine::live_cmds::LIVE_SERVICE;
use nodecraft_sync::{Frame, Role};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

mod connection;

use connection::{Connection, Event};

/// Opens a join link in a new window; the app's starts NodeCraft again, tests record it.
pub(crate) type OpenWindow = Box<dyn FnMut(&str) -> Result<(), Error>>;

/// A host service handler, for the caller to register.
pub(crate) type Handler = Box<dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, Error>>;

/// The most requests waiting for the next frame.
const MAX_ASKS: usize = 8;

enum Ask {
    Start(String),
    Join(JoinLink),
    Stop,
}

/// What the services see between frames.
#[derive(Default)]
struct Shared {
    asks: VecDeque<Ask>,
    /// The relay settings name, if any.
    relay: Option<String>,
    /// Shared or joined, or asked to be.
    live: bool,
    status: serde_json::Value,
}

/// How the connection is going.
#[derive(Debug, Clone, PartialEq)]
enum State {
    Connecting,
    Connected(u64),
    Reconnecting { why: String, at: Instant },
}

struct Live {
    link: JoinLink,
    connection: Connection,
    state: State,
    /// A joiner whose session hasn't yet taken up the shared project.
    joining: bool,
}

/// The share panel's view state.
#[derive(Default)]
pub(crate) struct Panel {
    pub(crate) open: bool,
    show_link: bool,
    relay: String,
    join: String,
}

pub(crate) struct Sharing {
    shared: Rc<RefCell<Shared>>,
    collab: Rc<RefCell<Option<Collab>>>,
    live: Option<Live>,
    open_window: OpenWindow,
    pub(crate) panel: Panel,
}

impl Sharing {
    /// Sharing with `open_window`, and the `share.session` and `share.links` handlers it answers
    /// through.
    pub(crate) fn new(open_window: OpenWindow) -> (Self, [(&'static str, Handler); 2]) {
        let shared = Rc::new(RefCell::new(Shared { status: json!({"state": "off"}), ..Shared::default() }));
        let collab: Rc<RefCell<Option<Collab>>> = Rc::new(RefCell::new(None));
        let asks = Rc::clone(&shared);
        let session: Handler = Box::new(move |request| ask(&asks, request));
        let links = Rc::clone(&collab);
        let links: Handler = Box::new(move |request| {
            let mut collab = links.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "live sharing is busy"))?;
            let collab = collab.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this project isn't shared live"))?;
            collab.links_request(request, now())
        });
        let sharing = Self { shared, collab, live: None, open_window, panel: Panel::default() };
        (sharing, [(LIVE_SERVICE, session), (LINKS_SERVICE, links)])
    }

    /// Connects to `link`'s relay and takes part in its room through `collab`.
    fn begin(&mut self, ctx: &egui::Context, link: JoinLink, collab: Collab, joining: bool) -> Result<(), Error> {
        let wake = ctx.clone();
        let connection = Connection::open(link.relay().to_owned(), move || wake.request_repaint())?;
        *self.collab.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "live sharing is busy"))? = Some(collab);
        self.live = Some(Live { link, connection, state: State::Connecting, joining });
        if let Ok(mut shared) = self.shared.try_borrow_mut() {
            shared.live = true;
        }
        Ok(())
    }

    /// The relay settings name, which `share.start` uses when it isn't given one.
    pub(crate) fn set_relay(&mut self, relay: Option<String>) {
        if self.panel.relay.is_empty() {
            self.panel.relay = relay.clone().unwrap_or_default();
        }
        if let Ok(mut shared) = self.shared.try_borrow_mut() {
            shared.relay = relay;
        }
    }

    /// The chip's words: `None` while nothing is shared.
    pub(crate) fn chip(&self) -> String {
        let Some(live) = &self.live else { return "Share".into() };
        let collab = self.collab.try_borrow().ok();
        let collab = collab.as_ref().and_then(|collab| collab.as_ref());
        match (&live.state, collab) {
            (State::Connecting, _) => "Connecting…".into(),
            (State::Reconnecting { .. }, _) => "Reconnecting…".into(),
            (State::Connected(_), Some(collab)) if collab.waiting_for_assets() > 0 => format!("Live · {} files to come", collab.waiting_for_assets()),
            (State::Connected(_), Some(collab)) if !collab.caught_up() => "Live · catching up".into(),
            (State::Connected(_), _) => "Live".into(),
        }
    }

    fn status(&self) -> serde_json::Value {
        let Some(live) = &self.live else { return json!({"state": "off"}) };
        let collab = self.collab.try_borrow().ok();
        let collab = collab.as_ref().and_then(|collab| collab.as_ref());
        let role = collab.and_then(Collab::role);
        let waiting = collab.map_or(0, Collab::waiting_for_assets);
        let state = match &live.state {
            State::Connecting => "connecting",
            State::Reconnecting { .. } => "reconnecting",
            State::Connected(_) if waiting > 0 => "waiting",
            State::Connected(_) if !collab.is_some_and(Collab::caught_up) => "catching_up",
            State::Connected(_) => "live",
        };
        json!({"state": state, "role": role, "waiting_for_assets": waiting})
    }
}

/// The `share.session` handler: checks what it can now and leaves the rest for the frame.
fn ask(shared: &Rc<RefCell<Shared>>, request: &serde_json::Value) -> Result<serde_json::Value, Error> {
    let mut shared = shared.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "live sharing is busy"))?;
    let op = request.get("op").and_then(serde_json::Value::as_str);
    if op == Some("status") {
        return Ok(shared.status.clone());
    }
    if shared.asks.len() >= MAX_ASKS {
        return Err(Error::new(ErrorKind::Conflict, "live sharing has too much to do; try again"));
    }
    match op {
        Some("start") => {
            if shared.live {
                return Err(Error::new(ErrorKind::Conflict, "this project is shared live already"));
            }
            let relay = match request.get("relay").and_then(serde_json::Value::as_str) {
                Some(relay) => relay.to_owned(),
                None => shared
                    .relay
                    .clone()
                    .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "no relay to share through: give one, or set `[share] relay` in settings"))?,
            };
            let relay = relay_address(&relay)?;
            shared.live = true;
            shared.asks.push_back(Ask::Start(relay));
            Ok(json!({"sharing": true}))
        }
        Some("join") => {
            let link = request.get("link").and_then(serde_json::Value::as_str).unwrap_or_default().parse::<JoinLink>()?;
            shared.asks.push_back(Ask::Join(link));
            Ok(json!({"opening": true}))
        }
        Some("stop") => {
            shared.asks.push_back(Ask::Stop);
            Ok(json!({"sharing": false}))
        }
        _ => Err(Error::new(ErrorKind::InvalidInput, "a live sharing request isn't readable")),
    }
}

/// Seconds since 1970, by this machine's clock: when share links run out.
fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

/// Starts NodeCraft again with `--join`, and writes `link` to its standard input.
pub(crate) fn open_window(link: &str) -> Result<(), Error> {
    use std::io::Write;
    let exe = std::env::current_exe().map_err(|_| Error::new(ErrorKind::Unsupported, "NodeCraft can't tell where it is installed"))?;
    let mut child = std::process::Command::new(exe)
        .arg("--join")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .map_err(|_| Error::new(ErrorKind::Unsupported, "a new NodeCraft window can't be opened"))?;
    let written = child.stdin.take().map(|mut stdin| stdin.write_all(format!("{link}\n").as_bytes()));
    // Reaps the window when it closes; nothing waits for it.
    std::thread::Builder::new()
        .name("nodecraft-joined-window".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|_| Error::new(ErrorKind::Unsupported, "a new NodeCraft window can't be watched"))?;
    match written {
        Some(Ok(())) => Ok(()),
        _ => Err(Error::new(ErrorKind::Unsupported, "the new window didn't take the join link")),
    }
}

impl NodeCraftApp {
    /// Live sharing, its services registered on `services`; `None`, logged, if they can't be.
    pub(super) fn register_sharing(services: &mut nodecraft_engine::commands::Services, open_window: OpenWindow) -> Option<Sharing> {
        let (sharing, handlers) = Sharing::new(open_window);
        for (id, handler) in handlers {
            if let Err(error) = services.register(id, handler) {
                log::warn!("live sharing unavailable: {}", error.message);
                return None;
            }
        }
        Some(sharing)
    }

    /// Joins the project `link` shares (at startup, from `--join`), in place of the starter
    /// project this window opened with.
    pub fn join_live(&mut self, ctx: &egui::Context, link: &str) -> Result<(), Error> {
        let link: JoinLink = link.parse()?;
        let sharing = self.sharing.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this window can't share live"))?;
        if sharing.live.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "this window is shared live already"));
        }
        let collab = link.join();
        sharing.begin(ctx, link, collab, true)
    }

    fn start_sharing(&mut self, ctx: &egui::Context, relay: &str) -> Result<(), Error> {
        let link = JoinLink::new_room(relay)?;
        let collab = link.share(&self.session)?;
        let sharing = self.sharing.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this window can't share live"))?;
        if sharing.live.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "this project is shared live already"));
        }
        sharing.begin(ctx, link, collab, false)?;
        sharing.panel.open = true;
        Ok(())
    }

    /// Leaves the shared session; the project stays here as it is, the person's own again.
    fn stop_sharing(&mut self, said: Option<String>) {
        if let Some(sharing) = &mut self.sharing {
            if sharing.live.take().is_some() {
                self.session.set_access(nodecraft_engine::access::Access::owner());
            }
            if let Ok(mut collab) = sharing.collab.try_borrow_mut() {
                *collab = None;
            }
            sharing.panel.show_link = false;
            if let Ok(mut shared) = sharing.shared.try_borrow_mut() {
                shared.live = false;
                shared.status = json!({"state": "off"});
            }
        }
        if let Some(said) = said {
            self.notice = Some(said);
        }
    }

    /// One frame of live sharing: the requests that came, then the connection.
    pub(super) fn tick_sharing(&mut self, ctx: &egui::Context) {
        let Some(sharing) = &mut self.sharing else { return };
        let asks: Vec<Ask> = sharing.shared.try_borrow_mut().map(|mut shared| shared.asks.drain(..).collect()).unwrap_or_default();
        for ask in asks {
            match ask {
                Ask::Start(relay) => {
                    if let Err(error) = self.start_sharing(ctx, &relay) {
                        if self.sharing.as_ref().is_some_and(|sharing| sharing.live.is_none())
                            && let Some(Ok(mut shared)) = self.sharing.as_ref().map(|sharing| sharing.shared.try_borrow_mut())
                        {
                            shared.live = false;
                        }
                        self.notice = Some(format!("Couldn't share live: {}", error.message));
                    }
                }
                Ask::Join(link) => {
                    let result = self.sharing.as_mut().map_or(Ok(()), |sharing| (sharing.open_window)(&link.reveal()));
                    if let Err(error) = result {
                        self.notice = Some(format!("Couldn't join: {}", error.message));
                    }
                }
                Ask::Stop => self.stop_sharing(Some("Stopped sharing live. The project stays open here.".into())),
            }
        }
        if let Err(why) = self.drive_sharing() {
            self.stop_sharing(Some(format!("Live sharing ended: {why}")));
        }
        if let Some(sharing) = &self.sharing {
            let status = sharing.status();
            if let Ok(mut shared) = sharing.shared.try_borrow_mut() {
                shared.status = status;
            }
            if sharing.live.is_some() {
                // Local edits go out, and the reconnect countdown moves, without input.
                ctx.request_repaint_after(Duration::from_millis(250));
            }
        }
    }

    /// Takes in what the connection brought, settles the session and sends what that makes.
    /// An error ends the sharing.
    fn drive_sharing(&mut self) -> Result<(), String> {
        let Some(sharing) = &mut self.sharing else { return Ok(()) };
        let Some(live) = &mut sharing.live else { return Ok(()) };
        let mut slot = sharing.collab.try_borrow_mut().map_err(|_| "live sharing is busy".to_string())?;
        let Some(collab) = slot.as_mut() else { return Ok(()) };
        for event in live.connection.events() {
            match event {
                Event::Connected(n) => {
                    live.state = State::Connected(n);
                    live.send(n, vec![collab.hello()])?;
                }
                Event::Frame(bytes) => {
                    let State::Connected(n) = live.state else { continue };
                    let replies = Frame::decode(&bytes).and_then(|frame| collab.receive(frame)).map_err(|e| e.message)?;
                    live.send(n, replies)?;
                }
                Event::Dropped { why, retry } => live.state = State::Reconnecting { why, at: Instant::now() + retry },
            }
        }
        let State::Connected(n) = live.state else { return Ok(()) };
        // Opening another project here must not send it into the room.
        if !live.joining
            && let Some(id) = collab.project_id().map_err(|e| e.message)?
            && id != self.session.document().id()
        {
            return Err("another project was opened in this window".into());
        }
        let mut replaced = false;
        if live.joining
            && let Some(id) = collab.project_id().map_err(|e| e.message)?
        {
            self.session.adopt_joined(joining(id).map_err(|e| e.message)?).map_err(|e| e.message)?;
            live.joining = false;
            replaced = true;
        }
        if !live.joining {
            let frames = collab.settle(&mut self.session).map_err(|e| e.message)?;
            live.send(n, frames)?;
        }
        if let Some(why) = collab.take_refusal() {
            self.notice = Some(format!("The relay didn't take a change to a discussion: {why}"));
        }
        if let Some(why) = collab.take_clash() {
            self.notice = Some(why);
        }
        drop(slot);
        // A joiner shows the shared project's first canvas once it has one.
        if replaced || !self.session.document().canvases().contains_key(&self.canvas) {
            if let Some(canvas) = sheet_view::home_canvas(self.session.document()) {
                self.canvas = canvas;
            }
            self.selection.clear();
            self.editors.clear();
            self.centred = false;
        }
        Ok(())
    }

    /// The chip beside API keys: Share, or how the shared session is doing.
    pub(super) fn share_button(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        let Some(sharing) = &mut self.sharing else { return };
        let label = sharing.chip();
        let color = if sharing.live.is_some() { t.accent } else { t.text_muted };
        if ui.add(egui::Button::new(egui::RichText::new(label).color(color)).frame(false)).on_hover_text("Share this project live, or join one").clicked() {
            sharing.panel.open = !sharing.panel.open;
        }
    }

    /// The share panel, under its chip.
    pub(super) fn share_panel(&mut self, ctx: &egui::Context, t: &Tokens) {
        let anchor = [-t.navigation_margin, self.search_rect().max.y - self.viewport.min.y + t.navigation_gap];
        let Some(sharing) = &mut self.sharing else { return };
        if !sharing.panel.open {
            return;
        }
        let mut open = true;
        let mut command: Option<(&'static str, serde_json::Value)> = None;
        let frame =
            egui::Frame::NONE.inner_margin(t.card_padding).fill(t.panel_fill).stroke(egui::Stroke::new(1.0, t.panel_stroke)).corner_radius(t.panel_radius);
        egui::Window::new("Share live")
            .id(egui::Id::new("nodecraft.share"))
            .open(&mut open)
            .constrain_to(self.viewport)
            .anchor(egui::Align2::RIGHT_TOP, anchor)
            .collapsible(false)
            .resizable(false)
            .frame(frame)
            .show(ctx, |ui| {
                ui.set_max_width(t.minimap_height * 2.5);
                match &sharing.live {
                    None => idle(ui, t, &mut sharing.panel, &mut command),
                    Some(live) => {
                        let role = sharing.collab.try_borrow().ok().and_then(|collab| collab.as_ref().and_then(Collab::role));
                        shared(ui, t, live, role, &mut sharing.panel, &mut command);
                    }
                }
            });
        sharing.panel.open &= open;
        if let Some((id, params)) = command {
            if let Err(error) = self.session.dispatch(id, &params) {
                self.notice = Some(error.message);
            } else if id == "share.start"
                && let Some(relay) = params.get("relay").and_then(serde_json::Value::as_str)
            {
                // The relay someone shared through is the one they'll want next time.
                let relay = relay.trim().to_owned();
                if let Some(store) = &mut self.store {
                    store.update(|settings| settings.share.relay = Some(relay.clone()));
                }
                if let Some(sharing) = &mut self.sharing {
                    sharing.set_relay(Some(relay));
                }
            }
        }
    }
}

impl Live {
    fn send(&self, n: u64, frames: Vec<Frame>) -> Result<(), String> {
        for frame in frames {
            self.connection.send(n, frame.encode().map_err(|e| e.message)?);
        }
        Ok(())
    }
}

/// Nothing shared: share through a relay, or join with a link.
fn idle(ui: &mut egui::Ui, t: &Tokens, panel: &mut Panel, command: &mut Option<(&'static str, serde_json::Value)>) {
    ui.label(egui::RichText::new("Share this project with others through a relay. Everyone with the join link can edit it.").color(t.text_muted));
    ui.horizontal(|ui| {
        ui.label("Relay");
        ui.add(egui::TextEdit::singleline(&mut panel.relay).hint_text("ws://relay.example:7070").desired_width(t.minimap_height * 1.5));
    });
    if ui.add_enabled(!panel.relay.trim().is_empty(), egui::Button::new("Share")).clicked() {
        *command = Some(("share.start", json!({"relay": panel.relay.trim()})));
    }
    ui.separator();
    ui.label(egui::RichText::new("Join a shared project in a new window").color(t.text_muted));
    ui.horizontal(|ui| {
        // Masked: whoever sees a join link can use it.
        ui.add(egui::TextEdit::singleline(&mut panel.join).password(true).hint_text("Paste a join link").desired_width(t.minimap_height * 1.5));
        if ui.add_enabled(!panel.join.trim().is_empty(), egui::Button::new("Join")).clicked() {
            *command = Some(("share.join", json!({"link": panel.join.trim()})));
            panel.join.clear();
        }
    });
}

/// Shared or joined: how it's going, the join link on request, and Stop.
fn shared(ui: &mut egui::Ui, t: &Tokens, live: &Live, role: Option<Role>, panel: &mut Panel, command: &mut Option<(&'static str, serde_json::Value)>) {
    let state = match &live.state {
        State::Connecting => "Connecting to the relay…".to_string(),
        State::Connected(_) if live.joining => "Connected; waiting for the project…".into(),
        State::Connected(_) => "Connected.".into(),
        State::Reconnecting { why, at } => {
            let secs = at.saturating_duration_since(Instant::now()).as_secs();
            format!("Disconnected: {why}. Trying again in {secs} s; your changes here are kept.")
        }
    };
    ui.label(state);
    let role = match role {
        Some(Role::Owner) => "You share this project: you edit it and manage its links.",
        Some(Role::Edit) => "You can edit this project.",
        Some(Role::Comment) => "You can read this project and discuss it.",
        Some(Role::View) => "You can look at this project; changes aren't yours to make.",
        None => "Waiting for the relay to say what you may do.",
    };
    ui.label(egui::RichText::new(role).color(t.text_muted));
    if matches!(live.link.key(), Key::Room(_)) {
        ui.separator();
        ui.label(egui::RichText::new("Anyone with the join link can open and edit this project.").color(t.text_muted));
        ui.horizontal(|ui| {
            if ui.button("Copy join link").clicked() {
                ui.ctx().copy_text(live.link.reveal());
            }
            let toggle = if panel.show_link { "Hide" } else { "Show" };
            if ui.button(toggle).clicked() {
                panel.show_link = !panel.show_link;
            }
        });
        if panel.show_link {
            ui.add(egui::Label::new(egui::RichText::new(live.link.reveal()).monospace()).wrap());
        }
    }
    ui.separator();
    if ui.button("Stop sharing").clicked() {
        *command = Some(("share.stop", json!({})));
    }
}

#[cfg(test)]
mod tests;
