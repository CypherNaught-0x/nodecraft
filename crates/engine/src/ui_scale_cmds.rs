//! Interface size (#1107): `view.ui_scale.*` make the shell's panels, menus, dialogs, top bar and
//! minimap larger or smaller, from 80 % to 200 %.
//!
//! The size is a setting of this installation ([`crate::settings::InterfaceSize`], kept in
//! `[appearance] interface_size`), never of a project: these commands record no document change
//! and no undo step. A frontend that draws an interface [`install`]s a [`UiScale`] it shares with
//! the session; the commands read it to decide what they would do, and the host service they ask
//! sets it. The frontend applies and saves whatever the shared value holds. Without a frontend
//! (`nodecraft-cli`, a headless session) the commands are refused as unsupported.
use std::cell::Cell;
use std::rc::Rc;

use crate::commands::{CommandSpec, Context, Effect, params};
use crate::session::Session;
use crate::settings::InterfaceSize;
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

/// The host service the commands ask.
pub const SERVICE: &str = "view.ui_scale";

/// The interface size a frontend draws at, shared between it and the session.
#[derive(Clone, Debug, Default)]
pub struct UiScale(Rc<Cell<InterfaceSize>>);

impl UiScale {
    pub fn new(size: InterfaceSize) -> Self {
        Self(Rc::new(Cell::new(size)))
    }

    pub fn get(&self) -> InterfaceSize {
        self.0.get()
    }

    /// Sets the size, as a frontend does when it reads its settings.
    pub fn set(&self, size: InterfaceSize) {
        self.0.set(size);
    }
}

/// Gives `session` the frontend's interface size: the commands read `scale` and set it.
pub fn install(session: &mut Session, scale: UiScale) -> Result<(), Error> {
    session.swap_resource(Some(scale.clone()));
    session.register_host_service(SERVICE, move |request: &Value| {
        let percent = request.get("percent").and_then(Value::as_f64).ok_or_else(|| Error::new(ErrorKind::InvalidInput, "malformed interface size request"))?;
        let size = InterfaceSize::from_percent(percent).ok_or_else(out_of_range)?;
        let previous = scale.get();
        scale.set(size);
        Ok(json!({"percent": size.percent(), "previous": previous.percent()}))
    })
}

fn out_of_range() -> Error {
    Error::new(ErrorKind::InvalidInput, format!("the interface size must be a number from {} to {} percent", InterfaceSize::MIN, InterfaceSize::MAX))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Set {
    percent: f64,
}

#[derive(Clone, Copy)]
enum Op {
    Increase,
    Decrease,
    Reset,
    Set,
}

/// The size the command would set, after checking its parameters and that a frontend is there.
fn target(c: &Context<'_>, value: &Value, op: Op) -> Result<InterfaceSize, Error> {
    let wanted = match op {
        Op::Set => {
            let Set { percent } = params(value)?;
            Some(InterfaceSize::from_percent(percent).ok_or_else(out_of_range)?)
        }
        Op::Increase | Op::Decrease | Op::Reset => {
            let Empty {} = params(value)?;
            None
        }
    };
    let current = match c.resources.get::<UiScale>() {
        Some(scale) if c.services.contains(SERVICE) => scale.get(),
        _ => {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "the interface size belongs to the app's window; run this in NodeCraft, or through its control channel",
            ));
        }
    };
    let next = match (op, wanted) {
        (Op::Set, Some(size)) => size,
        (Op::Increase, _) if current.percent() >= InterfaceSize::MAX => {
            return Err(Error::new(ErrorKind::Conflict, "the interface is already at its largest size, 200 %"));
        }
        (Op::Decrease, _) if current.percent() <= InterfaceSize::MIN => {
            return Err(Error::new(ErrorKind::Conflict, "the interface is already at its smallest size, 80 %"));
        }
        (Op::Reset, _) if current.percent() == InterfaceSize::DEFAULT && !current.damaged() => {
            return Err(Error::new(ErrorKind::Conflict, "the interface is already at its usual size, 100 %"));
        }
        (Op::Increase, _) => current.larger(),
        (Op::Decrease, _) => current.smaller(),
        _ => InterfaceSize::default(),
    };
    Ok(next)
}

fn request(c: &Context<'_>, value: &Value, op: Op) -> Result<Effect, Error> {
    let size = target(c, value, op)?;
    Ok(Effect::Service { capability: SERVICE, request: json!({"percent": size.percent()}) })
}

fn increase(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    request(c, v, Op::Increase)
}
fn decrease(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    request(c, v, Op::Decrease)
}
fn reset(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    request(c, v, Op::Reset)
}
fn set(c: &Context<'_>, v: &Value) -> Result<Effect, Error> {
    request(c, v, Op::Set)
}

fn empty_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": {}, "required": [], "additionalProperties": false})
}

fn set_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "properties": {"percent": {"type": "number", "minimum": InterfaceSize::MIN, "maximum": InterfaceSize::MAX, "description": "Interface size in percent; rounded to a whole percent"}}, "required": ["percent"], "additionalProperties": false})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "view.ui_scale.increase",
            label: "Larger interface",
            shortcut: Some("Command+Alt+="),
            alternate_shortcuts: &["Command+Alt+Plus"],
            input_schema: empty_schema,
            params_doc: "{} -> {percent, previous}; draws panels, menus and dialogs one step (10 %) larger, at most 200 %, and remembers it for this installation. Canvas zoom is separate. App window only; no document change.",
            enabled: |c, v| target(c, v, Op::Increase).map(|_| ()),
            run: increase,
        },
        CommandSpec {
            id: "view.ui_scale.decrease",
            label: "Smaller interface",
            shortcut: Some("Command+Alt+Minus"),
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {percent, previous}; draws panels, menus and dialogs one step (10 %) smaller, at least 80 %, and remembers it for this installation. App window only; no document change.",
            enabled: |c, v| target(c, v, Op::Decrease).map(|_| ()),
            run: decrease,
        },
        CommandSpec {
            id: "view.ui_scale.reset",
            label: "Reset interface size",
            shortcut: Some("Command+Alt+0"),
            alternate_shortcuts: &[],
            input_schema: empty_schema,
            params_doc: "{} -> {percent, previous}; draws panels, menus and dialogs at the usual size, 100 %. App window only; no document change.",
            enabled: |c, v| target(c, v, Op::Reset).map(|_| ()),
            run: reset,
        },
        CommandSpec {
            id: "view.ui_scale.set",
            label: "Set interface size",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: set_schema,
            params_doc: "{percent: number 80..=200} -> {percent, previous}; draws panels, menus and dialogs at `percent` (rounded to a whole percent) and remembers it for this installation. App window only; no document change.",
            enabled: |c, v| target(c, v, Op::Set).map(|_| ()),
            run: set,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([107; 16]), "Interface size").unwrap();
        Session::new(project).unwrap()
    }

    const IDS: [&str; 4] = ["view.ui_scale.increase", "view.ui_scale.decrease", "view.ui_scale.reset", "view.ui_scale.set"];

    #[test]
    fn without_a_window_every_command_is_unsupported() {
        let mut session = session();
        for id in IDS {
            let params = if id == "view.ui_scale.set" { json!({"percent": 150}) } else { json!({}) };
            assert_eq!(session.enabled(id, &params).unwrap_err().kind, ErrorKind::Unsupported, "{id}");
            assert_eq!(session.dispatch(id, &params).unwrap_err().kind, ErrorKind::Unsupported, "{id}");
        }
    }

    #[test]
    fn the_commands_step_set_and_reset_without_touching_the_document() {
        let mut session = session();
        let scale = UiScale::default();
        install(&mut session, scale.clone()).unwrap();
        let before = (session.document().clone(), session.revision(), session.can_undo());
        assert_eq!(session.enabled("view.ui_scale.reset", &json!({})).unwrap_err().kind, ErrorKind::Conflict, "already 100 %");
        assert_eq!(session.dispatch("view.ui_scale.increase", &json!({})).unwrap(), json!({"percent": 110, "previous": 100}));
        assert_eq!(scale.get().percent(), 110);
        assert_eq!(session.dispatch("view.ui_scale.set", &json!({"percent": 172.4})).unwrap()["percent"], 172);
        assert_eq!(session.dispatch("view.ui_scale.decrease", &json!({})).unwrap()["percent"], 170);
        assert_eq!(session.dispatch("view.ui_scale.set", &json!({"percent": 200})).unwrap()["percent"], 200);
        assert_eq!(session.enabled("view.ui_scale.increase", &json!({})).unwrap_err().kind, ErrorKind::Conflict, "at the largest");
        assert_eq!(session.dispatch("view.ui_scale.reset", &json!({})).unwrap()["percent"], 100);
        session.dispatch("view.ui_scale.set", &json!({"percent": 80})).unwrap();
        assert_eq!(session.enabled("view.ui_scale.decrease", &json!({})).unwrap_err().kind, ErrorKind::Conflict, "at the smallest");
        assert_eq!((session.document().clone(), session.revision(), session.can_undo()), before, "view state only");
        // A damaged setting reads as 100 % and can be reset to clear the mark.
        scale.set(InterfaceSize::parse("big"));
        assert!(session.enabled("view.ui_scale.reset", &json!({})).is_ok());
    }

    #[test]
    fn hostile_sizes_are_refused_and_change_nothing() {
        let mut session = session();
        let scale = UiScale::new(InterfaceSize::from_percent(130.0).unwrap());
        install(&mut session, scale.clone()).unwrap();
        for bad in [
            json!({"percent": 0}),
            json!({"percent": 1e9}),
            json!({"percent": -150}),
            json!({"percent": 79.4}),
            json!({"percent": 200.6}),
            json!({"percent": f64::MAX}),
            // JSON has no NaN: serde_json writes it as null.
            json!({"percent": f64::NAN}),
            json!({"percent": "150"}),
            json!({"percent": 150, "extra": 1}),
            json!({}),
            json!(null),
        ] {
            assert!(session.enabled("view.ui_scale.set", &bad).is_err(), "{bad}");
            let error = session.dispatch("view.ui_scale.set", &bad).unwrap_err();
            assert_eq!(error.kind, ErrorKind::InvalidInput, "{bad}");
            assert_eq!(scale.get().percent(), 130, "{bad}");
        }
        for id in ["view.ui_scale.increase", "view.ui_scale.decrease", "view.ui_scale.reset"] {
            assert_eq!(session.dispatch(id, &json!({"percent": 150})).unwrap_err().kind, ErrorKind::InvalidInput, "{id}");
        }
        assert_eq!(scale.get().percent(), 130);
    }
}
