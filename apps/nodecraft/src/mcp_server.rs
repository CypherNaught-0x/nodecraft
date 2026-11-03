//! Settings › Automation › Enable MCP server (#116): the app's own MCP server.
//!
//! [`Server`] is the shell's [`McpServerHost`]. Turning it on starts a thread that reads (or
//! makes) the bearer token in the keychain, binds `127.0.0.1:<port>` and then serves MCP
//! Streamable HTTP (`automation::mcp::http`) with the same tools as `nodecraft-cli mcp --bridge`.
//! Each request becomes a control [`Call`] that the window answers on its UI thread through
//! [`crate::control::CallPump`], so agents edit the canvas the person sees, under the same
//! command classification, quotes and budgets as the control channel.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

use nodecraft_automation::control::{self, Call};
use nodecraft_automation::mcp::McpServer;
use nodecraft_automation::mcp::http::{self, BearerToken, Config, Stopper};
use nodecraft_engine::secrets::{Account, Secret, SecretStore};
use nodecraft_engine::settings::MCP_TOKEN_ACCOUNT;
use nodecraft_ui_egui::app::automation::{McpServerHost, McpState};

/// A server that finished starting.
struct Started {
    url: String,
    token: BearerToken,
    calls: Receiver<Call>,
    stopper: Stopper,
    note: Option<String>,
}

/// The built-in server and what it is waiting on.
pub struct Server {
    enabled: bool,
    state: McpState,
    port: u16,
    token: Option<BearerToken>,
    note: Option<String>,
    calls: Option<Receiver<Call>>,
    stopper: Option<Stopper>,
    starting: Option<Receiver<Result<Started, String>>>,
    secrets: Arc<dyn SecretStore>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Server {
    /// A server for `127.0.0.1:port`, off until turned on. `wake` repaints the window when a
    /// request arrives or starting finishes.
    pub fn new(port: u16, secrets: Arc<dyn SecretStore>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self { enabled: false, state: McpState::Off, port, token: None, note: None, calls: None, stopper: None, starting: None, secrets, wake }
    }

    fn start(&mut self, fresh_token: bool) {
        self.stop();
        self.state = McpState::Starting;
        let (done, starting) = sync_channel(1);
        self.starting = Some(starting);
        let (port, secrets, wake) = (self.port, Arc::clone(&self.secrets), Arc::clone(&self.wake));
        let spawned = std::thread::Builder::new().name("nodecraft-mcp-server".into()).spawn(move || {
            let (token, note) = token(secrets.as_ref(), fresh_token);
            let token = match token {
                Ok(token) => token,
                Err(why) => {
                    let _ = done.send(Err(why));
                    wake();
                    return;
                }
            };
            let listening = match http::bind(Config::new(std::net::SocketAddr::from(([127, 0, 0, 1], port)), token.clone())) {
                Ok(listening) => listening,
                Err(e) => {
                    let _ = done.send(Err(e.message));
                    wake();
                    return;
                }
            };
            let (client, calls) = control::local(Arc::clone(&wake));
            let stopper = listening.stopper();
            let started = Started { url: listening.url(), token, calls, stopper: stopper.clone(), note };
            // Turned off (or on again) meanwhile: nobody will answer or stop this one.
            if done.send(Ok(started)).is_err() {
                stopper.stop();
                return;
            }
            wake();
            let mut server = McpServer::in_app(client, nodecraft_automation::mcp::os_ids());
            listening.serve(&mut |message| server.handle_message(message));
        });
        if let Err(e) = spawned {
            self.starting = None;
            self.state = McpState::Failed(format!("can't start the server: {e}"));
        }
    }

    /// Stops the server (or forgets the one starting, which then stops itself).
    pub fn stop(&mut self) {
        if let Some(stopper) = self.stopper.take() {
            stopper.stop();
        }
        self.starting = None;
        self.calls = None;
        self.token = None;
        self.note = None;
        self.state = McpState::Off;
    }

    /// Takes in a finished start and returns the requests waiting for the window (at most a
    /// frame's worth).
    pub fn poll(&mut self) -> Vec<Call> {
        if let Some(starting) = &self.starting {
            match starting.try_recv() {
                Ok(Ok(started)) => {
                    log::info!("MCP server on {}", started.url);
                    self.state = McpState::Running { url: started.url };
                    self.token = Some(started.token);
                    self.note = started.note;
                    self.calls = Some(started.calls);
                    self.stopper = Some(started.stopper);
                    self.starting = None;
                }
                Ok(Err(why)) => {
                    log::warn!("MCP server not started: {why}");
                    self.state = McpState::Failed(why);
                    self.starting = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.state = McpState::Failed("the server stopped while starting".into());
                    self.starting = None;
                }
            }
        }
        match &self.calls {
            Some(calls) => calls.try_iter().take(crate::control::PER_FRAME).collect(),
            None => Vec::new(),
        }
    }
}

/// The token in the keychain, made (or replaced, when `fresh`) if needed. A stored token is
/// replaced only when there is none or it can't be used: if the keychain can't be read right
/// now, a token is made for this session and nothing is written, so a passing failure never
/// overwrites the one clients have. The note says when a token lasts only until the app closes.
fn token(secrets: &dyn SecretStore, fresh: bool) -> (Result<BearerToken, String>, Option<String>) {
    let account = match Account::new(MCP_TOKEN_ACCOUNT) {
        Ok(account) => account,
        Err(e) => return (Err(e.to_string()), None),
    };
    if !fresh {
        match secrets.get(&account) {
            Ok(Some(stored)) => {
                if let Ok(token) = BearerToken::new(stored.expose()) {
                    return (Ok(token), None);
                }
            }
            Ok(None) => {}
            Err(why) => {
                let note = format!("The keychain couldn't be read ({why}); this token lasts until NodeCraft closes, and the stored one is kept.");
                return (BearerToken::generate().map_err(|e| e.message), Some(note));
            }
        }
    }
    let token = match BearerToken::generate() {
        Ok(token) => token,
        Err(e) => return (Err(e.message), None),
    };
    let kept = Secret::new(token.expose()).map_err(|e| e.to_string()).and_then(|secret| secrets.set(&account, &secret).map_err(|e| e.to_string()));
    let note = kept.err().map(|why| format!("The token couldn't be kept in the keychain ({why}); it lasts until NodeCraft closes."));
    (Ok(token), note)
}

impl McpServerHost for Server {
    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.stop();
        } else if !matches!(self.state, McpState::Running { .. } | McpState::Starting) {
            self.start(false);
        }
    }

    fn state(&self) -> McpState {
        self.state.clone()
    }

    fn token(&self) -> Option<String> {
        self.token.as_ref().map(|t| t.expose().to_owned())
    }

    fn new_token(&mut self) {
        if self.enabled {
            self.start(true);
        }
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

#[cfg(test)]
mod tests;
