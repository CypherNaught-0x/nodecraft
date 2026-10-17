//! `keys.*` commands: store, remove and check provider API keys. They change no document and add
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
        "key": {"type": "string", "minLength": 1, "maxLength": 1024, "description": "The API key (visible ASCII). Stored in the OS keychain only."}
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

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "keys.set",
            label: "Set provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{account: string, key: string}; stores the key in the OS keychain. Returns {stored, masked}; never echoes the key.",
            enabled: always,
            run: set,
        },
        CommandSpec {
            id: "keys.remove",
            label: "Remove provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: account_only_schema,
            params_doc: "{account: string}; removes the key. Returns {removed}.",
            enabled: always,
            run: remove,
        },
        CommandSpec {
            id: "keys.status",
            label: "Check provider key",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: account_only_schema,
            params_doc: "{account: string}; whether a key is stored. Returns {stored, masked}.",
            enabled: always,
            run: status,
        },
    ]
}
