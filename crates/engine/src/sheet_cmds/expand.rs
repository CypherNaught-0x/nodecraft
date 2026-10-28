//! `sheet.expand` (#163): one prompt template with named variables becomes many rows, one per
//! combination of the variables' values (their Cartesian product), each repeated `copies` times
//! (`docs/decisions/2026-10-28-sheet-templates.md`).
//!
//! A text variable fills its `{{name}}` placeholders; an image variable wires one reference node
//! per row. New rows copy the row they follow like `sheet.rows_add`, and each records where it
//! came from under [`PROVENANCE_CONFIG`]. `dry_run` reports the count, whether it fits and the
//! first prompts without editing anything; a real expansion is one changeset, so one undo
//! removes every row it made. Nothing runs: the rows are quoted and run like any others.
use super::template::{self, Piece, clip};
use super::*;
use nodecraft_exec::staleness::PROVENANCE_CONFIG;
use nodecraft_types::Modality;

/// Most variables one template uses.
pub const MAX_VARIABLES: usize = 8;
/// Most values one variable lists.
pub const MAX_VALUES: usize = MAX_ADD;
/// Longest text value, in bytes.
pub const MAX_VALUE_BYTES: usize = 4096;
/// Most copies of each combination.
pub const MAX_COPIES: u64 = MAX_ADD as u64;
/// Prompts a dry run shows.
const PREVIEW: usize = 5;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandParams {
    canvas: CanvasId,
    seed: NodeId,
    /// The row the new ones follow and copy; the last row when absent.
    #[serde(default)]
    after: Option<NodeId>,
    /// For an empty sheet's first rows: any catalog-backed generation kind.
    #[serde(default)]
    kind: Option<String>,
    template: String,
    #[serde(default)]
    variables: Vec<VariableParams>,
    #[serde(default)]
    copies: Option<u64>,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VariableParams {
    name: String,
    #[serde(default)]
    text: Option<Vec<String>>,
    #[serde(default)]
    images: Option<Vec<NodeId>>,
}

enum Values<'a> {
    Text(&'a [String]),
    Images(Vec<&'a Node>),
}

struct Variable<'a> {
    name: &'a str,
    values: Values<'a>,
}

impl Variable<'_> {
    fn len(&self) -> usize {
        match &self.values {
            Values::Text(values) => values.len(),
            Values::Images(values) => values.len(),
        }
    }
}

fn bad(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}

/// The variables, checked against the template and the sheet.
fn variables<'a>(given: &'a [VariableParams], pieces: &[Piece], canvas: &'a Canvas) -> Result<Vec<Variable<'a>>, Error> {
    if given.len() > MAX_VARIABLES {
        return Err(bad(format!("a template has at most {MAX_VARIABLES} variables")));
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(given.len());
    for variable in given {
        let name = variable.name.as_str();
        if !template::valid_name(name) {
            return Err(bad(format!("`{}` can't name a variable: use 1 to 32 letters, digits, `_` or `-`", clip(name))));
        }
        if !seen.insert(name.to_ascii_lowercase()) {
            return Err(bad(format!("two variables are named `{name}`; names differ by more than case")));
        }
        let values = match (&variable.text, &variable.images) {
            (Some(values), None) => {
                check_count(name, values.len())?;
                let mut distinct = BTreeSet::new();
                for value in values {
                    if value.len() > MAX_VALUE_BYTES {
                        return Err(bad(format!("a value of `{name}` is longer than {MAX_VALUE_BYTES} bytes")));
                    }
                    if !distinct.insert(value.as_str()) {
                        return Err(bad(format!("`{name}` lists \"{}\" twice; use copies to repeat rows", clip(value))));
                    }
                }
                Values::Text(values)
            }
            (None, Some(ids)) => {
                check_count(name, ids.len())?;
                let mut distinct = BTreeSet::new();
                let mut nodes = Vec::with_capacity(ids.len());
                for id in ids {
                    if !distinct.insert(*id) {
                        return Err(bad(format!("`{name}` lists reference {id} twice; use copies to repeat rows")));
                    }
                    let node =
                        canvas.nodes().get(id).ok_or_else(|| Error::new(ErrorKind::NotFound, format!("`{name}`'s reference {id} isn't on this sheet")))?;
                    if generation::is_generation(&node.kind) {
                        return Err(bad(format!("`{name}` lists a row; image values are image sources on the sheet")));
                    }
                    match node.outputs.as_slice() {
                        [output] if output.port_type.modality == Modality::Image => {}
                        _ => return Err(bad(format!("`{name}`'s reference {id} has no single image output"))),
                    }
                    nodes.push(node);
                }
                Values::Images(nodes)
            }
            _ => return Err(bad(format!("give `{name}` either `text` values or `images`, not both or neither"))),
        };
        out.push(Variable { name, values });
    }
    let used = template::names(pieces);
    for name in &used {
        match out.iter().find(|variable| variable.name == *name) {
            None => return Err(bad(format!("the template uses `{{{{{name}}}}}`, which no variable defines"))),
            Some(Variable { values: Values::Images(_), .. }) => {
                return Err(bad(format!("`{name}` is an image variable: it is wired into each row as a reference, not written in the prompt")));
            }
            Some(_) => {}
        }
    }
    if let Some(unused) = out.iter().find(|variable| matches!(variable.values, Values::Text(_)) && !used.contains(&variable.name)) {
        return Err(bad(format!("the template doesn't use `{{{{{}}}}}`; write it into the template or remove the variable", unused.name)));
    }
    Ok(out)
}

fn check_count(name: &str, count: usize) -> Result<(), Error> {
    if count == 0 || count > MAX_VALUES {
        return Err(bad(format!("`{name}` needs 1 to {MAX_VALUES} values")));
    }
    Ok(())
}

/// Each variable's value index for combination `index`: the first variable varies slowest.
fn combination(variables: &[Variable<'_>], mut index: usize) -> Vec<usize> {
    let mut picks = vec![0; variables.len()];
    for (pick, variable) in picks.iter_mut().zip(variables).rev() {
        let len = variable.len().max(1);
        *pick = index % len;
        index /= len;
    }
    picks
}

/// The prompt and provenance values of combination `picks`.
fn fill(pieces: &[Piece], variables: &[Variable<'_>], picks: &[usize]) -> (String, serde_json::Map<String, Json>) {
    let mut text = BTreeMap::new();
    let mut provenance = serde_json::Map::new();
    for (variable, pick) in variables.iter().zip(picks) {
        match &variable.values {
            Values::Text(values) => {
                let value = values.get(*pick).map_or("", String::as_str);
                text.insert(variable.name, value);
                provenance.insert(variable.name.to_owned(), json!(value));
            }
            Values::Images(nodes) => {
                provenance.insert(variable.name.to_owned(), json!({"node": nodes.get(*pick).map(|node| node.id)}));
            }
        }
    }
    (template::fill(pieces, &text), provenance)
}

fn expand(c: &Context<'_>, value: &Json) -> Result<Effect, Error> {
    let p: ExpandParams = params(value)?;
    let canvas = sheet(c, p.canvas)?;
    let pieces = template::parse(&p.template)?;
    let variables = variables(&p.variables, &pieces, canvas)?;
    let copies = p.copies.unwrap_or(1);
    if copies == 0 || copies > MAX_COPIES {
        return Err(bad(format!("make 1 to {MAX_COPIES} copies of each combination")));
    }
    let combinations = variables.iter().try_fold(1u64, |total, variable| total.checked_mul(variable.len() as u64));
    let count = combinations.and_then(|combinations| combinations.checked_mul(copies));
    let order = rows(canvas);
    let room = MAX_ROWS.saturating_sub(order.len());
    let factors: Vec<String> =
        variables.iter().map(|variable| format!("{} {}", variable.len(), variable.name)).chain((copies > 1).then(|| format!("{copies} copies"))).collect();
    let factors = if factors.is_empty() { "1 row".to_owned() } else { factors.join(" × ") };
    let problem = match count {
        None => Some(format!("{factors} is far more than {MAX_ADD} rows; remove values or lower copies")),
        Some(count) if count > MAX_ADD as u64 => {
            Some(format!("{factors} makes {count} rows, and one expansion adds at most {MAX_ADD}; remove values or lower copies"))
        }
        Some(count) if count > room as u64 => {
            Some(format!("{factors} makes {count} rows, but this sheet has room for {room} more (it holds at most {MAX_ROWS})"))
        }
        Some(_) => None,
    };
    let base = base(c, canvas, &order, p.after, p.kind.as_deref(), p.seed)?;
    let copies_rows = usize::try_from(copies).unwrap_or(1).max(1);
    let result = |prompts: Vec<String>, problem: Option<String>| {
        json!({"count": count, "combinations": combinations, "copies": copies, "limit": MAX_ADD.min(room), "room": room,
            "fits": problem.is_none(), "problem": problem, "factors": factors, "prompts": prompts})
    };
    if let Some(problem) = problem {
        if !p.dry_run {
            return Err(Error::new(ErrorKind::Overflow, problem));
        }
        // Too many to make, but the first few still show what the template says.
        let combos = combinations.and_then(|combinations| usize::try_from(combinations).ok()).unwrap_or(usize::MAX);
        let prompts =
            (0..PREVIEW).take_while(|n| n / copies_rows < combos).map(|n| fill(&pieces, &variables, &combination(&variables, n / copies_rows)).0).collect();
        return Ok(Effect::None { result: result(prompts, Some(problem)) });
    }
    // At most MAX_ADD rows: there is no problem.
    let count_rows = count.and_then(|count| usize::try_from(count).ok()).unwrap_or(0);
    let shared_from: BTreeSet<NodeId> = base.wires.iter().map(|wire| wire.from.node).collect();
    if let Some(variable) =
        variables.iter().find(|variable| matches!(&variable.values, Values::Images(nodes) if nodes.iter().any(|node| shared_from.contains(&node.id))))
    {
        return Err(bad(format!("a value of `{}` is already a shared reference of every row; unwire it first or leave it out", variable.name)));
    }
    let mut changes = Vec::new();
    let mut made = Vec::with_capacity(count_rows);
    let mut prompts = Vec::new();
    let mut wire = 0usize;
    for n in 0..count_rows {
        let picks = combination(&variables, n / copies_rows);
        let (prompt, values) = fill(&pieces, &variables, &picks);
        if prompts.len() < PREVIEW {
            prompts.push(prompt.clone());
        }
        let id = NodeId::from_bytes(derive(p.seed, "row", n));
        let mut node = blank_copy(&base.node, id);
        node.pos = row_pos(base.insert.saturating_add(n));
        node.params.insert(generation::PROMPT.into(), Value::Text(prompt));
        let mut provenance = json!({"expansion": p.seed, "template": p.template, "values": values});
        if copies > 1 {
            provenance["copy"] = json!((n % copies_rows).saturating_add(1));
        }
        node.config.insert(PROVENANCE_CONFIG.into(), Value::Text(provenance.to_string()));
        // The shared references first, like the row it copies, then one per image variable.
        let mut wires: Vec<Edge> = base
            .wires
            .iter()
            .map(|shared| Edge { id: UNSET, from: shared.from.clone(), to: Endpoint { node: id, port: shared.to.port.clone() }, order: shared.order })
            .collect();
        for (variable, pick) in variables.iter().zip(&picks) {
            let Values::Images(nodes) = &variable.values else { continue };
            let Some(reference) = nodes.get(*pick) else { continue };
            let Some(output) = reference.outputs.first() else { continue };
            let port = free_input(&node, |port| wires.iter().any(|edge| edge.to.port == port), output)
                .ok_or_else(|| Error::new(ErrorKind::Unsupported, format!("{} for `{}`", no_free_input(&node, output).message, variable.name)))?;
            let order = wires.iter().filter(|edge| edge.to.port == port.name).map(|edge| edge.order).max().map_or(0, |last| last.saturating_add(1));
            wires.push(Edge {
                id: UNSET,
                from: Endpoint { node: reference.id, port: output.name.clone() },
                to: Endpoint { node: id, port: port.name.clone() },
                order,
            });
        }
        changes.push(Change::AddNode { canvas: p.canvas, node });
        for mut edge in wires {
            edge.id = EdgeId::from_bytes(derive(p.seed, "wire", wire));
            wire = wire.saturating_add(1);
            changes.push(Change::Connect { canvas: p.canvas, edge });
        }
        made.push(id);
    }
    changes.extend(reflow(p.canvas, canvas, &slots_with(&order, base.insert, &made)));
    // Built like a real expansion, so `fits` in a dry run means the expansion works.
    let taken = changes.iter().any(|change| match change {
        Change::AddNode { node, .. } => canvas.nodes().contains_key(&node.id),
        Change::Connect { edge, .. } => canvas.edges().contains_key(&edge.id),
        _ => false,
    });
    if taken {
        return Err(Error::new(ErrorKind::Conflict, "that seed was used before; use a fresh one"));
    }
    if p.dry_run {
        return Ok(Effect::None { result: result(prompts, None) });
    }
    edit(c, changes, json!({"rows": made, "count": made.len()}))
}

/// A wire's id before it is given its derived one.
const UNSET: EdgeId = EdgeId::from_bytes([0; 16]);

fn variable_schema() -> Json {
    json!({
        "type": "object", "additionalProperties": false, "required": ["name"],
        "properties": {
            "name": {"type": "string", "pattern": "^[A-Za-z0-9_-]{1,32}$"},
            "text": {"type": "array", "items": {"type": "string", "maxLength": MAX_VALUE_BYTES}, "minItems": 1, "maxItems": MAX_VALUES},
            "images": {"type": "array", "items": uuid(), "minItems": 1, "maxItems": MAX_VALUES}
        }
    })
}

pub(super) fn spec() -> CommandSpec {
    spec!(
        "sheet.expand",
        "Expand a prompt template into rows",
        schema(
            &["canvas", "seed", "template"],
            json!({"canvas": uuid(), "seed": uuid(), "after": uuid(), "kind": {"enum": generation::KINDS},
                "template": {"type": "string", "maxLength": template::MAX_TEMPLATE_BYTES},
                "variables": {"type": "array", "items": variable_schema(), "maxItems": MAX_VARIABLES},
                "copies": {"type": "integer", "minimum": 1, "maximum": MAX_COPIES},
                "dry_run": {"type": "boolean"}})
        ),
        "{canvas, seed: new UUID, template, variables?: [{name, text?: [value], images?: [NodeId]}], copies? (1..100, default 1), after?, kind?, dry_run?} -> {rows, count}; one row per combination of the variables' values (first variable slowest), each repeated `copies` times, after `after` (the last row) and copying it like sheet.rows_add. `{{name}}` in the template is a text variable's value; `{{{{` and `}}}}` are literal braces. An image variable wires one of its image source nodes into each row instead. With no variables, `copies` rows of the template. One expansion adds at most 100 rows and the sheet holds 1000. dry_run: {count, combinations, copies, limit, room, fits, problem, factors, prompts: first 5} without editing. Each row records its values under config `nodecraft.sheet_expansion`. Rows are not run: quote and run them with run.quote/run.start.",
        expand
    )
}

#[cfg(test)]
#[path = "expand_tests.rs"]
mod tests;
