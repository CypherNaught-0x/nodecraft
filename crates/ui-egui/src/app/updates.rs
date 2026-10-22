//! Checking for a newer release in the desktop app (#123): the engine's `updates.fetch` service.
//!
//! `app.check_updates` (palette, MCP, control channel, and once at startup unless settings turn
//! it off) asks the service, which only queues the request and answers `{pending: id}`. Each
//! frame [`UpdateChecker::tick`] hands a queued request to the check thread, which reads the
//! release list from GitHub, so a slow network never holds up a frame. A newer release shows a
//! banner with its notes, a way to dismiss it and a way to stop checking. The startup check is
//! silent unless it finds one.
//!
//! The banner also offers to install the release (#540, [`installer`]) when this copy can replace
//! itself: a signed build installed as an app bundle, AppImage or MSI. Nothing is downloaded until
//! the person asks, and nothing restarts until they ask again.
use nodecraft_engine::commands::Services;
use nodecraft_engine::settings::UpdateSettings;
use nodecraft_engine::update_check::{self, Available, Check, Release, UPDATE_SERVICE};
use nodecraft_engine::update_install::{self, APPLY_SERVICE, INSTALL_SERVICE, Install};
use nodecraft_engine::{Session, build_info::VERSION};
use nodecraft_providers::http::Http;
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

pub(crate) mod install;
mod installer;

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
    /// What updates this copy instead of the person, when something does.
    managed: Option<&'static str>,
    /// The release the last check that answered offered, which `app.install_update` installs.
    offered: Option<Available>,
    /// Installs the offered release, when this copy can replace itself.
    installer: Option<installer::Installer>,
    /// Why this copy can't replace itself, shown in the banner; `None` with no installer hides
    /// installing (a build without an update key, or one a package manager updates).
    cannot_install: Option<String>,
    /// What the last update did, said once at startup.
    pub(super) after_update: Option<String>,
}

/// What updates this copy, when it came from Flatpak, Snap or a Linux system package.
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
    /// The check thread, over its own runtime and HTTP client, registered as `updates.fetch`,
    /// and the installer, registered as `updates.install` and `updates.apply`, when this copy can
    /// replace itself. `updates` is the updates folder in NodeCraft's settings folder.
    pub(super) fn new(services: &mut Services, updates: Option<PathBuf>) -> Result<Self, Error> {
        // First, so this copy is counted as running even if the rest fails.
        let alone = updates.as_deref().is_some_and(install::hold_instance);
        let runtime = runtime("update check")?;
        let http = nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?;
        let releases = feed_base().map(|base| format!("{base}releases.json"));
        let (mut checker, mut service) = Self::spawn(Box::new(move || match &releases {
            Some(url) => runtime.block_on(update_check::fetch_from(&http as &dyn Http, url)),
            None => runtime.block_on(update_check::fetch(&http as &dyn Http)),
        }))?;
        services.register(UPDATE_SERVICE, move |request| service(request))?;
        checker.managed = managed();
        let kind = match installable(checker.managed, updates.as_deref()) {
            Ok((kind, updates, keys, target)) => {
                checker.after_update = install::after_start(&updates, Some(&kind), alone);
                let job = download_job(kind, updates.clone(), keys, target)?;
                let (installer, handlers) = installer::Installer::spawn(job, updates)?;
                let (mut install, mut apply) = (handlers.install, handlers.apply);
                services.register(INSTALL_SERVICE, move |request| install(request))?;
                services.register(APPLY_SERVICE, move |request| apply(request))?;
                checker.installer = Some(installer);
                return Ok(checker);
            }
            Err(why) => why,
        };
        if let Some(updates) = &updates {
            checker.after_update = install::after_start(updates, None, alone);
        }
        checker.cannot_install = kind;
        Ok(checker)
    }

    /// Installs with `job`, as [`Self::new`] does with the real download (tests).
    #[cfg(test)]
    fn with_installer(&mut self, job: installer::Job, updates: PathBuf) -> Result<installer::Handlers, Error> {
        let (installer, handlers) = installer::Installer::spawn(job, updates)?;
        self.installer = Some(installer);
        Ok(handlers)
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
        let checker =
            Self { asked, asks, done, quiet: None, banner: None, managed: None, offered: None, installer: None, cannot_install: None, after_update: None };
        Ok((checker, Box::new(service)))
    }

    /// Whether a check is queued or running.
    pub(super) fn is_waiting(&self) -> bool {
        let asked = self.asked.borrow();
        asked.queued.is_some() || asked.running.is_some()
    }

    /// Whether an install is queued or downloading.
    pub(super) fn is_installing(&self) -> bool {
        self.installer.as_ref().is_some_and(installer::Installer::is_waiting)
    }

    /// Where the install stands, when this copy can install.
    fn install_status(&self) -> Option<Option<Install>> {
        self.installer.as_ref().map(installer::Installer::status)
    }

    /// Hands on and records an install; true when the window should close to restart.
    pub(super) fn tick_install(&mut self, session: &mut Session, busy: bool) -> bool {
        let offered = self.offered.as_ref();
        self.installer.as_mut().is_some_and(|installer| installer.tick(session, offered, busy))
    }

    /// From `on_exit`: starts the updated copy when the window closed to restart.
    pub(super) fn relaunch(&self, project: Option<&Path>) {
        if let Some(installer) = &self.installer {
            installer.relaunch(project);
        }
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
            None => {
                log::info!("update check: {}", check.available.as_ref().map_or("up to date", |a| a.version.as_str()));
                self.offered = check.available.clone();
            }
        }
        // A dismissed release isn't offered again at startup; asking explicitly shows it.
        let offered = check.available.clone().filter(|available| !quiet || settings.dismissed.as_deref() != Some(available.version.as_str()));
        if offered.is_some() {
            self.banner = offered;
            return None;
        }
        if quiet {
            return None;
        }
        // An explicit check that offers nothing replaces whatever an earlier one offered.
        if check.error.is_none() {
            self.banner = None;
        }
        Some(notice(&check))
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

fn runtime(what: &str) -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the {what} thread: {e}")))
}

/// A test feed in place of GitHub's (`NODECRAFT_UPDATE_FEED`, a URL ending in `/`), in debug
/// builds only: release builds read and install only NodeCraft's releases.
fn feed_base() -> Option<String> {
    let base = std::env::var("NODECRAFT_UPDATE_FEED").ok().filter(|_| cfg!(debug_assertions))?;
    Some(if base.ends_with('/') { base } else { format!("{base}/") })
}

/// The keys feeds are checked against: the built-in release keys, or in debug builds those in
/// the file `NODECRAFT_UPDATE_KEYS` names.
fn keys() -> Result<Vec<ed25519_dalek::VerifyingKey>, Error> {
    match std::env::var_os("NODECRAFT_UPDATE_KEYS").filter(|_| cfg!(debug_assertions)) {
        Some(path) => {
            let text = std::fs::read_to_string(&path).map_err(|e| Error::new(ErrorKind::InvalidInput, format!("NODECRAFT_UPDATE_KEYS: {e}")))?;
            update_install::parse_keys(&text)
        }
        None => Ok(update_install::built_in_keys()),
    }
}

type Installable = (install::Kind, PathBuf, Vec<ed25519_dalek::VerifyingKey>, &'static str);

/// How this copy replaces itself, or why it can't (`None`: say nothing about installing).
fn installable(managed: Option<&str>, updates: Option<&Path>) -> Result<Installable, Option<String>> {
    let keys = keys().map_err(|error| {
        log::warn!("update keys unusable: {}", error.message);
        None
    })?;
    // A build without a release key can't trust any feed; the banner keeps its notes link.
    if keys.is_empty() || managed.is_some() {
        return Err(None);
    }
    let target = update_install::target().ok_or_else(|| Some("NodeCraft can't update itself on this platform.".to_owned()))?;
    let updates = updates.ok_or_else(|| Some("NodeCraft has no settings folder, so it can't install updates.".to_owned()))?;
    let kind = install::detect().map_err(|why| Some(format!("To install it, download it from the releases page: {why}.")))?;
    Ok((kind, updates.to_path_buf(), keys, target))
}

/// Downloads, verifies and stages a release, on the install thread.
fn download_job(kind: install::Kind, updates: PathBuf, keys: Vec<ed25519_dalek::VerifyingKey>, target: &'static str) -> Result<installer::Job, Error> {
    let runtime = runtime("update install")?;
    let http = nodecraft_providers::http::ReqwestHttp::new().map_err(|e| Error::new(ErrorKind::Unsupported, e.message))?;
    let base = feed_base().unwrap_or_else(|| update_install::DOWNLOADS.to_owned());
    Ok(Box::new(move |available| {
        let downloaded = runtime.block_on(update_install::download(&http as &dyn Http, &base, available, VERSION, target, &keys))?;
        install::stage(&kind, &downloaded, &updates)
    }))
}

/// Said when a banner choice can't be kept for the next start.
const UNSAVED: &str = "Settings can't be saved in this window, so NodeCraft will check for updates again next time it starts.";

/// What the banner's buttons asked for.
enum Chosen {
    Notes(String),
    Dismiss(String),
    StopChecking,
    Command(&'static str),
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
        let busy = self.runs.as_ref().is_some_and(|runs| runs.is_busy() || runs.has_waiting());
        if checker.tick_install(&mut self.session, busy) {
            // `on_exit` closes the project, then starts the new copy.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if checker.is_waiting() || checker.is_installing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// The banner offering a newer release, centred under the top row of canvas controls.
    pub(super) fn show_update_banner(&mut self, ui: &egui::Ui, viewport: egui::Rect, t: &crate::theme::Tokens) {
        let Some(updates) = &self.updates else { return };
        let Some(available) = updates.banner.clone() else { return };
        let (managed, cannot_install, install) = (updates.managed, updates.cannot_install.clone(), updates.install_status());
        let busy = self.runs.as_ref().is_some_and(|runs| runs.is_busy() || runs.has_waiting());
        let mut chosen = None;
        egui::Area::new(ui.id().with("update_banner"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(viewport.center().x, viewport.min.y + t.navigation_margin + t.navigation_toolbar_height + t.navigation_gap))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ui.ctx(), |ui| {
                egui::Frame::NONE.fill(t.panel_fill).inner_margin(t.card_padding).show(ui, |ui| {
                    ui.set_max_width((2.0 * t.card_width).min(viewport.width().max(0.0)));
                    let kind = if available.prerelease { "A pre-release, " } else { "" };
                    ui.label(format!("{kind}NodeCraft {} is available (this is {VERSION}).", available.version));
                    if let Some(manager) = managed {
                        ui.label(format!("Update it through {manager}."));
                    }
                    if let Some(why) = &cannot_install {
                        ui.weak(why);
                    }
                    if let Some(Some(Install::Failed { error, .. })) = &install {
                        ui.colored_label(t.danger, format!("The update didn't install: {error}"));
                    }
                    ui.horizontal(|ui| {
                        match &install {
                            Some(Some(Install::Downloading { version })) => {
                                ui.spinner();
                                ui.label(format!("Downloading and verifying NodeCraft {version}…"));
                            }
                            Some(Some(Install::Ready { version })) if *version == available.version => {
                                let restart = ui.add_enabled(!busy, egui::Button::new("Restart to update"));
                                let restart = restart.on_disabled_hover_text("A generation is running; restart when it finishes.");
                                if restart.clicked() {
                                    chosen = Some(Chosen::Command("app.restart_to_update"));
                                }
                            }
                            Some(status) => {
                                let label = if matches!(status, Some(Install::Failed { .. })) { "Try again" } else { "Install update" };
                                if ui.button(label).clicked() {
                                    chosen = Some(Chosen::Command("app.install_update"));
                                }
                            }
                            None => {}
                        }
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
            // Failures show in the banner; a refusal (busy, nothing ready) becomes the notice.
            Chosen::Command(id) => {
                if let Err(error) = self.session.dispatch(id, &json!({})) {
                    self.notice = Some(error.message);
                }
            }
            Chosen::Dismiss(version) => {
                match &mut self.store {
                    Some(store) => {
                        store.update(|settings| settings.updates.dismissed = Some(version));
                    }
                    None => self.notice = Some(UNSAVED.into()),
                }
                self.close_update_banner();
            }
            Chosen::StopChecking => {
                self.notice = Some(match &mut self.store {
                    Some(store) => {
                        store.update(|settings| settings.updates.check = false);
                        "NodeCraft won't check for updates at startup. Check for Updates… in the palette still checks.".into()
                    }
                    None => UNSAVED.into(),
                });
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
