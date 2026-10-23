//! Sharing a project live from the desktop (#698), through a relay (`nodecraft-cli relay`).
//!
//! `share.start` asks the `share.session` service, which only queues the request; each frame
//! [`NodeCraftApp::tick_sharing`] takes it up: it names a new room, shares the session in it
//! ([`Collab`]) and hands both to a [`Worker`] thread, which opens a [`connection::Connection`] to the relay
//! on a thread of its own. The worker takes in what came and works out what the session must
//! take to match the room (#714); each frame hands it a copy of the document if that changed,
//! and applies the changesets that come back if nothing was edited meanwhile. A dropped
//! connection leaves the project open and editable; the thread reconnects, and the next hello
//! catches both sides up.
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
use nodecraft_engine::access::Access;
use nodecraft_engine::collab::{Collab, JoinLink, Key, LINKS_SERVICE, Links, Out, Taken, joining, relay_address};
use nodecraft_engine::live_cmds::LIVE_SERVICE;
use nodecraft_engine::session::DocumentLineage;
use nodecraft_sync::Role;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

mod connection;
mod spool;
mod worker;

use connection::Event;
use worker::{Status, Turn, Worker};

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
    worker: Worker,
    state: State,
    /// As of the worker's last report.
    status: Status,
    /// A joiner whose session hasn't yet taken up the shared project.
    joining: bool,
    /// The document being shared; sharing ends if another takes its place in this window.
    lineage: Option<DocumentLineage>,
    /// The session revision whose document the worker last had.
    sent: Option<u64>,
    /// A turn the worker hasn't answered yet.
    in_flight: bool,
    /// Something came in that the next turn settles.
    dirty: bool,
}

/// The owner's share links, kept here so `share.links` is answered at once however busy the
/// worker is.
#[derive(Default)]
struct Desk {
    links: Links,
    role: Option<Role>,
    live: bool,
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
    desk: Rc<RefCell<Desk>>,
    live: Option<Live>,
    open_window: OpenWindow,
    pub(crate) panel: Panel,
}

impl Sharing {
    /// Sharing with `open_window`, and the `share.session` and `share.links` handlers it answers
    /// through.
    pub(crate) fn new(open_window: OpenWindow) -> (Self, [(&'static str, Handler); 2]) {
        let shared = Rc::new(RefCell::new(Shared { status: json!({"state": "off"}), ..Shared::default() }));
        let desk: Rc<RefCell<Desk>> = Rc::default();
        let asks = Rc::clone(&shared);
        let session: Handler = Box::new(move |request| ask(&asks, request));
        let links = Rc::clone(&desk);
        let links: Handler = Box::new(move |request| {
            let mut desk = links.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "live sharing is busy"))?;
            if !desk.live {
                return Err(Error::new(ErrorKind::Unsupported, "this project isn't shared live"));
            }
            let role = desk.role;
            desk.links.request(role, request, now())
        });
        let sharing = Self { shared, desk, live: None, open_window, panel: Panel::default() };
        (sharing, [(LIVE_SERVICE, session), (LINKS_SERVICE, links)])
    }

    /// Connects to `link`'s relay and takes part in its room through the [`Collab`] `make` makes.
    /// A session that can import `large` assets takes those too large for one frame.
    fn begin(
        &mut self,
        ctx: &egui::Context,
        link: JoinLink,
        make: impl FnOnce() -> Result<Collab, Error> + Send + 'static,
        joining: bool,
        large: bool,
    ) -> Result<(), Error> {
        let wake = ctx.clone();
        let mut desk = self.desk.try_borrow_mut().map_err(|_| Error::new(ErrorKind::Conflict, "live sharing is busy"))?;
        // Who comes in by the room id owns it; a link's role comes from the relay.
        let status = Status { role: matches!(link.key(), Key::Room(_)).then_some(Role::Owner), ..Status::default() };
        // Assets too large for one frame come in parts, kept in files until imported (#700).
        let make = move || make().map(|collab| if large { collab.with_spool(Box::new(spool::FileSpool::default())) } else { collab });
        let worker = Worker::spawn(link.relay().to_owned(), make, move || wake.request_repaint())?;
        *desk = Desk { links: Links::default(), role: status.role, live: true };
        drop(desk);
        self.live = Some(Live { link, worker, state: State::Connecting, status, joining, lineage: None, sent: None, in_flight: false, dirty: false });
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

    /// The chip's words: "Share" while nothing is shared, else how sharing is going.
    pub(crate) fn chip(&self) -> String {
        let Some(live) = &self.live else { return "Share".into() };
        let status = &live.status;
        match &live.state {
            State::Connecting => "Connecting…".into(),
            State::Reconnecting { .. } => "Reconnecting…".into(),
            State::Connected(_) if status.waiting_for_assets > 0 => match status.asset_progress {
                (received, total) if total > 0 => format!("Live · {} files to come · {}%", status.waiting_for_assets, received.saturating_mul(100) / total),
                _ => format!("Live · {} files to come", status.waiting_for_assets),
            },
            State::Connected(_) if status.unusable_assets > 0 => format!("Live · {} files can't come", status.unusable_assets),
            State::Connected(_) if !live.caught_up() => "Live · catching up".into(),
            State::Connected(_) => "Live".into(),
        }
    }

    fn status(&self) -> serde_json::Value {
        let Some(live) = &self.live else { return json!({"state": "off"}) };
        let role = live.status.role;
        let waiting = live.status.waiting_for_assets;
        let state = match &live.state {
            State::Connecting => "connecting",
            State::Reconnecting { .. } => "reconnecting",
            State::Connected(_) if waiting > 0 => "waiting",
            State::Connected(_) if !live.caught_up() => "catching_up",
            State::Connected(_) => "live",
        };
        let (received, total) = live.status.asset_progress;
        json!({
            "state": state,
            "role": role,
            "waiting_for_assets": waiting,
            "asset_bytes_received": received,
            "asset_bytes_total": total,
            "unusable_assets": live.status.unusable_assets,
        })
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
            shared.status = json!({"state": "connecting"});
            shared.asks.push_back(Ask::Start(relay));
            Ok(json!({"sharing": true}))
        }
        Some("join") => {
            let link = request.get("link").and_then(serde_json::Value::as_str).unwrap_or_default().parse::<JoinLink>()?;
            shared.asks.push_back(Ask::Join(link));
            Ok(json!({"opening": true}))
        }
        Some("stop") => {
            if !shared.live {
                return Err(Error::new(ErrorKind::Conflict, "this project isn't shared live"));
            }
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
        let large = self.session.assets().context().supports_large_assets();
        sharing.begin(ctx, link, move || Ok(collab), true, large)?;
        // Nothing made here before the shared project comes would survive taking it in.
        self.session.set_access(Access::new(Role::View, None)?);
        Ok(())
    }

    fn start_sharing(&mut self, ctx: &egui::Context, relay: &str) -> Result<(), Error> {
        let link = JoinLink::new_room(relay)?;
        let sharing = self.sharing.as_mut().ok_or_else(|| Error::new(ErrorKind::Unsupported, "this window can't share live"))?;
        if sharing.live.is_some() {
            return Err(Error::new(ErrorKind::Conflict, "this project is shared live already"));
        }
        let (project, revision) = (self.session.document().clone(), self.session.revision());
        let room = link.clone();
        let large = self.session.assets().context().supports_large_assets();
        sharing.begin(ctx, link, move || room.share_project(&project, revision), false, large)?;
        sharing.panel.open = true;
        if let Some(live) = &mut sharing.live {
            live.lineage = Some(self.session.document_lineage());
        }
        // The relay someone shared through is the one they'll want next time.
        sharing.set_relay(Some(relay.to_owned()));
        if let Some(store) = &mut self.store {
            store.update(|settings| settings.share.relay = Some(relay.to_owned()));
        }
        Ok(())
    }

    /// Leaves the shared session; the project stays here as it is, the person's own again.
    fn stop_sharing(&mut self, said: Option<String>) {
        if let Some(sharing) = &mut self.sharing {
            if sharing.live.take().is_some() {
                self.session.set_access(Access::owner());
            }
            if let Ok(mut desk) = sharing.desk.try_borrow_mut() {
                *desk = Desk::default();
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
                Ask::Stop => {
                    let said = if self.session.file_status().available {
                        "Stopped sharing live. The project stays open here."
                    } else {
                        "Stopped sharing live. The project stays open here, but this window can't save it."
                    };
                    self.stop_sharing(Some(said.into()));
                }
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

    /// Takes in what the worker reported, applies what it planned, and hands it the session as
    /// it now is. An error ends the sharing.
    fn drive_sharing(&mut self) -> Result<(), String> {
        let Some(sharing) = &mut self.sharing else { return Ok(()) };
        let Some(live) = &mut sharing.live else { return Ok(()) };
        let mut desk = sharing.desk.try_borrow_mut().map_err(|_| "live sharing is busy".to_string())?;
        let mut plans = Vec::new();
        for report in live.worker.reports()? {
            for event in report.events {
                match event {
                    // The relay forgets links while the room is empty; each connection tells it again.
                    Event::Connected(n) => {
                        live.state = State::Connected(n);
                        live.worker.queue(desk.links.again())?;
                    }
                    Event::Dropped { why, retry } => live.state = State::Reconnecting { why, at: Instant::now() + retry },
                    Event::Frame(_) => {}
                }
            }
            if let Some(why) = report.ended {
                return Err(why);
            }
            live.status = report.status;
            live.dirty |= report.dirty;
            if let Some(notice) = report.notices.into_iter().last() {
                self.notice = Some(notice);
            }
            if let Some((n, asks)) = report.assets {
                let frames = asks.into_iter().flat_map(|(hash, stretch)| Out::Asset(hash, stretch).frames(&self.session)).collect();
                live.worker.send(n, frames)?;
            }
            plans.extend(report.plan);
        }
        desk.role = live.status.role;
        live.worker.queue(desk.links.take())?;
        drop(desk);
        // Opening another project here, or another copy of this one, must not send it into the
        // room.
        let replaced_here = live.lineage.as_ref().is_some_and(|lineage| !self.session.is_current_document(lineage));
        if !live.joining && (replaced_here || live.status.project_id.is_some_and(|id| id != self.session.document().id())) {
            return Err("another project was opened in this window".into());
        }
        let mut replaced = false;
        if live.joining
            && let Some(id) = live.status.project_id
        {
            self.session.adopt_joined(joining(id).map_err(|e| e.message)?).map_err(|e| e.message)?;
            live.joining = false;
            live.lineage = Some(self.session.document_lineage());
            live.sent = None;
            replaced = true;
        }
        for plan in plans {
            live.in_flight = false;
            let replaces = plan.replaces();
            match plan.take(&mut self.session).map_err(|e| e.message)? {
                Taken::Nothing => {}
                // Edited meanwhile: the next turn plans again from the newer document.
                Taken::Stale => {
                    live.worker.taken(self.session.revision(), None, false)?;
                    live.dirty = true;
                }
                Taken::Applied(applied) => {
                    applied.map_err(|e| e.message)?;
                    // Said now, with the document as taken: an edit before the next turn is
                    // the session's own, to publish.
                    let revision = self.session.revision();
                    live.worker.taken(revision, Some(self.session.document().clone()), true)?;
                    live.sent = Some(revision);
                    // A joiner took the shared project in whole: this window's document still.
                    if replaces {
                        live.lineage = Some(self.session.document_lineage());
                        replaced = true;
                    }
                }
            }
        }
        if matches!(live.state, State::Connected(_)) && !live.joining && !live.in_flight && (live.dirty || live.sent != Some(self.session.revision())) {
            let revision = self.session.revision();
            // The copy is the one cost left here; it is made only when the document changed.
            let document = (live.sent != Some(revision)).then(|| self.session.document().clone());
            let assets = self.session.assets().hashes().copied().collect();
            live.worker.turn(Turn { revision, document, assets })?;
            live.sent = Some(revision);
            live.in_flight = true;
            live.dirty = false;
        }
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
                    Some(live) => shared(ui, t, live, live.status.role, &mut sharing.panel, &mut command),
                }
            });
        sharing.panel.open &= open;
        if let Some((id, params)) = command
            && let Err(error) = self.session.dispatch(id, &params)
        {
            self.notice = Some(error.message);
        }
    }
}

impl Live {
    /// Whether this session holds what the room holds, as far as it has heard.
    fn caught_up(&self) -> bool {
        self.status.caught_up && !self.dirty
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
