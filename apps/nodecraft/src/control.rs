//! `nodecraft --control`: the app answering the control channel (`automation::control`).
//!
//! [`Controlled`] wraps the egui app. Each frame it answers waiting requests against the
//! session the window shows, so changes appear live. Screenshots are taken through egui and
//! answered when the image arrives, a frame later.

use std::path::PathBuf;
use std::sync::Arc;

use nodecraft_automation::control::{self, Call, Listener, Request, Token, TokenFile};
use nodecraft_ui_egui::NodeCraftApp;

/// Requests answered per frame, so a busy client can't stall drawing.
const PER_FRAME: usize = 16;
/// Screenshots waiting for their image at once.
const MAX_PENDING_SHOTS: usize = 4;
/// A screenshot whose image hasn't arrived by then (the window isn't drawing) fails.
const SHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Marks the screenshots this module asks for, so others aren't taken as replies.
struct ControlShot;

pub struct Controlled {
    pub app: NodeCraftApp,
    listener: Listener,
    shots: Vec<Call>,
    /// When the pending screenshot was requested.
    shot_since: Option<std::time::Instant>,
    token_file: Option<(PathBuf, TokenFile)>,
}

impl Controlled {
    /// Starts listening and writes the token file into `dir` (`control.json`).
    /// On failure the app comes back with the error, so it can still open without the channel.
    pub fn start(app: NodeCraftApp, ctx: &egui::Context, dir: Option<PathBuf>) -> Result<Self, (Box<NodeCraftApp>, nodecraft_types::Error)> {
        match Self::listen(ctx, dir) {
            Ok((listener, token_file)) => Ok(Self { app, listener, shots: Vec::new(), shot_since: None, token_file }),
            Err(e) => Err((Box::new(app), e)),
        }
    }

    fn listen(ctx: &egui::Context, dir: Option<PathBuf>) -> Result<(Listener, Option<(PathBuf, TokenFile)>), nodecraft_types::Error> {
        let token = Token::generate()?;
        let waker = ctx.clone();
        let listener = control::listen(token.clone(), Arc::new(move || waker.request_repaint()))?;
        let file = TokenFile { port: listener.addr.port(), token: token.to_hex(), pid: std::process::id() };
        let token_file = match dir {
            Some(dir) => {
                let path = control::private_dir(&dir)?.join("control.json");
                file.write(&path)?;
                log::info!("control channel on {}; token in {}", listener.addr, path.display());
                Some((path, file))
            }
            None => {
                // No settings folder (in-memory settings): print where to connect instead. A
                // closed stdout is an error, not a panic.
                use std::io::Write;
                let text = serde_json::to_string(&file).map_err(|e| nodecraft_types::Error::new(nodecraft_types::ErrorKind::Internal, e.to_string()))?;
                writeln!(std::io::stdout(), "{text}")
                    .and_then(|()| std::io::stdout().flush())
                    .map_err(|e| nodecraft_types::Error::new(nodecraft_types::ErrorKind::Unsupported, format!("can't print the control token: {e}")))?;
                None
            }
        };
        Ok((listener, token_file))
    }

    /// Answers waiting requests and finished screenshots.
    fn pump(&mut self, ctx: &egui::Context) {
        let ours = |data: &egui::UserData| data.data.as_ref().is_some_and(|d| d.downcast_ref::<ControlShot>().is_some());
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, user_data, .. } if ours(user_data) => Some(Arc::clone(image)),
                _ => None,
            })
        });
        if let Some(image) = image {
            let pending: Vec<Call> = std::mem::take(&mut self.shots).into_iter().filter(|c| !c.expired()).collect();
            self.shot_since = None;
            if !pending.is_empty() {
                // Encoding a large window takes a while: not on the UI thread.
                let encoded = std::thread::Builder::new().name("nodecraft-control-shot".into()).spawn(move || {
                    let [w, h] = image.size;
                    let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
                    let answer = match (u32::try_from(w), u32::try_from(h)) {
                        (Ok(w), Ok(h)) => control::screenshot_answer(w, h, &rgba),
                        _ => Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Unsupported, "the window is too large to capture")),
                    };
                    for call in pending {
                        call.reply(answer.clone());
                    }
                });
                if let Err(e) = encoded {
                    log::warn!("screenshot not encoded: {e}");
                }
            }
        }
        // The window isn't drawing (minimised, hidden): fail the waiting screenshots.
        if self.shot_since.is_some_and(|since| since.elapsed() > SHOT_TIMEOUT) {
            for call in std::mem::take(&mut self.shots) {
                call.reply(Err(nodecraft_types::Error::new(
                    nodecraft_types::ErrorKind::Timeout,
                    "the window didn't draw a frame to capture; is it minimised?",
                )));
            }
            self.shot_since = None;
        }
        let calls: Vec<Call> = self.listener.calls.try_iter().take(PER_FRAME).collect();
        let more = calls.len() == PER_FRAME;
        for call in calls {
            // The client already gave up on it: running it now could apply a command twice.
            if call.expired() {
                continue;
            }
            match answer(&mut self.app, ctx, &call.request) {
                Some(result) => call.reply(result),
                None if call.request == Request::Screenshot && self.shots.len() < MAX_PENDING_SHOTS => {
                    if self.shots.is_empty() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(ControlShot)));
                        self.shot_since = Some(std::time::Instant::now());
                    }
                    self.shots.push(call);
                }
                None => call.reply(Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Busy, "screenshots are already being taken; try again"))),
            }
        }
        if more {
            ctx.request_repaint();
        } else if !self.shots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

impl eframe::App for Controlled {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.pump(ctx);
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }

    fn on_exit(&mut self) {
        self.app.on_exit();
        if let Some((path, file)) = &self.token_file {
            file.remove_if_ours(path);
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}

// Keep the production UI-thread dispatch independently testable without a native window.
fn answer(app: &mut NodeCraftApp, ctx: &egui::Context, request: &Request) -> Option<Result<serde_json::Value, nodecraft_types::Error>> {
    control::answer_with(&mut LiveCommands { app, ctx }, request)
}
struct LiveCommands<'a> {
    app: &'a mut NodeCraftApp,
    ctx: &'a egui::Context,
}
impl LiveCommands<'_> {
    fn navigation_idle(&self) -> Result<(), nodecraft_types::Error> {
        // The internal minimap bridge permits its own pan intents during a drag. External
        // requests must not use that exemption to steal the pointer's active gesture.
        if self.app.navigation.pointer_owned || self.app.navigation.drag.is_some() {
            return Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Conflict, "a canvas gesture owns navigation"));
        }
        Ok(())
    }
}
impl nodecraft_automation::commands::CommandHost for LiveCommands<'_> {
    fn session(&self) -> &nodecraft_engine::Session {
        &self.app.session
    }
    fn enabled(&mut self, id: &str, params: &serde_json::Value) -> Result<(), nodecraft_types::Error> {
        if NodeCraftApp::is_navigation_command(id) {
            self.navigation_idle()?;
            self.app.navigation_enabled(self.ctx, id, params)
        } else if NodeCraftApp::is_search_command(id) {
            self.navigation_idle()?;
            self.app.search_enabled(self.ctx, id, params)
        } else {
            self.app.session.enabled(id, params)
        }
    }
    fn dispatch(&mut self, id: &str, params: &serde_json::Value) -> Result<serde_json::Value, nodecraft_types::Error> {
        if NodeCraftApp::is_navigation_command(id) {
            self.navigation_idle()?;
            self.app.dispatch_navigation(self.ctx, id, params)
        } else if NodeCraftApp::is_search_command(id) {
            self.navigation_idle()?;
            self.app.dispatch_search(self.ctx, id, params)
        } else {
            self.app.session.dispatch(id, params)
        }
    }
}

#[cfg(test)]
mod tests;
