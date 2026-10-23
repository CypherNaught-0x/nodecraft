//! Trusted frontend dispatch after shared automation classification. This is not an admission
//! bypass: implementations must use Session dispatch or its supported scoped live-view bridge.
use nodecraft_engine::{Session, impact::Impact};
use nodecraft_types::Error;
use serde_json::Value;

/// A session with optional live frontend capabilities. The protocol owns command lookup and
/// classification; the host supplies fresh availability and the same engine mutation path.
/// Dispatch must retain engine parameter validation, panic guards and enabled checks.
pub trait CommandHost {
    fn session(&self) -> &Session;
    fn enabled(&mut self, id: &str, params: &Value) -> Result<(), Error> {
        self.session().enabled(id, params)
    }
    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error>;
    /// What dispatching `id` would do, without doing it; refuses what dispatch would refuse.
    fn impact(&mut self, id: &str, params: &Value) -> Result<Impact, Error> {
        self.session().impact(id, params)
    }
    /// Whether this host routes the live view commands ([`crate::mcp::LIVE_ONLY`]) to a window.
    fn live(&self) -> bool {
        false
    }
    /// The folders an agent may open projects from and save them to (#423); none refuses the
    /// file commands.
    fn file_roots(&self) -> Option<&crate::roots::FileRoots> {
        None
    }
}

impl CommandHost for Session {
    fn session(&self) -> &Session {
        self
    }
    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        Session::dispatch(self, id, params)
    }
}
