//! `keys.*` commands: store, remove, look up and test provider API keys. They change no document and add
//! no undo step; the host's `keys` service ([`crate::secrets::register_key_service`]) talks to the
//! keychain. Results never contain a key, only a masked tail.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, Context, Effect, params};
use crate::secrets::KEY_SERVICE;
use nodecraft_types::Error;

fn account_schema() -> Value {
    json!({"type": "string", "minLength": 1, "maxLength": 128, "pattern": "^[a-z0-9_.:\\[\\]-]+$", "description": "Provider key name, e.g. openai, anthropic, comfyui:gpu-box:8188"})
}

fn set_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["account", "key"], "properties": {
        "account": account_schema(),
        "key": {"type": "string", "minLength": 1, "maxLength": 16384, "description": "The API key (visible ASCII), or for account google-cloud the whole service-account key file (JSON), which is stored compactly. Stored in the OS keychain only."}
    }})
}

fn account_only_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["account"], "properties": {"account": account_schema()}})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    account: String,
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountParams {
    account: String,
}

fn always(_: &Context<'_>, _: &Value) -> Result<(), Error> {
    Ok(())
}

fn set(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let p: SetParams = params(p)?;
    Ok(Effect::Service { capability: KEY_SERVICE, request: json!({"op": "set", "account": p.account, "key": p.key}) })
}

fn remove(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let p: AccountParams = params(p)?;
    Ok(Effect::Service { capability: KEY_SERVICE, request: json!({"op": "remove", "account": p.account}) })
}

fn status(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let p: AccountParams = params(p)?;
    Ok(Effect::Service { capability: KEY_SERVICE, request: json!({"op": "status", "account": p.account}) })
}

fn test(_: &Context<'_>, p: &Value) -> Result<Effect, Error> {
    let p: AccountParams = params(p)?;
    Ok(Effect::Service { capability: KEY_SERVICE, request: json!({"op": "test", "account": p.account}) })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "keys.set",
            label: "Set provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{account: string, key: string}; stores the key in the OS keychain. Returns {stored, masked}, or {pending: id} in the app, where the keychain answers later; never echoes the key.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "keys.remove",
            label: "Remove provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: account_only_schema,
            params_doc: "{account: string}; removes the key. Returns {removed}, or {pending: id} in the app.",
            enabled: always,
            run: remove,
        },
        CommandSpec {
            id: "keys.status",
            label: "Check provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: account_only_schema,
            params_doc: "{account: string}; whether a key is stored. Returns {stored, masked}, or {pending: id} in the app.",
            enabled: always,
            run: status,
        },
        CommandSpec {
            id: "keys.test",
            label: "Test provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: account_only_schema,
            params_doc: "{account: string}; checks the stored key with a free request to the provider (sent only to its own API). Returns {result: \"verified\"} or {result: \"unverifiable\", reason}; a rejected key is an Authentication error. In the app returns {pending: id}; hosts that can't send the check (CLI, MCP, browser) answer Unsupported.",
            enabled: always,
            run: test,
        },
    ]
}
