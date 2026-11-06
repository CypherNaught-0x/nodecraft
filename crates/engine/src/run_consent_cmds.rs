//! Paid runs from automation (#1079): whether the control channel (and the MCP bridge over it),
//! the desktop app's built-in MCP server and the in-canvas agent may start runs that cost money.
//!
//! The desktop app keeps the decisions (Settings › Automation, `[automation.run_consent]`) and
//! asks the person the first time a channel's `run.start` would spend money: Allow once, Always
//! allow up to a per-run ceiling, or Never. It registers [`SERVICE`]; these commands only ask it.
//! Giving or changing consent is the person's decision, so `automation.run_consent_set` and
//! `automation.run_consent_answer` are human-only for automation; any client may read where it
//! stands with `automation.run_consent_list`. Without the app's window (the CLI) the commands
//! are refused as unsupported; the browser keeps decisions for the session only.
use crate::commands::{CommandSpec, Context, Effect, params};
use crate::settings::{MAX_CEILING_CURRENCIES, RUN_CONSENT_CLIENTS, RunConsentDecision, is_currency_code};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The host service the commands use.
pub const SERVICE: &str = "automation.run_consent";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    client: String,
    decision: String,
    #[serde(default)]
    ceiling: Option<BTreeMap<String, u64>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    answer: String,
    #[serde(default)]
    ceiling: Option<BTreeMap<String, u64>>,
}

fn host(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(SERVICE) {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::Unsupported, "paid runs from automation are decided in the desktop app's window"))
    }
}

/// A ceiling for "always": one to [`MAX_CEILING_CURRENCIES`] currency codes, each with a whole
/// number of micros above zero.
fn ceiling(decision: &str, ceiling: Option<BTreeMap<String, u64>>) -> Result<BTreeMap<String, u64>, Error> {
    match (decision, ceiling) {
        ("always", Some(ceiling)) => {
            if ceiling.is_empty() || ceiling.len() > MAX_CEILING_CURRENCIES {
                return Err(Error::new(ErrorKind::InvalidInput, format!("`ceiling` names 1 to {MAX_CEILING_CURRENCIES} currencies")));
            }
            if let Some(code) = ceiling.keys().find(|code| !is_currency_code(code)) {
                let shown: String = code.chars().take(16).collect();
                return Err(Error::new(ErrorKind::InvalidInput, format!("`{shown}` is not a currency code")));
            }
            if ceiling.values().any(|micros| *micros == 0) {
                return Err(Error::new(ErrorKind::InvalidInput, "a ceiling of 0 allows nothing; use `never` instead"));
            }
            Ok(ceiling)
        }
        ("always", None) => Err(Error::new(ErrorKind::InvalidInput, "`always` needs a `ceiling`: the most one run may cost, in micros per currency")),
        (_, Some(_)) => Err(Error::new(ErrorKind::InvalidInput, "only `always` takes a `ceiling`")),
        (_, None) => Ok(BTreeMap::new()),
    }
}

fn set(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let Set { client, decision, ceiling: asked } = params(value)?;
    if !RUN_CONSENT_CLIENTS.contains(&client.as_str()) {
        return Err(Error::new(ErrorKind::InvalidInput, "`client` is `control`, `mcp_server` or `agent`"));
    }
    if RunConsentDecision::parse(&decision).is_none() {
        return Err(Error::new(ErrorKind::InvalidInput, "`decision` is `ask`, `always` or `never`"));
    }
    let ceiling = ceiling(&decision, asked)?;
    host(c)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "set", "client": client, "decision": decision, "ceiling": ceiling}) })
}

fn answer(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let Answer { answer, ceiling: asked } = params(value)?;
    if !matches!(answer.as_str(), "once" | "always" | "never" | "decline") {
        return Err(Error::new(ErrorKind::InvalidInput, "`answer` is `once`, `always`, `never` or `decline`"));
    }
    let ceiling = ceiling(&answer, asked)?;
    host(c)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "answer", "answer": answer, "ceiling": ceiling}) })
}

fn list(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let Empty {} = params(value)?;
    host(c)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"op": "list"}) })
}

fn enabled_set(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    set(c, value).map(|_| ())
}

fn enabled_answer(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    answer(c, value).map(|_| ())
}

fn enabled_list(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    list(c, value).map(|_| ())
}

fn ceiling_schema() -> Value {
    json!({"type": "object", "minProperties": 1, "maxProperties": MAX_CEILING_CURRENCIES,
        "propertyNames": {"pattern": "^[A-Za-z0-9_]{1,16}$"}, "additionalProperties": {"type": "integer", "minimum": 1}})
}

fn set_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["client", "decision"], "properties": {
        "client": {"enum": RUN_CONSENT_CLIENTS},
        "decision": {"enum": ["ask", "always", "never"]},
        "ceiling": ceiling_schema()}})
}

fn answer_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["answer"], "properties": {
        "answer": {"enum": ["once", "always", "never", "decline"]},
        "ceiling": ceiling_schema()}})
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": {}, "required": [], "additionalProperties": false})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "automation.run_consent_list",
            label: "Paid runs from automation",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {clients: [{client: \"control\"|\"mcp_server\"|\"agent\", decision: \"ask\"|\"always\"|\"never\", ceiling: {CUR: micros}}], asking: {client, token}|null}; whether each automation channel may start paid runs in the desktop app, and which channel's run the window is asking the person about now. `always` covers runs that accept at most `ceiling` per currency and have no part of unknown price; others ask. Runs priced entirely free never ask. Read-only; desktop app only.",
            enabled: enabled_list,
            run: list,
        },
        CommandSpec {
            id: "automation.run_consent_set",
            label: "Set Paid Runs from Automation",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{client: \"control\"|\"mcp_server\"|\"agent\", decision: \"ask\"|\"always\"|\"never\", ceiling?: {CUR: micros}} -> the list; remembers whether `client` may start paid runs. `always` needs a `ceiling`, the most one run may accept per currency; `ask` forgets the decision (and any Allow once still unused), so the next paid run asks. Desktop app only; only a person runs it.",
            enabled: enabled_set,
            run: set,
        },
        CommandSpec {
            id: "automation.run_consent_answer",
            label: "Answer Paid Run Request",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: answer_schema,
            params_doc: "{answer: \"once\"|\"always\"|\"never\"|\"decline\", ceiling?: {CUR: micros}} -> the list; answers the question the window is asking about an automation client's paid run. `once` allows that run (its quote token) one time, `always` remembers the channel with `ceiling` (at least that run's price), `never` remembers a refusal, `decline` refuses this run alone. Desktop app only; only a person runs it.",
            enabled: enabled_answer,
            run: answer,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn the_commands_check_their_params_ask_the_host_and_never_touch_the_document() {
        let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([93; 16]), "Consent test").unwrap();
        let mut session = crate::Session::new(project).unwrap();
        let good = [
            ("automation.run_consent_list", json!({})),
            ("automation.run_consent_set", json!({"client": "control", "decision": "always", "ceiling": {"USD": 1_000_000}})),
            ("automation.run_consent_answer", json!({"answer": "once"})),
        ];
        for (id, params) in &good {
            assert_eq!(session.dispatch(id, params).unwrap_err().kind, ErrorKind::Unsupported, "{id} without a host");
        }
        let asked = Rc::new(RefCell::new(Vec::new()));
        let seen = Rc::clone(&asked);
        session
            .register_host_service(SERVICE, move |request| {
                seen.borrow_mut().push(request.clone());
                Ok(json!({"clients": [], "asking": null}))
            })
            .unwrap();
        let before = (session.document().clone(), session.revision(), session.can_undo());
        for bad in [
            json!({"client": "browser", "decision": "never"}),
            json!({"client": "control", "decision": "maybe"}),
            json!({"client": "control", "decision": "always"}),
            json!({"client": "control", "decision": "always", "ceiling": {}}),
            json!({"client": "control", "decision": "always", "ceiling": {"USD": 0}}),
            json!({"client": "control", "decision": "always", "ceiling": {"US D": 5}}),
            json!({"client": "control", "decision": "always", "ceiling": {"USD": -5}}),
            json!({"client": "control", "decision": "never", "ceiling": {"USD": 5}}),
            json!({"client": "control", "decision": "never", "extra": 1}),
            json!(null),
        ] {
            assert!(session.dispatch("automation.run_consent_set", &bad).is_err(), "{bad}");
        }
        for bad in [json!({"answer": "yes"}), json!({"answer": "always"}), json!({"answer": "once", "ceiling": {"USD": 1}}), json!({})] {
            assert!(session.dispatch("automation.run_consent_answer", &bad).is_err(), "{bad}");
        }
        assert!(session.dispatch("automation.run_consent_list", &json!({"all": true})).is_err());
        assert!(asked.borrow().is_empty(), "nothing invalid reached the host");
        for (id, params) in &good {
            session.dispatch(id, params).unwrap();
        }
        assert_eq!(
            *asked.borrow(),
            [
                json!({"op": "list"}),
                json!({"op": "set", "client": "control", "decision": "always", "ceiling": {"USD": 1_000_000}}),
                json!({"op": "answer", "answer": "once", "ceiling": {}}),
            ]
        );
        assert_eq!((session.document().clone(), session.revision(), session.can_undo()), before);
    }
}
