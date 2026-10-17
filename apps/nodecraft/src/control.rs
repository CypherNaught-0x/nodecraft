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

pub struct Controlled {
    pub app: NodeCraftApp,
    listener: Listener,
    shots: Vec<Call>,
    token_file: Option<PathBuf>,
}

impl Controlled {
    /// Starts listening and writes the token file into `dir` (`control.json`).
    pub fn start(app: NodeCraftApp, ctx: &egui::Context, dir: Option<PathBuf>) -> Result<Self, nodecraft_types::Error> {
        let token = Token::generate()?;
        let waker = ctx.clone();
        let listener = control::listen(token.clone(), Arc::new(move || waker.request_repaint()))?;
        let token_file = match dir {
            Some(dir) => {
                let path = dir.join("control.json");
                TokenFile { port: listener.addr.port(), token: token.to_hex(), pid: std::process::id() }.write(&path)?;
                log::info!("control channel on {}; token in {}", listener.addr, path.display());
                Some(path)
            }
            None => {
                // No settings folder (in-memory settings): print where to connect instead.
                println!("{}", serde_json::json!({"port": listener.addr.port(), "token": token.to_hex(), "pid": std::process::id()}));
                None
            }
        };
        Ok(Self { app, listener, shots: Vec::new(), token_file })
    }

    /// Answers waiting requests and finished screenshots.
    fn pump(&mut self, ctx: &egui::Context) {
        let screenshots: Vec<Arc<egui::ColorImage>> = ctx.input(|i| {
            i.raw.events.iter().filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(Arc::clone(image)) } else { None }).collect()
        });
        for image in screenshots {
            let pending = std::mem::take(&mut self.shots);
            if pending.is_empty() {
                continue;
            }
            let [w, h] = image.size;
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_srgba_unmultiplied()).collect();
            let answer = match (u32::try_from(w), u32::try_from(h)) {
                (Ok(w), Ok(h)) => control::screenshot_answer(w, h, &rgba),
                _ => Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Unsupported, "the window is too large to capture")),
            };
            for call in pending {
                call.reply(answer.clone());
            }
        }
        for call in self.listener.calls.try_iter().take(PER_FRAME).collect::<Vec<_>>() {
            match control::answer(&mut self.app.session, &call.request) {
                Some(result) => call.reply(result),
                None if call.request == Request::Screenshot && self.shots.len() < MAX_PENDING_SHOTS => {
                    if self.shots.is_empty() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                    }
                    self.shots.push(call);
                    ctx.request_repaint();
                }
                None => call.reply(Err(nodecraft_types::Error::new(nodecraft_types::ErrorKind::Busy, "screenshots are already being taken; try again"))),
            }
        }
        // More waiting: come back next frame.
        if !self.shots.is_empty() {
            ctx.request_repaint();
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
        if let Some(path) = &self.token_file {
            let _ = std::fs::remove_file(path);
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}
