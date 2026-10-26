//! Narrow authenticated channel to focus the desktop owning the recovery journal (#968).
//! It deliberately accepts only file.focus_owner; it grants no document or filesystem access.
use nodecraft_automation::control::{self, Client, Listener, Request, Token, TokenFile};
use nodecraft_engine::storage_window_cmds::SERVICE;
use nodecraft_types::{Error, ErrorKind};
use nodecraft_ui_egui::NodeCraftApp;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

pub struct StorageWindow {
    session: Arc<()>,
    owner: Option<(Listener, PathBuf, TokenFile)>,
    outcomes: mpsc::Receiver<Result<(), Error>>,
    busy: Arc<AtomicBool>,
}
impl StorageWindow {
    pub fn start(app: &mut NodeCraftApp, ctx: &egui::Context, recovery: &Path) -> Result<Option<Self>, Error> {
        let owner = app.session.file_status().available;
        if !owner && !app.storage_owner_blocked() {
            return Ok(None);
        }
        let folder = recovery.parent().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "recovery folder is missing"))?;
        let dir = control::private_dir(folder)?;
        let mut name = recovery.file_name().ok_or_else(|| Error::new(ErrorKind::InvalidInput, "recovery filename is missing"))?.to_os_string();
        name.push(".window.json");
        let path = dir.join(name);
        let (outcomes, receiver) = mpsc::sync_channel(1);
        let busy = Arc::new(AtomicBool::new(false));
        let listener = if owner {
            let token = Token::generate()?;
            let wake = ctx.clone();
            let listener = control::listen(token.clone(), Arc::new(move || wake.request_repaint()))?;
            let file = TokenFile { port: listener.addr.port(), token: token.to_hex(), pid: std::process::id() };
            file.write(&path)?;
            let focus = ctx.clone();
            app.session.register_host_service(SERVICE, move |_| {
                focus.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                focus.send_viewport_cmd(egui::ViewportCommand::Focus);
                Ok(json!({"requested":true}))
            })?;
            Some((listener, path, file))
        } else {
            let running = busy.clone();
            let wake = ctx.clone();
            app.session.register_host_service(SERVICE, move |_| {
                if running.swap(true, Ordering::SeqCst) {
                    return Err(Error::new(ErrorKind::Busy, "A request to focus the storage window is already pending."));
                }
                let target = path.clone();
                let outcomes = outcomes.clone();
                let wake = wake.clone();
                if std::thread::Builder::new()
                    .name("nodecraft-focus-owner".into())
                    .spawn(move || {
                        let result = focus_remote(&target);
                        let _ = outcomes.send(result);
                        wake.request_repaint();
                    })
                    .is_err()
                {
                    running.store(false, Ordering::SeqCst);
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "The storage window request could not start. Switch to the other NodeCraft window manually.",
                    ));
                }
                Ok(json!({"requested":true}))
            })?;
            None
        };
        Ok(Some(Self { session: app.session.view_identity(), owner: listener, outcomes: receiver, busy }))
    }
    pub fn pump(&mut self, app: &mut NodeCraftApp, ctx: &egui::Context) {
        if !Arc::ptr_eq(&self.session, &app.session.view_identity()) {
            self.retire();
            let _ = self.outcomes.try_recv();
            self.busy.store(false, Ordering::SeqCst);
            return;
        }
        if let Some((listener, _, _)) = &self.owner {
            // Bound work per frame even when an authenticated local caller is noisy.
            for call in listener.calls.try_iter().take(8) {
                if !call.expired() {
                    let result = answer(app, &call.request);
                    call.reply(result);
                }
            }
        }
        if let Ok(result) = self.outcomes.try_recv() {
            self.busy.store(false, Ordering::SeqCst);
            app.notice = Some(match result {
                Ok(()) => "The storage window was asked to come forward. Use its File menu to open or save a project; this window's edits stay here.".into(),
                Err(_) => "The other NodeCraft window could not be reached. Switch to it manually and use its File menu. If it has closed, close this window and reopen NodeCraft; this window's edits cannot be saved.".into(),
            });
        }
        if self.busy.load(Ordering::SeqCst) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    fn retire(&mut self) {
        if let Some((_, path, token)) = self.owner.take() {
            token.remove_if_ours(&path);
        }
    }
}
impl Drop for StorageWindow {
    fn drop(&mut self) {
        self.retire();
    }
}
fn focus_remote(path: &Path) -> Result<(), Error> {
    let mut client = Client::from_token_file(path)?;
    let args = json!({"id":"file.focus_owner", "params":{}})
        .as_object()
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::Internal, "focus request could not be built"))?;
    client.call("command.run", args)?;
    Ok(())
}
fn answer(app: &mut NodeCraftApp, request: &Request) -> Result<Value, Error> {
    if let Request::RunCommand(args) = request
        && args.len() == 2
        && args.get("id").and_then(Value::as_str) == Some("file.focus_owner")
        && args.get("params") == Some(&json!({}))
    {
        return app.session.dispatch("file.focus_owner", &json!({}));
    }
    Err(Error::new(ErrorKind::Forbidden, "This channel only focuses the project storage window."))
}

#[cfg(test)]
mod tests;
