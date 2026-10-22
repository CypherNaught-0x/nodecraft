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
}

impl CommandHost for Host {
    fn session(&self) -> &Session {
        &self.session
    }

    fn dispatch(&mut self, id: &str, params: &Value) -> Result<Value, Error> {
        if id != "run.estimate" {
            // Idle MCP has no frame pump: expire ten-minute quotes before later commands.
            self.runner.refresh_estimates(&mut self.session);
            return self.session.dispatch(id, params);
        }
        // Keep engine schema, permissions and enabled checks ahead of keychain/network work.
        self.session.enabled(id, params)?;
        prepare(&mut self.session, params, &mut *self.maker)?;
        self.runner.install(&mut self.session);
        self.runner.estimate(&mut self.session, params)
    }
}

/// Route exactly the selected/field cards, or the upstream closure requested by the command.
/// Parameters have already passed the engine's enabled check; no second command parser exists.
fn prepare(session: &mut Session, params: &Value, maker: &mut Maker<'_>) -> Result<(), Error> {
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
    crate::quote::prepare_selection(session, canvas, &nodes, maker, matches!(mode, "with_upstream" | "all_stale"))
}

/// The CLI's same targets/upstream request, submitted before it reads the final run quote.
pub fn for_targets(runner: &mut Runner, session: &mut Session, canvas: CanvasId, targets: &[NodeId]) -> Result<Value, Error> {
    let params =
        if targets.is_empty() { json!({"canvas": canvas, "mode": "all_stale"}) } else { json!({"canvas": canvas, "mode": "with_upstream", "nodes": targets}) };
    runner.estimate(session, &params)
}

/// An explicitly requested provider estimate for `quote`; assets stay in memory.
pub fn quote(path: &std::path::Path, options: &crate::Options) -> std::process::ExitCode {
    let result = (|| {
        let mut services = nodecraft_engine::commands::Services::default();
        let mut runner = Runner::estimates_only(&mut services, "cli-estimate".into())?;
        let mut session = crate::project::open_with_assets(path)?.with_services(services);
        let canvas = crate::quote::canvas_of(&session, options.canvas.as_deref())?;
        let targets = crate::quote::targets(&session, canvas, &options.nodes)?;
        let mut maker = crate::providers::keychain_maker(nodecraft_engine::settings::peek().providers)?;
        crate::quote::prepare(&mut session, canvas, &targets, &mut maker)?;
        runner.install(&mut session);
        let estimate = for_targets(&mut runner, &mut session, canvas, &targets)?;
        let mut quoted = crate::quote::quote(&mut session, canvas, &targets)?;
        quoted["estimate"] = estimate;
        Ok::<_, Error>(crate::print(options.json, &quoted, |value| {
            format!("{}{}", summary(&value["estimate"]), crate::quote::quote_text(&session, canvas, value))
        }))
    })();
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
