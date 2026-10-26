//! A desktop host can focus the window owning its recovery journal (#968).
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

pub const SERVICE: &str = "file.owner_window";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
fn run(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let _: Empty = params(value)?;
    if !c.services.contains(SERVICE) {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "The storage window cannot be focused automatically. Switch to the other NodeCraft window and use its File menu.",
        ));
    }
    Ok(Effect::Service { capability: SERVICE, request: json!({}) })
}
fn enabled(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    run(c, value).map(|_| ())
}
fn schema() -> Value {
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema", "type":"object", "properties":{}, "required":[], "additionalProperties":false})
}
pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "file.focus_owner",
        label: "Focus project storage window",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{}; asks the desktop window owning the recovery journal to come forward. Returns {requested: true}. Does not open a project or change either document. Requires a desktop owner-window host.",
        enabled,
        run,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focus_requires_host_and_never_changes_or_opens_a_document() {
        let project = nodecraft_graph::Project::new(nodecraft_types::ProjectId::from_bytes([91; 16]), "Focus test").unwrap();
        let mut session = crate::Session::new(project).unwrap();
        assert!(session.enabled("file.focus_owner", &json!({})).is_err());
        session.register_host_service(SERVICE, |_| Ok(json!({"requested":true}))).unwrap();
        let before = (session.document().clone(), session.revision(), session.can_undo());
        assert!(session.dispatch("file.focus_owner", &json!({"location":"untrusted"})).is_err());
        assert_eq!(session.dispatch("file.focus_owner", &json!({})).unwrap(), json!({"requested":true}));
        assert_eq!((session.document().clone(), session.revision(), session.can_undo()), before);
    }
}
