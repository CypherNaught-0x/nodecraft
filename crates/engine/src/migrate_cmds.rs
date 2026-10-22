//! `node.migrate` (#173): brings a card made with an older version of its kind (a plugin card
//! after its package was updated) to the kind's current ports and settings, in one undo step.
//! `dry_run` only says what would change; the warnings name every input, output and setting the
//! migration drops, and connections to dropped ports go in the same edit.
use crate::commands::{CommandSpec, Context, Effect, params};
use nodecraft_graph::{Change, Changeset, Node};
use nodecraft_nodes::NodeMigration;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrateParams {
    canvas: CanvasId,
    node: NodeId,
    #[serde(default)]
    dry_run: bool,
}

fn proposed(c: &Context<'_>, p: &MigrateParams) -> Result<(Option<NodeMigration>, Vec<Change>), Error> {
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "canvas does not exist"))?;
    let node = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, "node does not exist"))?;
    let Some(migration) = c.kinds.get(&node.kind)?.prepare_migration(node)? else { return Ok((None, Vec::new())) };
    let gone = |ports: &[nodecraft_graph::Port], now: &[nodecraft_graph::Port], name: &str| {
        ports.iter().any(|port| port.name == name) && !now.iter().any(|port| port.name == name && ports.contains(port))
    };
    let dropped = |next: &Node, edge: &nodecraft_graph::Edge| {
        (edge.to.node == node.id && gone(&node.inputs, &next.inputs, &edge.to.port))
            || (edge.from.node == node.id && gone(&node.outputs, &next.outputs, &edge.from.port))
    };
    let mut changes: Vec<Change> =
        canvas.edges().values().filter(|edge| dropped(&migration.node, edge)).map(|edge| Change::Disconnect { canvas: p.canvas, id: edge.id }).collect();
    changes.push(Change::SetNode { canvas: p.canvas, node: migration.node.clone() });
    Ok((Some(migration), changes))
}

fn migrate(c: &Context<'_>, value: &Value) -> Result<Effect, Error> {
    let p: MigrateParams = params(value)?;
    let (migration, changes) = proposed(c, &p)?;
    let Some(migration) = migration else { return Ok(Effect::None { result: json!({"migrated": false, "warnings": []}) }) };
    let disconnected = changes.len().saturating_sub(1);
    if p.dry_run {
        return Ok(Effect::None { result: json!({"migrated": false, "pending": true, "disconnected": disconnected, "warnings": migration.warnings}) });
    }
    let result = json!({"migrated": true, "disconnected": disconnected, "warnings": migration.warnings});
    Ok(Effect::Edit { changes: Changeset::new(changes), result, gesture: None })
}

fn enabled(c: &Context<'_>, value: &Value) -> Result<(), Error> {
    let p: MigrateParams = params(value)?;
    if !p.dry_run {
        c.editable()?;
    }
    let (_, changes) = proposed(c, &p)?;
    c.preview_changes(&Changeset::new(changes))
}

fn schema() -> Value {
    json!({"$schema": "https://json-schema.org/draft/2020-12/schema", "type": "object", "additionalProperties": false,
        "required": ["canvas", "node"], "properties": {
            "canvas": {"type": "string", "format": "uuid"},
            "node": {"type": "string", "format": "uuid"},
            "dry_run": {"type": "boolean", "description": "Only report what would change."}}})
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "node.migrate",
        label: "Update card to its current version",
        shortcut: None,
        alternate_shortcuts: &[],
        input_schema: schema,
        params_doc: "{canvas, node, dry_run?} -> {migrated, pending?, disconnected, warnings: [string]}; brings a card made with an older version of its kind (a plugin card after its package was updated) to the current ports and settings, in one undo step. Connections to dropped ports are removed in the same edit; warnings name everything dropped. dry_run reports without changing anything (pending: true when there is something to migrate). Nothing to migrate is {migrated: false, warnings: []}.",
        enabled,
        run: migrate,
    }]
}

#[cfg(test)]
mod tests;
