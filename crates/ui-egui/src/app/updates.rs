//! Checking for a newer release in the desktop app (#123): the engine's `updates.fetch` service.
//!
//! `app.check_updates` (palette, MCP, control channel, and once at startup unless settings turn
//! it off) asks the service, which only queues the request and answers `{pending: id}`. Each
//! frame [`UpdateChecker::tick`] hands a queued request to the check thread, which reads the
//! release list from GitHub, so a slow network never holds up a frame. A newer release shows a
//! banner with its notes, a way to dismiss it and a way to stop checking; nothing is downloaded
//! or installed, and nothing restarts. The startup check is silent unless it finds one.
use nodecraft_engine::commands::Services;
use nodecraft_engine::settings::UpdateSettings;
use nodecraft_engine::update_check::{self, Available, Check, Release, UPDATE_SERVICE};
use nodecraft_engine::{Session, build_info::VERSION};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

/// Reads the release list; the app's calls GitHub, tests pass a fixture.
pub(super) type Fetch = Box<dyn FnMut() -> Result<Vec<Release>, Error> + Send>;

/// The `updates.fetch` handler [`UpdateChecker::spawn`] registers.
pub(super) type Service = Box<dyn FnMut(&serde_json::Value) -> Result<serde_json::Value, Error>>;

type Done = (u64, Result<Vec<Release>, Error>);

/// What the service asked for and the frame hasn't handed on.
#[derive(Default)]
struct Asked {
    next: u64,
    queued: Option<u64>,
    /// The check handed to the thread and not yet answered.
    running: Option<u64>,
}

pub(super) struct UpdateChecker {
    asked: Rc<RefCell<Asked>>,
    asks: SyncSender<u64>,
    done: Receiver<Done>,
    /// The startup check, which says nothing unless it finds a release.
    quiet: Option<u64>,
    /// The release the banner offers.
    pub(super) banner: Option<Available>,
}

/// How this copy was installed, when something other than NodeCraft updates it.
fn managed() -> Option<&'static str> {
    if std::env::var_os("FLATPAK_ID").is_some() {
        return Some("Flatpak");
    }
    if std::env::var_os("SNAP").is_some() {
        return Some("Snap");
    }
    let exe = std::env::current_exe().ok()?;
    (cfg!(target_os = "linux") && exe.starts_with("/usr/") && !exe.starts_with("/usr/local/")).then_some("your package manager")
}

impl UpdateChecker {
    /// The check thread, over its own runtime and HTTP client, registered as `updates.fetch`.
    pub(super) fn new(services: &mut Services) -> Result<Self, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the update check thread: {e}")))?;
        let http = nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?;
        let (checker, mut service) = Self::spawn(Box::new(move || runtime.block_on(update_check::fetch(&http as &dyn Http))))?;
        services.register(UPDATE_SERVICE, move |request| service(request))?;
        Ok(checker)
    }

    /// The check thread over `fetch`, and the `updates.fetch` handler that queues for it.
    pub(super) fn spawn(mut fetch: Fetch) -> Result<(Self, Service), Error> {
        let (asks, inbox) = sync_channel::<u64>(1);
        let (outbox, done) = sync_channel::<Done>(1);
        std::thread::Builder::new()
            .name("nodecraft-updates".into())
            .spawn(move || {
                for id in inbox {
                    // A reply that panics the client fails this check, not the thread.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut fetch))
                        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "checking for updates failed unexpectedly")));
                    if outbox.send((id, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the update check thread: {e}")))?;
        let asked = Rc::new(RefCell::new(Asked::default()));
        let queue = asked.clone();
        let service = move |_: &serde_json::Value| {
            let mut asked = queue.borrow_mut();
            if asked.queued.is_some() || asked.running.is_some() {
                return Err(Error::new(ErrorKind::Busy, "NodeCraft is already checking for updates; try again when that finishes"));
            }
            asked.next = asked.next.saturating_add(1);
            let id = asked.next;
            asked.queued = Some(id);
            Ok(json!({"pending": id}))
        };
        Ok((Self { asked, asks, done, quiet: None, banner: None }, Box::new(service)))
    }

    /// Whether a check is queued or running.
    pub(super) fn is_waiting(&self) -> bool {
        let asked = self.asked.borrow();
        asked.queued.is_some() || asked.running.is_some()
    }

    /// Hands a queued request to the check thread and records a finished check, returning what to
    /// tell the person about it.
    pub(super) fn tick(&mut self, session: &mut Session, settings: &UpdateSettings) -> Option<String> {
        let queued = self.asked.borrow_mut().queued.take();
        if let Some(id) = queued {
            if self.asks.try_send(id).is_err() {
                return self.told(session, settings, id, Err(Error::new(ErrorKind::Unsupported, "the update check thread stopped")));
            }
            self.asked.borrow_mut().running = Some(id);
        }
        let (id, result) = match self.done.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                let id = self.asked.borrow().running?;
                (id, Err(Error::new(ErrorKind::Unsupported, "the update check thread stopped")))
            }
        };
        self.told(session, settings, id, result)
    }

    fn told(&mut self, session: &mut Session, settings: &UpdateSettings, id: u64, result: Result<Vec<Release>, Error>) -> Option<String> {
        self.asked.borrow_mut().running = None;
        let check = update_check::finish(session, id, settings.channel, result);
        let quiet = self.quiet == Some(id);
        if quiet {
            self.quiet = None;
        }
        match &check.error {
            Some(error) => log::info!("update check failed: {error}"),
            None => log::info!("update check: {}", check.available.as_ref().map_or("up to date", |a| a.version.as_str())),
        }
        // A dismissed release isn't offered again at startup; asking explicitly shows it.
        let offered = check.available.clone().filter(|available| !quiet || settings.dismissed.as_deref() != Some(available.version.as_str()));
        if offered.is_some() {
            self.banner = offered;
            return None;
        }
        (!quiet).then(|| notice(&check))
    }
}

/// One line about a check the person asked for that offers nothing.
pub(super) fn notice(check: &Check) -> String {
    match (&check.error, &check.available) {
        (Some(error), _) => format!("Couldn't check for updates: {error}"),
        (None, Some(available)) => format!("NodeCraft {} is available.", available.version),
        (None, None) => format!("NodeCraft {} is up to date.", check.current),
    }
}

/// What the banner's buttons asked for.
enum Chosen {
    Notes(String),
    Dismiss(String),
    StopChecking,
}

impl super::NodeCraftApp {
    /// Asks for the startup check, when settings allow it.
    pub(super) fn check_updates_at_startup(&mut self, settings: &UpdateSettings) {
        if !settings.check || self.updates.is_none() {
            return;
        }
        let pending = self.session.dispatch("app.check_updates", &json!({}));
        let id = pending.ok().and_then(|answer| answer.get("pending").and_then(serde_json::Value::as_u64));
        if let (Some(updates), Some(id)) = (&mut self.updates, id) {
            updates.quiet = Some(id);
        }
    }

    /// Hands on and records update checks; repaints while one runs so its answer is seen.
    pub(super) fn tick_updates(&mut self, ctx: &egui::Context) {
        let Some(checker) = &mut self.updates else {
            return;
        };
        let settings = self.store.as_ref().map(|store| store.get().updates.clone()).unwrap_or_default();
        if let Some(notice) = checker.tick(&mut self.session, &settings) {
            self.notice = Some(notice);
        }
        if checker.is_waiting() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// The banner offering a newer release.
    pub(super) fn show_update_banner(&mut self, ui: &egui::Ui, t: &crate::theme::Tokens) {
        let Some(available) = self.updates.as_ref().and_then(|updates| updates.banner.clone()) else {
            return;
        };
        let mut chosen = None;
        egui::Area::new(ui.id().with("update_banner"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_TOP, egui::Vec2::new(0.0, t.card_padding))
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding).show(ui, |ui| {
                    ui.set_max_width(2.0 * t.card_width);
                    let kind = if available.prerelease { "A pre-release, " } else { "" };
                    ui.label(format!("{kind}NodeCraft {} is available (this is {VERSION}).", available.version));
                    if let Some(manager) = managed() {
                        ui.label(format!("Update it through {manager}."));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Release notes").clicked() {
                            chosen = Some(Chosen::Notes(available.url.clone()));
                        }
                        if ui.button("Dismiss").clicked() {
                            chosen = Some(Chosen::Dismiss(available.version.clone()));
                        }
                        if ui.button("Don't check at startup").clicked() {
                            chosen = Some(Chosen::StopChecking);
                        }
                    });
                });
            });
        let Some(chosen) = chosen else { return };
        match chosen {
            // The url is on NodeCraft's releases page: `update_check` keeps no other.
            Chosen::Notes(url) => ui.ctx().open_url(egui::OpenUrl::new_tab(url)),
            Chosen::Dismiss(version) => {
                if let Some(store) = &mut self.store {
                    store.update(|settings| settings.updates.dismissed = Some(version));
                }
                self.close_update_banner();
            }
            Chosen::StopChecking => {
                if let Some(store) = &mut self.store {
                    store.update(|settings| settings.updates.check = false);
                }
                self.notice = Some("NodeCraft won't check for updates at startup. Check for Updates… in the palette still checks.".into());
                self.close_update_banner();
            }
        }
    }

    fn close_update_banner(&mut self) {
        if let Some(updates) = &mut self.updates {
            updates.banner = None;
        }
    }
}

#[cfg(test)]
mod tests;
