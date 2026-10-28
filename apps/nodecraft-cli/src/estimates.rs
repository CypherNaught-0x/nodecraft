//! Headless provider estimates: dispatch the shared command, then wait only for its notice.
use nodecraft_automation::commands::CommandHost;
use nodecraft_engine::Session;
use nodecraft_types::{CanvasId, Error, ErrorKind, FieldId, NodeId};
use serde_json::{Value, json};

use crate::quote::Maker;
use crate::run::Runner;

/// Standalone MCP owns an estimates-only host. Paid execution remains unavailable here.
pub struct Host {
    pub session: Session,
    pub runner: Runner,
    pub maker: Box<Maker<'static>>,
    routing: crate::quote::Routing,
}

/// Optional price support must not prevent ordinary standalone MCP commands from starting.
pub fn optional_setup(
    services: &mut nodecraft_engine::commands::Services,
    maker: impl FnOnce() -> Result<Box<Maker<'static>>, Error>,
) -> Option<(Runner, Box<Maker<'static>>)> {
    match guarded(|| {
        // Build HTTP before registering any estimate service: a TLS failure leaves none behind.
        let maker = maker()?;
        let runner = Runner::estimates_only(services, "mcp-estimates".into())?;
        Ok((runner, maker))
    }) {
        Ok(ready) => Some(ready),
        Err(error) => {
            eprintln!("warning: provider estimates are unavailable: {}", error.message);
            None
        }
    }
}

pub(super) fn guarded<T>(work: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .map_err(|_| Error::new(ErrorKind::Internal, "provider price preparation failed unexpectedly; no generation was submitted"))?
}

impl Host {
    pub fn new(session: Session, runner: Runner, maker: Box<Maker<'static>>) -> Self {
        Self { session, runner, maker, routing: crate::quote::Routing::new(nodecraft_engine::settings::peek().providers) }
    }

    fn dispatch_inner(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        if id != "run.estimate" {
            // Idle MCP has no frame pump: expire ten-minute quotes before later commands.
            self.runner.refresh_estimates(&mut self.session);
            let result = self.session.dispatch(id, params)?;
            if matches!(id, "keys.set" | "keys.remove") {
                self.routing = crate::quote::Routing::new(nodecraft_engine::settings::peek().providers);
            }
            return Ok(result);
        }
        // Keep engine schema, permissions and enabled checks ahead of keychain/network work.
        self.session.enabled(id, params)?;
        prepare(&mut self.session, params, &mut *self.maker, &mut self.routing)?;
        self.runner.install(&mut self.session);
        self.runner.estimate(&mut self.session, params)
    }
}

impl CommandHost for Host {
    fn session(&self) -> &Session {
        &self.session
    }

    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        guarded(|| self.dispatch_inner(id, params))
    }
}

/// Route exactly the selected/field cards, or the upstream closure requested by the command.
/// Parameters have already passed the engine's enabled check; no second command parser exists.
fn prepare(session: &mut Session, params: &Value, maker: &mut Maker<'_>, routing: &mut crate::quote::Routing) -> Result<(), Error> {
    let invalid = || Error::new(ErrorKind::InvalidInput, "the estimate's selection is invalid");
    let canvas: CanvasId = serde_json::from_value(params.get("canvas").cloned().ok_or_else(invalid)?).map_err(|_| invalid())?;
    let mode = params.get("mode").and_then(Value::as_str).ok_or_else(invalid)?;
    let nodes: Vec<NodeId> = match mode {
        "selected" | "with_upstream" => {
            let nodes: Vec<NodeId> = match params.get("nodes") {
                Some(nodes) => serde_json::from_value(nodes.clone()).map_err(|_| invalid())?,
                None => Vec::new(),
            };
            // Unlike the CLI's empty target default, an empty selected run stays empty.
            let graph = session.document().canvases().get(&canvas).ok_or_else(invalid)?;
            if nodes.iter().any(|id| !graph.nodes().contains_key(id)) {
                return Err(Error::new(ErrorKind::NotFound, "the estimate names a node outside its canvas"));
            }
            nodes
        }
        "field" => {
            let field: FieldId = serde_json::from_value(params.get("field").cloned().ok_or_else(invalid)?).map_err(|_| invalid())?;
            let graph = session.document().canvases().get(&canvas).ok_or_else(invalid)?;
            if !graph.fields().contains_key(&field) {
                return Err(Error::new(ErrorKind::NotFound, "the estimate names a field outside its canvas"));
            }
            graph.nodes().values().filter(|node| node.field == Some(field)).map(|node| node.id).collect()
        }
        "all_stale" => crate::quote::targets(session, canvas, &[])?,
        _ => return Err(invalid()),
    };
    routing.prepare(session, canvas, &nodes, maker, matches!(mode, "with_upstream" | "all_stale"))
}

/// The CLI's same targets/upstream request, submitted before it reads the final run quote.
pub fn for_targets(runner: &mut Runner, session: &mut Session, canvas: CanvasId, targets: &[NodeId]) -> Result<Value, Error> {
    let params =
        if targets.is_empty() { json!({"canvas": canvas, "mode": "all_stale"}) } else { json!({"canvas": canvas, "mode": "with_upstream", "nodes": targets}) };
    runner.estimate(session, &params)
}

/// An explicitly requested provider estimate for `quote`; assets stay in memory.
pub fn quote(path: &std::path::Path, options: &crate::Options) -> std::process::ExitCode {
    let result = guarded(|| {
        let mut services = nodecraft_engine::commands::Services::default();
        let mut runner = Runner::estimates_only(&mut services, "cli-estimate".into())?;
        let (mut session, canvas, wanted) = crate::inputs::open_session(path, options, services)?;
        let targets = crate::quote::targets(&session, canvas, &wanted)?;
        let mut maker = crate::providers::keychain_maker(nodecraft_engine::settings::peek().providers)?;
        crate::quote::prepare(&mut session, canvas, &targets, &mut maker)?;
        runner.install(&mut session);
        let estimate = for_targets(&mut runner, &mut session, canvas, &targets)?;
        let mut quoted = crate::quote::quote(&mut session, canvas, &targets)?;
        quoted.as_object_mut().ok_or_else(|| Error::new(ErrorKind::Internal, "run.quote returned an invalid response"))?.insert("estimate".into(), estimate);
        Ok::<_, Error>(crate::print(options.json, &quoted, |value| {
            format!("{}{}", summary(&value["estimate"]), crate::quote::quote_text(&session, canvas, value))
        }))
    });
    match result {
        Ok(code) => code,
        Err(error) => crate::fail(&error.message),
    }
}

/// Terminal estimate status, also shown before a CLI run's step report.
pub fn summary(estimate: &Value) -> String {
    match estimate.get("status").and_then(Value::as_str) {
        Some("estimated") => format!("Provider prices: {} quoted, {} unquoted.\n", estimate["quoted"], estimate["unquoted"]),
        Some("not_estimated") => format!("Provider prices unavailable: {}\n", estimate.get("reason").and_then(Value::as_str).unwrap_or("no answer")),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests;
