//! Several ComfyUI servers (#864). A card whose model runs on ComfyUI follows the default server
//! unless `comfy.server.use` names another; the name is kept in the card's config
//! ([`COMFY_SERVER_CONFIG`]) and the host routes the card to that server's adapter
//! (`provider_setup::slot_for`). Servers themselves live in the settings, which the engine doesn't
//! hold: a card naming a server that isn't there follows the default.
//!
//! Each server's last check (`comfy.nodes.refresh {server}`) is kept here for the server panel;
//! only the default server's check installs the node list workflows import against. Every
//! successful check also retains bounded partner-class evidence for that server's run quotes.
use crate::Session;
use crate::comfy_workflow_cmds::{self, Fetched, Report};
use crate::commands::{CommandSpec, Context, Effect, invalid, params};
use crate::provider_setup::COMFY_SERVER_CONFIG;
use crate::settings::{MAX_COMFY_SERVERS, check_server_name};
use nodecraft_graph::{Change, Changeset};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

/// Each server's last check, by name.
#[derive(Default)]
struct ServerReports(BTreeMap<String, Report>);

/// Records what fetch `id` of `server` brought back. The default server's fetch installs its node
/// list too (as `comfy_workflow_cmds::finish`); every server's partner evidence prices its runs.
pub fn finish(session: &mut Session, id: u64, server: &str, is_default: bool, fetched: Result<Fetched, Error>) -> Report {
    if let Ok(fetched) = &fetched
        && let Some(instance) = crate::provider_setup::comfy_instance(server)
    {
        crate::comfy_partner_quotes::keep(session, Some(instance), &fetched.nodes);
    }
    let report = if is_default {
        comfy_workflow_cmds::finish(session, id, fetched)
    } else {
        match fetched {
            Ok(Fetched { nodes, health }) => Report { id, nodes: Some(nodes.len()), error: None, server: health },
            Err(error) => Report { id, nodes: None, error: Some(error.message), server: None },
        }
    };
    let mut reports = session.swap_resource::<ServerReports>(None).unwrap_or_default();
    if reports.0.contains_key(server) || reports.0.len() < MAX_COMFY_SERVERS {
        reports.0.insert(server.to_owned(), report.clone());
    }
    session.swap_resource(Some(reports));
    report
}

/// The last check of `server`.
pub fn report<'a>(session: &'a Session, server: &str) -> Option<&'a Report> {
    session.resource::<ServerReports>().and_then(|reports| reports.0.get(server))
}

/// Forgets what was read from `server` after it changed or was removed; the node list too when it
/// was the default's.
pub fn forget(session: &mut Session, server: &str, was_default: bool) {
    if let Some(instance) = crate::provider_setup::comfy_instance(server) {
        crate::comfy_partner_quotes::forget(session, Some(instance));
    }
    if was_default {
        comfy_workflow_cmds::forget(session);
    }
    if let Some(mut reports) = session.swap_resource::<ServerReports>(None) {
        reports.0.remove(server);
        session.swap_resource(Some(reports));
    }
}

/// Every server's last check, for `comfy.nodes.status`.
pub(crate) fn reports(c: &Context<'_>) -> Json {
    c.resources.get::<ServerReports>().map_or_else(|| json!({}), |reports| json!(reports.0))
}

/// Whether `node`'s model runs on ComfyUI, so it can pick a server.
pub fn runs_on_comfy(catalog: &crate::generation_catalog::GenerationCatalog, node: &nodecraft_graph::Node) -> bool {
    match node.params.get(nodecraft_nodes::generation::MODEL) {
        Some(Value::Text(model)) => catalog.run_catalog().catalog().get(model).is_some_and(|descriptor| descriptor.provider == "comfyui"),
        _ => false,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UseParams {
    canvas: CanvasId,
    id: NodeId,
    /// The server to run on; `null` follows the default.
    server: Option<String>,
}

fn changed(c: &Context<'_>, value: &Json) -> Result<Option<(CanvasId, nodecraft_graph::Node)>, Error> {
    let p: UseParams = params(value)?;
    // Serde reads a missing `server` as null; the schema requires saying which.
    if value.get("server").is_none() {
        return Err(invalid("server is required: a server's name, or null to follow the default"));
    }
    if let Some(name) = &p.server {
        check_server_name(name).map_err(invalid)?;
    }
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let node = canvas.nodes().get(&p.id).ok_or_else(|| Error::new(ErrorKind::NotFound, "document entity does not exist"))?;
    let catalog = crate::generation_catalog::GenerationCatalog::current(c.resources).ok();
    if !catalog.is_some_and(|catalog| runs_on_comfy(catalog, node)) {
        return Err(invalid("only a card whose model runs on ComfyUI picks a ComfyUI server"));
    }
    let mut card = node.clone();
    match p.server {
        Some(name) => card.config.insert(COMFY_SERVER_CONFIG.into(), Value::Text(name)),
        None => card.config.remove(COMFY_SERVER_CONFIG),
    };
    Ok((card != *node).then_some((p.canvas, card)))
}

fn enabled(c: &Context<'_>, value: &Json) -> Result<(), Error> {
    changed(c, value)?;
    c.editable()
}

fn run(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    enabled(c, value)?;
    Ok(match changed(c, value)? {
        Some((canvas, node)) => {
            Effect::Edit { changes: Changeset::new(vec![Change::SetNode { canvas, node }]), result: json!({"changed": true}), gesture: None }
        }
        None => Effect::None { result: json!({"changed": false}) },
    })
}

fn schema() -> Json {
    let uuid = json!({"type": "string", "format": "uuid"});
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "id", "server"],
        "properties": {"canvas": uuid, "id": uuid,
            "server": {"type": ["string", "null"], "minLength": 1, "maxLength": crate::settings::MAX_SERVER_NAME}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "comfy.server.use",
        label: "Use ComfyUI server",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, id, server: string | null} -> {changed}; picks the ComfyUI server (by its name in settings) a card whose model runs on ComfyUI is sent to, or with null has it follow the default server. A card naming a server that isn't in settings follows the default. Refused for any other card. One undo step.",
        enabled,
        run,
    }]
}

#[cfg(test)]
mod tests;
