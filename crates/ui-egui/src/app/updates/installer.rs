//! Installing the offered release (#540): the engine's `updates.install` and `updates.apply`
//! services.
//!
//! `app.install_update` asks `updates.install`, which only queues the release the last check
//! offered; each frame [`Installer::tick`] hands it to the install thread, which downloads it,
//! verifies the signed feed and payload (`update_install::download`) and stages it
//! ([`install::stage`]). `app.restart_to_update` asks `updates.apply`, which swaps the staged copy
//! in ([`install::apply`]) unless a generation is running; the frame then closes the window and
//! [`Installer::relaunch`], from `on_exit`, starts the new copy once the project is closed.
use super::install::{self, Relaunch, Staged};
use nodecraft_engine::Session;
use nodecraft_engine::build_info::VERSION;
use nodecraft_engine::update_check::Available;
use nodecraft_engine::update_install::{self, Install};
use nodecraft_types::{Error, ErrorKind};
use serde_json::json;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

/// Downloads, verifies and stages a release; the app's uses GitHub, tests pass a fixture.
pub(super) type Job = Box<dyn FnMut(&Available) -> Result<Staged, Error> + Send>;

type Done = (String, Result<Staged, Error>);

/// Where the install stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Phase {
    #[default]
    Idle,
    /// Asked for, not yet handed to the thread.
    Queued(Available),
    Downloading(String),
    Ready(Staged),
    Failed {
        version: String,
        error: String,
    },
}

/// What the services and the frame share.
#[derive(Default)]
pub(super) struct Shared {
    /// The release the last check offered.
    pub(super) offered: Option<Available>,
    pub(super) phase: Phase,
    /// A generation runs or waits, so restarting now would lose it; set each frame.
    pub(super) busy: bool,
    /// The swap is done: close the window and start this.
    pub(super) restart: Option<(String, Relaunch)>,
}

pub(super) struct Installer {
    pub(super) shared: Rc<RefCell<Shared>>,
    asks: SyncSender<Available>,
    done: Receiver<Done>,
    /// What's recorded for `app.update_status`, so it is recorded only when it changes.
    recorded: Option<Install>,
    /// Set once the window is closing for the restart.
    pub(super) relaunch: Option<Relaunch>,
}

/// The two service handlers [`Installer::spawn`] returns.
pub(super) struct Handlers {
    pub(super) install: super::Service,
    pub(super) apply: super::Service,
}

impl Installer {
    /// The install thread over `job`, and the handlers that queue for it. `updates` is the
    /// updates folder.
    pub(super) fn spawn(mut job: Job, updates: PathBuf) -> Result<(Self, Handlers), Error> {
        let (asks, inbox) = sync_channel::<Available>(1);
        let (outbox, done) = sync_channel::<Done>(1);
        std::thread::Builder::new()
            .name("nodecraft-update-install".into())
            .spawn(move || {
                for available in inbox {
                    // A download or unpacking that panics fails this install, not the thread.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&available)))
                        .unwrap_or_else(|_| Err(Error::new(ErrorKind::Internal, "installing the update failed unexpectedly; nothing was installed")));
                    if outbox.send((available.version, result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::new(ErrorKind::Unsupported, format!("can't start the update install thread: {e}")))?;
        let shared = Rc::new(RefCell::new(Shared::default()));
        let queue = shared.clone();
        let install = move |_: &serde_json::Value| {
            let mut shared = queue.borrow_mut();
            let Some(offered) = shared.offered.clone() else {
                return Err(Error::new(ErrorKind::InvalidInput, "no update is offered; check for updates first"));
            };
            match &shared.phase {
                Phase::Queued(_) | Phase::Downloading(_) => {
                    return Err(Error::new(ErrorKind::Busy, "NodeCraft is already downloading the update; wait for it to finish"));
                }
                Phase::Ready(staged) if staged.version == offered.version => return Ok(json!({"pending": offered.version})),
                Phase::Idle | Phase::Ready(_) | Phase::Failed { .. } => {}
            }
            let version = offered.version.clone();
            shared.phase = Phase::Queued(offered);
            Ok(json!({"pending": version}))
        };
        let swap = shared.clone();
        let apply = move |_: &serde_json::Value| {
            let mut shared = swap.borrow_mut();
            let Phase::Ready(staged) = shared.phase.clone() else {
                return Err(Error::new(ErrorKind::InvalidInput, "no update is ready to install; Install Update first"));
            };
            if shared.restart.is_some() {
                return Ok(json!({"restarting": staged.version}));
            }
            if shared.busy {
                return Err(Error::new(ErrorKind::Busy, "a generation is running; restart to update when it finishes"));
            }
            match install::apply(&staged, &updates) {
                Ok(relaunch) => {
                    install::write_last(&updates, &install::LastUpdate { from: VERSION.into(), to: staged.version.clone(), error: None });
                    shared.restart = Some((staged.version.clone(), relaunch));
                    Ok(json!({"restarting": staged.version}))
                }
                Err(error) => {
                    shared.phase = Phase::Failed { version: staged.version, error: error.message.clone() };
                    Err(error)
                }
            }
        };
        let installer = Self { shared, asks, done, recorded: None, relaunch: None };
        Ok((installer, Handlers { install: Box::new(install), apply: Box::new(apply) }))
    }

    /// Whether the thread has work, so the frames keep coming.
    pub(super) fn is_waiting(&self) -> bool {
        matches!(self.shared.borrow().phase, Phase::Queued(_) | Phase::Downloading(_))
    }

    /// Hands a queued install to the thread, takes a finished one, and records where it stands.
    /// Returns true when the window should close for the restart.
    pub(super) fn tick(&mut self, session: &mut Session, offered: Option<&Available>, busy: bool) -> bool {
        {
            let mut shared = self.shared.borrow_mut();
            shared.offered = offered.cloned();
            shared.busy = busy;
            if let Phase::Queued(available) = shared.phase.clone() {
                shared.phase = match self.asks.try_send(available.clone()) {
                    Ok(()) => Phase::Downloading(available.version),
                    Err(_) => Phase::Failed { version: available.version, error: "the update install thread stopped".into() },
                };
            }
            if let Phase::Downloading(version) = shared.phase.clone() {
                match self.done.try_recv() {
                    Ok((done, result)) if done == version => {
                        shared.phase = match result {
                            Ok(staged) => Phase::Ready(staged),
                            Err(error) => Phase::Failed { version, error: error.message },
                        };
                    }
                    Ok(_) | Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        shared.phase = Phase::Failed { version, error: "the update install thread stopped".into() };
                    }
                }
            }
        }
        let install = self.status();
        if install != self.recorded {
            match &install {
                Some(Install::Ready { version }) => log::info!("update {version} is staged"),
                Some(Install::Failed { version, error }) => log::info!("update {version} failed: {error}"),
                Some(Install::Downloading { .. }) | None => {}
            }
            update_install::record(session, install.clone());
            self.recorded = install;
        }
        // A generation started after the swap (the busy check can be a frame old) keeps the window
        // open until it finishes; the swapped-out copy keeps running meanwhile.
        let restart = {
            let mut shared = self.shared.borrow_mut();
            if shared.busy { None } else { shared.restart.take() }
        };
        match restart {
            Some((_, relaunch)) => {
                self.relaunch = Some(relaunch);
                true
            }
            None => false,
        }
    }

    /// What `app.update_status` reports as `install`.
    pub(super) fn status(&self) -> Option<Install> {
        match &self.shared.borrow().phase {
            Phase::Idle => None,
            Phase::Queued(available) => Some(Install::Downloading { version: available.version.clone() }),
            Phase::Downloading(version) => Some(Install::Downloading { version: version.clone() }),
            Phase::Ready(staged) => Some(Install::Ready { version: staged.version.clone() }),
            Phase::Failed { version, error } => Some(Install::Failed { version: version.clone(), error: error.clone() }),
        }
    }

    /// From `on_exit`: starts the new copy, which waits for this process to end.
    pub(super) fn relaunch(&self, project: Option<&Path>) {
        if let Some(relaunch) = &self.relaunch
            && let Err(error) = install::relaunch(relaunch, project, install::started_with_control())
        {
            log::error!("{}", error.message);
        }
    }
}
