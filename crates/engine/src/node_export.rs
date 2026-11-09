//! `export.node_plan` / `export.node_run` (#746): perform what an Export to folder card
//! (`media.export`) describes.
//!
//! The card never runs (the planner resolves it away), so nothing a run, recompute, undo, redo,
//! autosave or replay does can write a file. Writing happens only here, when a person (or, for the
//! preview, anyone) dispatches one of these commands. Each translates the card into exactly the
//! `export.local_plan` / `export.local_run` request — the active results wired into it, its
//! template, collision policy and format, and its subfolder inside the folder the host granted —
//! and hands that to the same builder, so a card can never mean anything a direct request could
//! not. The destination comes only from the injected [`crate::local_export::ExportHost`]: the
//! project holds no path, and a command parameter cannot name one. See
//! `docs/decisions/2026-10-28-media-export-node.md`.
use crate::commands::{CommandSpec, Context, Effect, params};
use crate::local_export::{MAX_SOURCES, Request, Source, build_request, host, planned_json};
use nodecraft_format::export::{Collision, FormatRequest, reasons};
use nodecraft_nodes::export;
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, Value};
use serde::Deserialize;
use serde_json::{Value as Json, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeRequest {
    canvas: CanvasId,
    node: NodeId,
    /// Write on the shared background export worker; see `export.local_run`.
    #[serde(default)]
    background: bool,
}

fn invalid(message: &'static str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// The wired sources of `card`, in port order (image, video, audio, mesh, document) then wire
/// order, routers resolved. A list output contributes each item. Bounded: one source past the
/// batch limit is refused rather than silently dropped.
fn sources(c: &Context<'_>, canvas: &nodecraft_graph::Canvas, card: NodeId) -> Result<Vec<Source>, Error> {
    let incoming = nodecraft_exec::routing::incoming(canvas, c.kinds)?;
    if incoming.invalid.keys().any(|(node, _)| *node == card) {
        return Err(invalid("a router wired into this card is invalid; fix or remove it before exporting"));
    }
    let feeds = incoming.feeds.get(&card).map(Vec::as_slice).unwrap_or_default();
    let mut found = Vec::new();
    for (port, _) in export::INPUTS {
        for (_, _, from, from_port) in feeds.iter().filter(|feed| feed.0 == port) {
            let items = canvas.nodes().get(from).and_then(|upstream| {
                let generation = upstream.history.iter().find(|g| Some(g.id) == upstream.active)?;
                let index = upstream.outputs.iter().position(|p| p.name == *from_port)?;
                match generation.outputs.get(index)? {
                    Value::List(items) => {
                        Some(crate::history_cmds::list_items(items, !generation.items.is_empty()).take(MAX_SOURCES.saturating_add(1)).count())
                    }
                    _ => None,
                }
            });
            match items {
                // A single value, or one the shared resolver will explain (no result, a missing
                // node, a changed port layout): one entry either way.
                None => found.push(Source { node: *from, port: (*from_port).to_owned(), item: None }),
                Some(count) => {
                    for item in 0..count {
                        if found.len() > MAX_SOURCES {
                            break;
                        }
                        let item = u32::try_from(item).map_err(|_| invalid("an output has too many items to export"))?;
                        found.push(Source { node: *from, port: (*from_port).to_owned(), item: Some(item) });
                    }
                }
            }
            if found.len() > MAX_SOURCES {
                return Err(invalid("an Export to folder card writes at most 256 files at a time; wire fewer results into it"));
            }
        }
    }
    if found.is_empty() {
        return Err(invalid("wire image, video, audio, 3D or document results into this card to export them"));
    }
    Ok(found)
}

/// The `export.local_*` request this card stands for.
fn request(c: &Context<'_>, parameters: &Json) -> Result<Request, Error> {
    let p: NodeRequest = params(parameters)?;
    let canvas = c.document.canvases().get(&p.canvas).ok_or_else(|| Error::new(ErrorKind::NotFound, "no canvas with this id is open in this project"))?;
    let node = canvas.nodes().get(&p.node).ok_or_else(|| Error::new(ErrorKind::NotFound, reasons::UNKNOWN_NODE))?;
    if node.kind != export::ID {
        return Err(invalid("this is not an Export to folder card"));
    }
    let settings = export::settings(c.kinds.get(export::ID)?, node)?;
    let on_collision = match settings.on_collision.as_str() {
        "skip" => Collision::Skip,
        "numbered" => Collision::Numbered,
        "overwrite" => Collision::Overwrite,
        _ => return Err(invalid("choose what to do when a file exists: keep, number or replace")),
    };
    if settings.format != "original" {
        return Err(Error::new(ErrorKind::Unsupported, "only the original file format is available in this build"));
    }
    // The grant first: without a folder a person chose there is nothing to plan against, and the
    // reason (desktop app, or no folder chosen yet) is the actionable one.
    let grant = host(c)?;
    let mut destination = grant.primary().ok_or_else(|| Error::new(ErrorKind::Unsupported, reasons::NO_HOST))?.to_path_buf();
    for part in &settings.subfolder {
        destination.push(part);
    }
    let destination = destination.to_str().ok_or_else(|| invalid("the chosen folder's name can't be used for export; choose another folder"))?.to_owned();
    let sources = sources(c, canvas, node.id)?;
    Ok(Request {
        canvas: p.canvas,
        sources,
        destination,
        template: Some(settings.template),
        on_collision: Some(on_collision),
        format: Some(FormatRequest::default()),
        background: p.background,
    })
}

fn plan(c: &Context<'_>, parameters: &Json) -> Result<Json, Error> {
    let request = request(c, parameters)?;
    let plan = build_request(c, &request, false)?;
    let mut result = planned_json(&plan);
    // So a person can be asked before `overwrite` replaces anything (#746). Unknown is null.
    result["existing"] = json!(crate::local_export::existing_files(host(c)?, &plan));
    Ok(result)
}

fn run(c: &Context<'_>, parameters: &Json) -> Result<Effect, Error> {
    let request = request(c, parameters)?;
    let plan = build_request(c, &request, true)?;
    Ok(Effect::LocalExport { plan, background: request.background })
}

fn schema() -> Json {
    let uuid = json!({"type":"string","format":"uuid"});
    json!({"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,
        "required":["canvas","node"],
        "properties":{"canvas":uuid,"node":uuid,
            "background":{"type":"boolean","default":false,
                "description":"Write on the background export worker and return {pending:id}; poll export.status and stop with export.cancel."}}})
}

const PLAN_DOC: &str = "Preview what an Export to folder card (media.export) would write, without writing. Params: {canvas:uuid, \
    node:uuid}. The sources are the active results wired into the card (each item of a list output), its template, \
    on_collision and format parameters, and its subfolder inside the folder a person chose for the card in the desktop app. \
    Returns the same entries as export.local_plan, plus existing: how many intended names already exist in the \
    folder (what overwrite would replace; null when unknown). Without a chosen folder it is Unsupported: only a person can choose one, \
    in the desktop app, so callers without an export host (MCP, the CLI, the in-canvas agent) always get Unsupported.";

const RUN_DOC: &str = "Write what an Export to folder card (media.export) describes into the folder a person chose for it. \
    Params: {canvas:uuid, node:uuid, background?:bool}. Same request, outcome and guarantees as export.local_run; the card \
    itself never runs, so runs, recompute, undo and autosave never write files. No dialogs; without a chosen folder it is \
    Unsupported.";

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "export.node_plan",
            label: "Preview card export",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: PLAN_DOC,
            enabled: |c, v| plan(c, v).map(|_| ()),
            run: |c, v| plan(c, v).map(|result| Effect::None { result }),
        },
        CommandSpec {
            id: "export.node_run",
            label: "Export card to folder",
            shortcut: None,
            alternate_shortcuts: &[],
            input_schema: schema,
            params_doc: RUN_DOC,
            enabled: |c, v| run(c, v).map(|_| ()),
            run,
        },
    ]
}

#[cfg(test)]
mod tests;
