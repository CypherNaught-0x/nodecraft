//! The batch (#92): a list of items that fans each one through the nodes downstream of it.
//!
//! Like a router, a batch never runs: the run planner resolves it into its items, and every
//! downstream node with a single input fed by it runs once per item. Its items are, in order,
//! the values wired into its `in` input (results of other nodes, uploads in source nodes), then,
//! for a text batch, its own `text` split per `split`. Two batches reaching one node pair their
//! items per `pairing`: Cross (every combination, the default) or Zip (item by item, sizes must
//! match). A list input fed by a batch collects every item instead of fanning.
//!
//! A batch carries one modality, chosen per instance in `config["modality"]` (`text` when
//! absent), as routers do.
use crate::{Category, JobSpec, ModeId, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Choice, Error, ErrorKind, Modality, ParamControl, ParamSchema, ParamSpec, PortType, Value, VisibleIf};
use std::borrow::Cow;

/// The batch's kind id.
pub const ID: &str = "structure.batch";
/// Most items one batch holds. Fan-out is capped again where batches combine.
pub const MAX_ITEMS: usize = 1000;
/// Longest custom separator, in bytes.
pub const MAX_SEPARATOR_BYTES: usize = 64;
/// Most bytes of text a batch splits.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// How a text batch splits its `text` into items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    /// The whole text is one item.
    None,
    /// One item per line.
    Line,
    /// One item per piece between occurrences of `separator`.
    Separator,
    /// One item per CSV row: the first column, after a header row.
    Csv,
}

/// How two batches reaching one node pair their items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pairing {
    /// Every combination: N × M runs.
    Cross,
    /// Item by item: N runs, and both batches must have N items.
    Zip,
}

fn choice(value: &str, label: &str) -> Choice {
    Choice { value: value.into(), label: label.into() }
}

fn when(parameter: &str, equals: &str) -> Option<VisibleIf> {
    Some(VisibleIf { parameter: parameter.into(), equals: Value::Text(equals.into()) })
}

fn params(modality: Modality) -> ParamSchema {
    let mut params = Vec::new();
    if modality == Modality::Text {
        params.push(ParamSpec {
            name: "text".into(),
            label: "Items".into(),
            description: Some("Text to split into items, after any wired inputs".into()),
            control: ParamControl::Text { multiline: true, max_bytes: MAX_TEXT_BYTES },
            required: false,
            default: Some(Value::Text(String::new())),
            visible_if: None,
        });
        params.push(ParamSpec {
            name: "split".into(),
            label: "Split".into(),
            description: None,
            control: ParamControl::Choice {
                options: vec![
                    choice("line", "One per line"),
                    choice("separator", "Custom separator"),
                    choice("csv", "CSV first column"),
                    choice("none", "Whole text"),
                ],
            },
            required: true,
            default: Some(Value::Text("line".into())),
            visible_if: None,
        });
        params.push(ParamSpec {
            name: "separator".into(),
            label: "Separator".into(),
            description: None,
            control: ParamControl::Text { multiline: false, max_bytes: MAX_SEPARATOR_BYTES },
            required: false,
            default: Some(Value::Text(",".into())),
            visible_if: when("split", "separator"),
        });
        params.push(ParamSpec {
            name: "header".into(),
            label: "First row is a header".into(),
            description: None,
            control: ParamControl::Toggle,
            required: false,
            default: Some(Value::Bool(true)),
            visible_if: when("split", "csv"),
        });
    }
    params.push(ParamSpec {
        name: "pairing".into(),
        label: "Pairing".into(),
        description: Some("How this batch pairs with another batch reaching the same node".into()),
        control: ParamControl::Choice { options: vec![choice("cross", "Cross: every combination"), choice("zip", "Zip: item by item")] },
        required: true,
        default: Some(Value::Text("cross".into())),
        visible_if: None,
    });
    ParamSchema { params }
}

fn descriptor(modality: Modality) -> NodeDescriptor {
    NodeDescriptor {
        id: ID.into(),
        label: "Batch".into(),
        category: Category::Structure,
        inputs: vec![Port { name: "in".into(), port_type: PortType { modality, many: true } }],
        outputs: vec![Port { name: "item".into(), port_type: PortType { modality, many: false } }],
        params: params(modality),
        quick_add: Some("B".into()),
    }
}

/// The modality a batch carries (the router's config key and names).
pub fn modality(node: &Node) -> Result<Modality, Error> {
    crate::router::modality(node).map_err(|_| invalid("a batch's modality must be text, image, video, audio, mesh, document or style"))
}

/// The `config` that makes a batch carry `modality`.
pub fn config(modality: Modality) -> std::collections::BTreeMap<String, Value> {
    crate::router::config(modality)
}

fn text_param<'a>(node: &'a Node, name: &str, default: &'a str) -> Result<&'a str, Error> {
    match node.params.get(name) {
        None => Ok(default),
        Some(Value::Text(text)) => Ok(text),
        Some(_) => Err(Error::new(ErrorKind::InvalidInput, format!("the batch's {name} must be text"))),
    }
}

/// How `node` pairs with other batches.
pub fn pairing(node: &Node) -> Result<Pairing, Error> {
    match text_param(node, "pairing", "cross")? {
        "cross" => Ok(Pairing::Cross),
        "zip" => Ok(Pairing::Zip),
        _ => Err(invalid("a batch's pairing must be cross or zip")),
    }
}

fn split_mode(node: &Node) -> Result<Split, Error> {
    match text_param(node, "split", "line")? {
        "none" => Ok(Split::None),
        "line" => Ok(Split::Line),
        "separator" => Ok(Split::Separator),
        "csv" => Ok(Split::Csv),
        _ => Err(invalid("a batch's split must be none, line, separator or csv")),
    }
}

fn too_many() -> Error {
    Error::new(ErrorKind::InvalidInput, format!("a batch holds at most {MAX_ITEMS} items"))
}

fn push(items: &mut Vec<Value>, value: Value) -> Result<(), Error> {
    if items.len() >= MAX_ITEMS {
        return Err(too_many());
    }
    items.push(value);
    Ok(())
}

/// Pieces are trimmed; empty and whitespace-only pieces are skipped (they are spacing, not
/// items).
fn push_piece(items: &mut Vec<Value>, piece: &str) -> Result<(), Error> {
    let piece = piece.trim();
    if piece.is_empty() { Ok(()) } else { push(items, Value::Text(piece.to_owned())) }
}

/// The items of `node`, in order: `wired` (the values on its `in` input, lists already spliced),
/// then its own text split per its settings. Bounded by [`MAX_ITEMS`]; never drops a CSV row
/// silently.
pub fn items(node: &Node, wired: impl IntoIterator<Item = Value>) -> Result<Vec<Value>, Error> {
    let mut items = Vec::new();
    for value in wired {
        push(&mut items, value)?;
    }
    if modality(node)? != Modality::Text {
        return Ok(items);
    }
    let text = text_param(node, "text", "")?;
    if text.len() > MAX_TEXT_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, "the batch's text is too long"));
    }
    match split_mode(node)? {
        Split::None => push_piece(&mut items, text)?,
        Split::Line => {
            for line in text.lines() {
                push_piece(&mut items, line)?;
            }
        }
        Split::Separator => {
            let separator = text_param(node, "separator", ",")?;
            if separator.is_empty() || separator.len() > MAX_SEPARATOR_BYTES {
                return Err(Error::new(ErrorKind::InvalidInput, format!("the batch's separator must be 1 to {MAX_SEPARATOR_BYTES} bytes")));
            }
            for piece in text.split(separator) {
                push_piece(&mut items, piece)?;
            }
        }
        Split::Csv => {
            let header = !matches!(node.params.get("header"), Some(Value::Bool(false)));
            for cell in csv_first_column(text, header)? {
                push(&mut items, Value::Text(cell))?;
            }
        }
    }
    Ok(items)
}

/// The first column of each CSV record (RFC 4180 quoting: `"…"` with `""` for a quote; comma
/// separated; CRLF or LF rows). A leading byte-order mark is ignored and blank lines between
/// records are skipped. A record whose first cell is empty, or an unterminated quote, is an error
/// naming its row, so no row is dropped silently.
pub fn csv_first_column(text: &str, header: bool) -> Result<Vec<String>, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut cells = Vec::new();
    let mut chars = text.chars().peekable();
    let mut row = 0usize;
    loop {
        // Skip blank lines between records.
        while matches!(chars.peek(), Some('\n' | '\r')) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        row = row.saturating_add(1);
        let mut cell = String::new();
        let mut quoted = false;
        if chars.peek() == Some(&'"') {
            chars.next();
            quoted = true;
            loop {
                match chars.next() {
                    None => return Err(Error::new(ErrorKind::InvalidInput, format!("CSV row {row} has an unterminated quote"))),
                    Some('"') if chars.peek() == Some(&'"') => {
                        chars.next();
                        cell.push('"');
                    }
                    Some('"') => break,
                    Some(ch) => cell.push(ch),
                }
            }
        }
        // The rest of the first cell, then skip the rest of the record. Quotes later in the
        // record are honoured so a quoted newline doesn't end it.
        let mut first = true;
        let mut in_quote = false;
        while let Some(&ch) = chars.peek() {
            if !in_quote && (ch == '\n' || ch == '\r') {
                break;
            }
            chars.next();
            match ch {
                '"' => in_quote = !in_quote,
                ',' if !in_quote => first = false,
                _ if first && !quoted => cell.push(ch),
                _ if first && !ch.is_whitespace() => {
                    return Err(Error::new(ErrorKind::InvalidInput, format!("CSV row {row} has text after a closing quote")));
                }
                _ => {}
            }
        }
        if in_quote {
            return Err(Error::new(ErrorKind::InvalidInput, format!("CSV row {row} has an unterminated quote")));
        }
        if header && row == 1 {
            continue;
        }
        let cell = if quoted { cell } else { cell.trim().to_owned() };
        if cell.is_empty() {
            return Err(Error::new(ErrorKind::InvalidInput, format!("CSV row {row} has an empty first column")));
        }
        if cells.len() >= MAX_ITEMS {
            return Err(too_many());
        }
        cells.push(cell);
    }
    Ok(cells)
}

pub struct BatchKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> BatchKind {
    BatchKind { descriptor: descriptor(Modality::Text) }
}

impl NodeKind for BatchKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        match modality(node)? {
            Modality::Text => Ok(Cow::Borrowed(&self.descriptor)),
            other => Ok(Cow::Owned(descriptor(other))),
        }
    }
    /// A batch takes anything of its modality.
    fn mode(&self, _: &[PortType]) -> Option<ModeId> {
        ModeId::new("batch").ok()
    }
    fn plan(&self, _: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        Err(invalid("batches are resolved at plan time and never run"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_types::NodeId;

    fn node(params: &[(&str, Value)]) -> Node {
        let mut node = kind().instantiate_instance(NodeId::from_bytes([1; 16]), Default::default(), &Default::default()).unwrap();
        for (name, value) in params {
            node.params.insert((*name).into(), value.clone());
        }
        node
    }

    fn text(value: &str) -> Value {
        Value::Text(value.into())
    }

    fn texts(items: Vec<Value>) -> Vec<String> {
        items
            .into_iter()
            .map(|v| match v {
                Value::Text(t) => t,
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn defaults_split_by_line_and_cross() {
        let n = node(&[("text", text("a cat\n\n   \r\n a dog \r\nan owl"))]);
        kind().validate_instance(&n, true).unwrap();
        assert_eq!(texts(items(&n, []).unwrap()), ["a cat", "a dog", "an owl"]);
        assert_eq!(pairing(&n).unwrap(), Pairing::Cross);
    }

    #[test]
    fn wired_items_come_first_and_modes_split() {
        let n = node(&[("text", text("x; y ;; z")), ("split", text("separator")), ("separator", text(";"))]);
        assert_eq!(texts(items(&n, [text("w")]).unwrap()), ["w", "x", "y", "z"]);
        let whole = node(&[("text", text("  one\ntwo  ")), ("split", text("none"))]);
        assert_eq!(texts(items(&whole, []).unwrap()), ["one\ntwo"]);
        let blank = node(&[("text", text(" \n ")), ("split", text("none"))]);
        assert!(items(&blank, []).unwrap().is_empty());
        let empty_separator = node(&[("text", text("a")), ("split", text("separator")), ("separator", text(""))]);
        assert!(items(&empty_separator, []).is_err());
        assert!(pairing(&node(&[("pairing", text("diagonal"))])).is_err());
        assert!(items(&node(&[("split", text("words"))]), []).is_err());
    }

    #[test]
    fn csv_takes_the_first_column_and_reports_bad_rows() {
        let csv = "\u{feff}prompt,seed\r\n\"a, quoted \"\"cat\"\"\",1\r\n\r\nplain dog , 2\n\"multi\nline\",3\n";
        assert_eq!(csv_first_column(csv, true).unwrap(), ["a, quoted \"cat\"", "plain dog", "multi\nline"]);
        assert_eq!(csv_first_column("a\nb", false).unwrap(), ["a", "b"]);
        assert!(csv_first_column("h\n\"open", true).unwrap_err().message.contains("row 2"));
        assert!(csv_first_column("h\n,1\nb", true).unwrap_err().message.contains("row 2 has an empty first column"));
        assert!(csv_first_column("h\n\"a\"b,1", true).is_err());
        assert!(csv_first_column("h\na,\"unterminated", true).is_err());
        let n = node(&[("text", text("h\nx\ny")), ("split", text("csv"))]);
        assert_eq!(texts(items(&n, []).unwrap()), ["x", "y"]);
        let raw = node(&[("text", text("x\ny")), ("split", text("csv")), ("header", Value::Bool(false))]);
        assert_eq!(texts(items(&raw, []).unwrap()), ["x", "y"]);
    }

    #[test]
    fn items_are_capped() {
        let many = "x\n".repeat(MAX_ITEMS + 1);
        assert!(items(&node(&[("text", text(&many))]), []).is_err());
        assert!(csv_first_column(&many, false).is_err());
        assert!(items(&node(&[]), std::iter::repeat_n(text("w"), MAX_ITEMS + 1)).is_err());
    }

    #[test]
    fn other_modalities_take_only_wired_items() {
        let image = kind().instantiate_instance(NodeId::from_bytes([2; 16]), config(Modality::Image), &Default::default()).unwrap();
        kind().validate_instance(&image, true).unwrap();
        assert_eq!(image.outputs.first().map(|p| p.port_type), Some(PortType { modality: Modality::Image, many: false }));
        assert!(!image.params.contains_key("text"));
        assert_eq!(items(&image, [Value::Bool(true)]).unwrap().len(), 1);
        assert!(kind().plan(&PlanCtx { targets: &nodecraft_types::EMPTY_TARGET_SELECTION, node: &image, inputs: &[] }).is_err());
    }
}
