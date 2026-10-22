//! Share links for a project shared live (#121), kept by whoever shared it.
//!
//! - `share.links` (read-only): the links, by id, with their terms.
//! - `share.link_create {role, name, expires?}` (human only): a new link; its key is shown
//!   once.
//! - `share.link_update {id, role, name, expires?}` (human only): new terms; whoever is in
//!   through it comes back in on them.
//! - `share.link_revoke {id}`: takes a link back; whoever is in through it is put out at once.
//!
//! A link lets its holder view, comment on or edit the project, until `expires` (seconds since
//! 1970, UTC) if set. Nothing here changes the document: the links live in the shared session.
//! Host contract: provide [`LINKS_SERVICE`] while a session is shared, and pass each request
//! to [`crate::collab::Collab::links_request`].

use crate::collab::LINKS_SERVICE;
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::validate_author;
use nodecraft_sync::Role;
use nodecraft_types::{Error, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListParams {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateParams {
    role: Role,
    name: String,
    #[serde(default)]
    expires: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    id: String,
    role: Role,
    name: String,
    #[serde(default)]
    expires: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdParams {
    id: String,
}

fn host(c: &Context<'_>) -> Result<(), Error> {
    if c.services.contains(LINKS_SERVICE) { Ok(()) } else { Err(Error::new(ErrorKind::Unsupported, "this project isn't shared live")) }
}

fn terms(role: Role, name: &str) -> Result<(), Error> {
    if role == Role::Owner {
        return Err(Error::new(ErrorKind::InvalidInput, "a share link can't make its holder the owner"));
    }
    validate_author(name)
}

fn id(id: &str) -> Result<(), Error> {
    if id.len() == 64 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(())
    } else {
        Err(Error::new(ErrorKind::InvalidInput, "a share link id is 64 lowercase hex digits"))
    }
}

fn enabled_list(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    params::<ListParams>(value)?;
    host(c)
}

fn run_list(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_list(c, value)?;
    Ok(Effect::Service { capability: LINKS_SERVICE, request: json!({"op": "list"}) })
}

fn enabled_create(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p = params::<CreateParams>(value)?;
    terms(p.role, &p.name)?;
    host(c)
}

fn run_create(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_create(c, value)?;
    let p = params::<CreateParams>(value)?;
    Ok(Effect::Service { capability: LINKS_SERVICE, request: json!({"op": "create", "role": p.role, "name": p.name, "expires": p.expires}) })
}

fn enabled_update(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p = params::<UpdateParams>(value)?;
    id(&p.id)?;
    terms(p.role, &p.name)?;
    host(c)
}

fn run_update(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_update(c, value)?;
    let p = params::<UpdateParams>(value)?;
    Ok(Effect::Service { capability: LINKS_SERVICE, request: json!({"op": "update", "id": p.id, "role": p.role, "name": p.name, "expires": p.expires}) })
}

fn enabled_revoke(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    id(&params::<IdParams>(value)?.id)?;
    host(c)
}

fn run_revoke(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    enabled_revoke(c, value)?;
    let p = params::<IdParams>(value)?;
    Ok(Effect::Service { capability: LINKS_SERVICE, request: json!({"op": "revoke", "id": p.id}) })
}

fn list_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": [], "properties": {}})
}

fn terms_properties() -> Value {
    json!({
        "role": {"enum": ["view", "comment", "edit"]},
        "name": {"type": "string", "minLength": 1, "maxLength": nodecraft_graph::MAX_AUTHOR_BYTES},
        "expires": {"type": ["integer", "null"], "minimum": 0, "description": "When the link stops working, in seconds since 1970 (UTC); absent or null for never."}
    })
}

fn create_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["role", "name"], "properties": terms_properties()})
}

fn update_schema() -> Value {
    let mut properties = terms_properties();
    if let Value::Object(fields) = &mut properties {
        fields.insert("id".into(), json!({"type": "string", "pattern": "^[0-9a-f]{64}$"}));
    }
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["id", "role", "name"], "properties": properties})
}

fn id_schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false, "required": ["id"],
        "properties": {"id": {"type": "string", "pattern": "^[0-9a-f]{64}$"}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "share.links",
            label: "List share links",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: list_schema,
            params_doc: "{} -> {links: [{id, role, name, expires, expired}], max}. The share links of the project this session shared live. A link is known by its id; its key, which lets someone in, isn't shown again. `expires` is seconds since 1970 (UTC), null for never. Read-only; only who shared the project may list them.",
            enabled: enabled_list,
            run: run_list,
        },
        CommandSpec {
            id: "share.link_create",
            label: "Create a share link",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: create_schema,
            params_doc: "{role: view|comment|edit, name, expires?} -> {id, key, role, name, expires, expired}. A new link into the shared project for the person `name`, whose discussions it signs. view sees everything and changes nothing; comment also starts, answers and resolves discussions, editing and deleting only its own messages; edit changes the project as its owner does. `key` lets whoever holds it in and is shown only now. Refused unless this session shared the project. Human only.",
            enabled: enabled_create,
            run: run_create,
        },
        CommandSpec {
            id: "share.link_update",
            label: "Change a share link",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: update_schema,
            params_doc: "{id, role: view|comment|edit, name, expires?} -> {id, role, name, expires, expired}. Gives a link new terms; whoever is in through it is put out and comes back in on them. Human only.",
            enabled: enabled_update,
            run: run_update,
        },
        CommandSpec {
            id: "share.link_revoke",
            label: "Revoke a share link",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: id_schema,
            params_doc: "{id} -> {id, revoked: true}. Takes a link back: whoever is in through it is put out, and it no longer lets anyone in or fetches any media.",
            enabled: enabled_revoke,
            run: run_revoke,
        },
    ]
}
