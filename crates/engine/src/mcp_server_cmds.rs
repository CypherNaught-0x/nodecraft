//! The desktop app's built-in MCP server (#116): Settings › Automation › Enable MCP server.
//!
//! The host runs the server (`automation::mcp::http`, on this computer only, with a bearer
//! token) and registers [`SERVICE`]; these commands only ask it. The token never passes through
//! a command: the person copies it from the Automation panel. Turning the server on or off is a
//! person's decision, so `automation.mcp_server_set` is human-only for automation; any client
//! may read the status.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host service the commands use.
pub const SERVICE: &str = "automation.mcp_server";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

fn host(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(SERVICE) {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::Unsupported, "the built-in MCP server is part of the desktop app; elsewhere run `nodecraft-cli mcp --listen`"))
    }
}

fn set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let Set { enabled } = params(value)?;
    host(c)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "set", "enabled": enabled}) })
}

fn status(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let Empty {} = params(value)?;
    host(c)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "status"}) })
}

fn enabled_set(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    set(c, value).map(|_| ())
}

fn enabled_status(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    status(c, value).map(|_| ())
}

fn set_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": {"enabled": {"type": "boolean"}}, "required": ["enabled"], "additionalProperties": false})
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": {}, "required": [], "additionalProperties": false})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "automation.mcp_server_set",
            label: "Enable MCP Server",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{enabled: bool} -> {enabled, state, url, error}; turns the desktop app's built-in MCP server on or off and remembers the choice. It listens on this computer only (http://127.0.0.1:<port>/mcp) and needs the bearer token shown in Settings › Automation. Starting finishes in the background: read the outcome with automation.mcp_server_status. Desktop app only; only a person runs it.",
            enabled: enabled_set,
            run: set,
        },
        CommandSpec {
            id: "automation.mcp_server_status",
            label: "MCP server status",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {enabled, state: \"off\"|\"starting\"|\"running\"|\"failed\", url: string|null, error: string|null}; whether the desktop app's built-in MCP server is on and where clients connect. Never includes the token. Read-only; desktop app only.",
            enabled: enabled_status,
            run: status,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn the_commands_ask_the_host_and_never_touch_the_document() {
        let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([92; 16]), "MCP test").unwrap();
        let mut session = crate::Session::new(project).unwrap();
        for (id, params) in [("automation.mcp_server_set", json!({"enabled": true})), ("automation.mcp_server_status", json!({}))] {
            assert_eq!(session.enabled(id, &params).unwrap_err().kind, ErrorKind::Unsupported, "{id} without a host");
            assert_eq!(session.dispatch(id, &params).unwrap_err().kind, ErrorKind::Unsupported);
        }
        let asked = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&asked);
        session
            .register_host_service(SERVICE, move |request| {
                seen.borrow_mut().push(request.clone());
                Ok(json!({"enabled": true, "state": "starting", "url": null, "error": null}))
            })
            .unwrap();
        let before = (session.document().clone(), session.revision(), session.can_undo());
        for bad in [json!({}), json!({"enabled": "yes"}), json!({"enabled": true, "port": 1}), json!(null)] {
            assert!(session.dispatch("automation.mcp_server_set", &bad).is_err(), "{bad}");
        }
        assert!(session.dispatch("automation.mcp_server_status", &json!({"token": true})).is_err());
        assert_eq!(session.dispatch("automation.mcp_server_set", &json!({"enabled": true})).unwrap()["state"], "starting");
        session.dispatch("automation.mcp_server_status", &json!({})).unwrap();
        assert_eq!(*asked.borrow(), [json!({"op": "set", "enabled": true}), json!({"op": "status"})]);
        assert_eq!((session.document().clone(), session.revision(), session.can_undo()), before);
    }
}
