//! The folder: the files of a folder on disk, imported into the project, as one card.
//!
//! A folder holds its files in `config`: their names (paths inside the folder, `/`-separated) in
//! `config["names"]` and their values in `config["files"]`, in import order. Media go into the
//! project's asset store when the folder is imported, so a project keeps working when the folder
//! moves or changes; importing again refreshes it. Like a batch, a folder carries one modality,
//! chosen per instance in `config["modality"]` (`image` when absent).
//!
//! Its parameters choose which files it gives and in what order: name filters (`*` and `?`
//! wildcards), a sort (natural, alphabetical, import order or a seeded shuffle), reverse, and a
//! range (skip the first N, take every Nth, at most N). It gives them four ways:
//!
//! - `file` and `name` loop over the files: like a batch's item, every node downstream with a
//!   single input fed by them runs once per file. Both share the folder's items, so a node fed by
//!   a file and its name runs once per file, not once per pair.
//! - `files` and `names` give every file, or every name, at once as one list.
use crate::{Category, JobSpec, ModeContract, ModeId, NodeContract, NodeDescriptor, NodeKind, PlanCtx, descriptor::invalid};
use nodecraft_graph::{Node, Port};
use nodecraft_types::{Affects, Choice, Error, ErrorKind, Modality, ParamControl, ParamRule, ParamSchema, ParamSpec, PortType, Unsigned, Value, VisibleIf};
use std::{borrow::Cow, cmp::Ordering, collections::BTreeMap};

/// The folder's kind id.
pub const ID: &str = "source.folder";
/// Most files one folder holds: a batch's limit, since its files fan out like a batch's items.
pub const MAX_FILES: usize = crate::batch::MAX_ITEMS;
/// Longest name a folder keeps for one file, in bytes.
pub const MAX_NAME_BYTES: usize = 1024;
/// Longest name filter, in bytes.
pub const MAX_PATTERN_BYTES: usize = 256;
/// The config key holding the files' names.
pub const NAMES: &str = "names";
/// The config key holding the files' values.
pub const FILES: &str = "files";
/// The config key holding the folder's own name, for its card.
pub const FOLDER: &str = "folder";
/// The output that loops over the files.
pub const FILE: &str = "file";
/// The output that loops over the files' names.
pub const NAME: &str = "name";
/// The output giving every file as one list.
pub const ALL_FILES: &str = "files";
/// The output giving every name as one list.
pub const ALL_NAMES: &str = "names";

/// The modalities a folder can hold: everything a source node can.
pub const MODALITIES: [Modality; 6] = [Modality::Image, Modality::Video, Modality::Audio, Modality::Text, Modality::Mesh, Modality::Document];

/// Whether `port` is one of the outputs that loop over the files.
pub fn loops(port: &str) -> bool {
    port == FILE || port == NAME
}

/// Whether `node`'s output `port` loops over its files: a folder's `file` or `name`.
pub fn loops_over(node: &Node, port: &str) -> bool {
    node.kind == ID && loops(port)
}

fn choice(value: &str, label: &str) -> Choice {
    Choice { value: value.into(), label: label.into() }
}

fn when(parameter: &str, equals: &str) -> Option<VisibleIf> {
    Some(VisibleIf { parameter: parameter.into(), equals: Value::Text(equals.into()) })
}

fn count(name: &str, label: &str, description: &str, min: u64, default: u64) -> ParamSpec {
    ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some(description.into()),
        control: ParamControl::Unsigned { min: Some(Unsigned(min)), max: Some(Unsigned(MAX_FILES as u64)), step: Unsigned(1) },
        required: true,
        default: Some(Value::Unsigned(Unsigned(default))),
        visible_if: None,
    }
}

fn params() -> ParamSchema {
    let filter = |name: &str, label: &str, description: &str| ParamSpec {
        name: name.into(),
        label: label.into(),
        description: Some(description.into()),
        control: ParamControl::Text { multiline: false, max_bytes: MAX_PATTERN_BYTES },
        required: false,
        default: Some(Value::Text(String::new())),
        visible_if: None,
    };
    ParamSchema {
        params: vec![
            filter("include", "Only names like", "Wildcards * and ?, several separated by ; (for example *.png; *.jpg). Empty keeps every file"),
            filter("exclude", "Skip names like", "Wildcards * and ?, several separated by ;. Empty skips none"),
            ParamSpec {
                name: "sort".into(),
                label: "Order".into(),
                description: None,
                control: ParamControl::Choice {
                    options: vec![
                        choice("natural", "Name (2 before 10)"),
                        choice("name", "Name (alphabetical)"),
                        choice("imported", "As imported"),
                        choice("shuffle", "Shuffled"),
                    ],
                },
                required: true,
                default: Some(Value::Text("natural".into())),
                visible_if: None,
            },
            ParamSpec {
                name: "seed".into(),
                label: "Shuffle seed".into(),
                description: Some("The same seed always gives the same order".into()),
                control: ParamControl::Seed { max: None },
                required: true,
                default: Some(Value::Unsigned(Unsigned(0))),
                visible_if: when("sort", "shuffle"),
            },
            ParamSpec {
                name: "reverse".into(),
                label: "Reverse order".into(),
                description: None,
                control: ParamControl::Toggle,
                required: false,
                default: Some(Value::Bool(false)),
                visible_if: None,
            },
            count("skip", "Skip first", "Leave out this many files at the start", 0, 0),
            count("every", "Take every", "1 takes every file, 2 every other file, and so on", 1, 1),
            count("limit", "At most", "Most files to give; 0 gives them all", 0, 0),
            ParamSpec {
                name: "name_format".into(),
                label: "Names".into(),
                description: Some("What the name outputs give for each file".into()),
                control: ParamControl::Choice {
                    options: vec![choice("file", "File name"), choice("stem", "File name without extension"), choice("path", "Path inside the folder")],
                },
                required: true,
                default: Some(Value::Text("file".into())),
                visible_if: None,
            },
            ParamSpec {
                name: "pairing".into(),
                label: "Pairing".into(),
                description: Some("How the looping outputs pair with a batch or another folder reaching the same node".into()),
                control: ParamControl::Choice { options: vec![choice("cross", "Cross: every combination"), choice("zip", "Zip: item by item")] },
                required: true,
                default: Some(Value::Text("cross".into())),
                visible_if: None,
            },
        ],
    }
}

fn descriptor(modality: Modality) -> NodeDescriptor {
    let port = |name: &str, modality, many| Port { name: name.into(), port_type: PortType { modality, many } };
    NodeDescriptor {
        id: ID.into(),
        label: "Folder".into(),
        category: Category::Source,
        inputs: Vec::new(),
        outputs: vec![port(FILE, modality, false), port(NAME, Modality::Text, false), port(ALL_FILES, modality, true), port(ALL_NAMES, Modality::Text, true)],
        params: params(),
        quick_add: None,
    }
}

/// The modality a folder holds.
pub fn modality(node: &Node) -> Result<Modality, Error> {
    if !node.config.contains_key(crate::router::MODALITY) {
        return Ok(Modality::Image);
    }
    let modality = crate::router::modality(node).map_err(|_| invalid("a folder's modality must be image, video, audio, text, mesh or document"))?;
    if MODALITIES.contains(&modality) { Ok(modality) } else { Err(invalid("a folder's modality must be image, video, audio, text, mesh or document")) }
}

/// The `config` of a folder named `folder` holding `files` (name and value, in import order) of
/// `modality`. Checked as a stored folder is.
pub fn config(modality: Modality, folder: &str, files: Vec<(String, Value)>) -> Result<BTreeMap<String, Value>, Error> {
    let mut config = crate::router::config(modality);
    config.insert(FOLDER.into(), Value::Text(folder.chars().take(MAX_NAME_BYTES / 4).collect()));
    let (names, values): (Vec<_>, Vec<_>) = files.into_iter().map(|(name, value)| (Value::Text(name), value)).unzip();
    config.insert(NAMES.into(), Value::List(names));
    config.insert(FILES.into(), Value::List(values));
    let mut probe = Node::new(nodecraft_types::NodeId::from_bytes([0; 16]), ID);
    probe.config = config;
    stored(&probe)?;
    Ok(probe.config)
}

/// The folder's own name, for its card; empty before anything is imported.
pub fn folder_name(node: &Node) -> &str {
    match node.config.get(FOLDER) {
        Some(Value::Text(name)) => name,
        _ => "",
    }
}

/// One file of a folder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry<'a> {
    /// Its path inside the folder, `/`-separated.
    pub name: &'a str,
    pub value: &'a Value,
}

impl Entry<'_> {
    /// The last part of its path.
    pub fn file_name(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(self.name)
    }
    /// Its file name without its extension (a name that is only an extension keeps it).
    pub fn stem(&self) -> &str {
        let file = self.file_name();
        match file.rfind('.') {
            Some(0) | None => file,
            Some(dot) => file.get(..dot).unwrap_or(file),
        }
    }
}

fn list<'a>(node: &'a Node, key: &str) -> Result<&'a [Value], Error> {
    match node.config.get(key) {
        None => Ok(&[]),
        Some(Value::List(values)) => Ok(values),
        Some(_) => Err(invalid("a folder's files must be a list")),
    }
}

/// Every file the folder holds, in import order, checked: names and values pair up, names are
/// short, unique and printable, and values are of the folder's modality.
pub fn stored(node: &Node) -> Result<Vec<Entry<'_>>, Error> {
    let modality = modality(node)?;
    let (names, values) = (list(node, NAMES)?, list(node, FILES)?);
    if names.len() != values.len() {
        return Err(invalid("a folder needs one name per file"));
    }
    if names.len() > MAX_FILES {
        return Err(Error::new(ErrorKind::InvalidInput, format!("a folder holds at most {MAX_FILES} files")));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut entries = Vec::with_capacity(names.len());
    for (name, value) in names.iter().zip(values) {
        let Value::Text(name) = name else { return Err(invalid("a folder's file names must be text")) };
        if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) || !seen.insert(name.as_str()) {
            return Err(invalid("a folder's file names must be unique, printable and at most 1 KiB"));
        }
        let fits = match (modality, value) {
            (Modality::Text, Value::Text(_)) => true,
            (Modality::Text, _) | (_, Value::Text(_)) => false,
            (_, Value::Media(asset)) => modality.accepts_mime(asset.mime.essence()),
            _ => false,
        };
        if !fits {
            return Err(invalid("a folder's files must all be of its modality"));
        }
        entries.push(Entry { name, value });
    }
    Ok(entries)
}

fn text<'a>(node: &'a Node, name: &str, default: &'a str) -> Result<&'a str, Error> {
    match node.params.get(name) {
        None => Ok(default),
        Some(Value::Text(text)) => Ok(text),
        Some(_) => Err(Error::new(ErrorKind::InvalidInput, format!("the folder's {name} must be text"))),
    }
}

fn number(node: &Node, name: &str, default: u64) -> Result<u64, Error> {
    match node.params.get(name) {
        None => Ok(default),
        Some(Value::Unsigned(Unsigned(n))) => Ok(*n),
        Some(_) => Err(Error::new(ErrorKind::InvalidInput, format!("the folder's {name} must be a whole number"))),
    }
}

/// How the folder pairs with batches and other folders.
pub fn pairing(node: &Node) -> Result<crate::batch::Pairing, Error> {
    match text(node, "pairing", "cross")? {
        "cross" => Ok(crate::batch::Pairing::Cross),
        "zip" => Ok(crate::batch::Pairing::Zip),
        _ => Err(invalid("a folder's pairing must be cross or zip")),
    }
}

/// Most matching steps one pass over a folder's files may take: generous for real filters, and
/// a bound on what a hostile project's filters can cost.
pub const MAX_MATCH_STEPS: u64 = 4_000_000;

fn lower(text: &str) -> Vec<char> {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Whether `name` matches `pattern` (both lowercased): `*` is any run of characters, `?` any
/// one. Backtracks only to the last `*`; every step is charged to `budget`, `None` once spent.
fn matches(pattern: &[char], name: &[char], budget: &mut u64) -> Option<bool> {
    let (mut p, mut n) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        *budget = budget.checked_sub(1)?;
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, n));
                p += 1;
            }
            Some(c) if *c == '?' || Some(c) == name.get(n) => {
                p += 1;
                n += 1;
            }
            _ => match star {
                Some((at, from)) => {
                    p = at + 1;
                    n = from + 1;
                    star = Some((at, from + 1));
                }
                None => return Some(false),
            },
        }
    }
    Some(pattern.get(p..).is_some_and(|rest| rest.iter().all(|c| *c == '*')))
}

/// Whether `name` matches `pattern`: `*` is any run of characters, `?` any one, case ignored.
pub fn wildcard(pattern: &str, name: &str) -> bool {
    let mut unbounded = u64::MAX;
    matches(&lower(pattern), &lower(name), &mut unbounded).unwrap_or(false)
}

/// `;`-separated patterns, lowercased, each with whether it matches the path (it has a `/`)
/// rather than the file name.
fn patterns(text: &str) -> Vec<(Vec<char>, bool)> {
    text.split(';').map(str::trim).filter(|p| !p.is_empty()).map(|p| (lower(p), p.contains('/'))).collect()
}

/// Whether any of `patterns` matches `path` or `file` (lowercased); `None` once `budget` is spent.
fn matches_any(patterns: &[(Vec<char>, bool)], path: &[char], file: &[char], budget: &mut u64) -> Option<bool> {
    for (pattern, whole) in patterns {
        if matches(pattern, if *whole { path } else { file }, budget)? {
            return Some(true);
        }
    }
    Some(false)
}

/// Natural order: runs of digits compare by value, so `img2` comes before `img10`; letters
/// compare without case.
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let digits = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut run = String::new();
                    while let Some(c) = chars.next_if(char::is_ascii_digit) {
                        run.push(c);
                    }
                    run
                };
                let (x, y) = (digits(&mut a), digits(&mut b));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                match x.len().cmp(&y.len()).then_with(|| x.cmp(y)) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            (Some(x), Some(y)) => {
                match x.to_lowercase().cmp(y.to_lowercase()) {
                    Ordering::Equal => {}
                    other => return other,
                }
                a.next();
                b.next();
            }
        }
    }
}

/// SplitMix64: a small, well-mixed generator, so a seed gives the same shuffle everywhere.
fn split_mix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn too_complex() -> Error {
    invalid("the folder's name filters take too long to match its files; use fewer * in them")
}

/// The files the folder gives, in order: those its filters keep, sorted, then its range.
pub fn selected(node: &Node) -> Result<Vec<Entry<'_>>, Error> {
    let mut entries = stored(node)?;
    let (include, exclude) = (text(node, "include", "")?, text(node, "exclude", "")?);
    if include.len() > MAX_PATTERN_BYTES || exclude.len() > MAX_PATTERN_BYTES {
        return Err(Error::new(ErrorKind::InvalidInput, format!("a folder's name filters are at most {MAX_PATTERN_BYTES} bytes")));
    }
    let (include, exclude) = (patterns(include), patterns(exclude));
    if !include.is_empty() || !exclude.is_empty() {
        let mut budget = MAX_MATCH_STEPS;
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries {
            // Each name is lowercased once, whatever the number of patterns.
            let (path, file) = (lower(entry.name), lower(entry.file_name()));
            let wanted = include.is_empty() || matches_any(&include, &path, &file, &mut budget).ok_or_else(too_complex)?;
            if wanted && !matches_any(&exclude, &path, &file, &mut budget).ok_or_else(too_complex)? {
                kept.push(entry);
            }
        }
        entries = kept;
    }
    match text(node, "sort", "natural")? {
        "natural" => entries.sort_by(|a, b| natural(a.name, b.name).then_with(|| a.name.cmp(b.name))),
        "name" => entries.sort_by_cached_key(|entry| (entry.name.to_lowercase(), entry.name)),
        "imported" => {}
        "shuffle" => {
            // Fisher–Yates from the files in natural order, so the same files and seed always
            // give the same order whatever order they were imported in.
            entries.sort_by(|a, b| natural(a.name, b.name).then_with(|| a.name.cmp(b.name)));
            let mut state = number(node, "seed", 0)?;
            for at in (1..entries.len()).rev() {
                let bound = u64::try_from(at).unwrap_or(u64::MAX).saturating_add(1);
                let other = usize::try_from(split_mix(&mut state) % bound).unwrap_or(0);
                entries.swap(at, other);
            }
        }
        _ => return Err(invalid("a folder's order must be natural, name, imported or shuffle")),
    }
    if matches!(node.params.get("reverse"), Some(Value::Bool(true))) {
        entries.reverse();
    }
    let skip = usize::try_from(number(node, "skip", 0)?).unwrap_or(usize::MAX);
    let every = usize::try_from(number(node, "every", 1)?).unwrap_or(usize::MAX).max(1);
    let limit = match number(node, "limit", 0)? {
        0 => usize::MAX,
        n => usize::try_from(n).unwrap_or(usize::MAX),
    };
    Ok(entries.into_iter().skip(skip).step_by(every).take(limit).collect())
}

/// What the name outputs give for `entry`.
pub fn display_name(node: &Node, entry: &Entry<'_>) -> Result<String, Error> {
    Ok(match text(node, "name_format", "file")? {
        "file" => entry.file_name(),
        "stem" => entry.stem(),
        "path" => entry.name,
        _ => return Err(invalid("a folder's names must be file, stem or path")),
    }
    .to_owned())
}

/// The values the folder gives on `port`, one per selected file. Empty when nothing is selected;
/// [`plan`](NodeKind::plan) refuses that with a reason.
pub fn values(node: &Node, port: &str) -> Result<Vec<Value>, Error> {
    let entries = selected(node)?;
    match port {
        FILE | ALL_FILES => Ok(entries.iter().map(|entry| entry.value.clone()).collect()),
        NAME | ALL_NAMES => entries.iter().map(|entry| display_name(node, entry).map(Value::Text)).collect(),
        _ => Err(invalid("a folder has no such output")),
    }
}

/// Why a folder gives nothing, when it doesn't.
fn empty(node: &Node) -> Error {
    if stored(node).is_ok_and(|files| files.is_empty()) {
        invalid("the folder has no files yet; drop a folder on it or import one")
    } else {
        invalid("no file in the folder matches its filters and range")
    }
}

/// The items the folder's looping outputs give on `port`: its selected files' values or names.
/// Nothing selected is an error, as an empty batch would run nothing.
pub fn items(node: &Node, port: &str) -> Result<Vec<Value>, Error> {
    let values = values(node, port)?;
    if values.is_empty() { Err(empty(node)) } else { Ok(values) }
}

pub struct FolderKind {
    descriptor: NodeDescriptor,
}

pub fn kind() -> FolderKind {
    FolderKind { descriptor: descriptor(Modality::Image) }
}

impl NodeKind for FolderKind {
    fn descriptor(&self) -> &NodeDescriptor {
        &self.descriptor
    }
    fn instance<'a>(&'a self, node: &Node) -> Result<Cow<'a, NodeDescriptor>, Error> {
        match modality(node)? {
            Modality::Image => Ok(Cow::Borrowed(&self.descriptor)),
            other => Ok(Cow::Owned(descriptor(other))),
        }
    }
    fn mode(&self, inputs: &[PortType]) -> Option<ModeId> {
        inputs.is_empty().then(ModeId::source)
    }
    /// As for every source: no inputs, one mode, and its settings shape what it gives.
    fn contract(&self, descriptor: &NodeDescriptor) -> NodeContract {
        let mut contract = NodeContract::derive(descriptor);
        for output in &mut contract.outputs {
            output.role = "value".into();
        }
        contract.modes = vec![ModeContract { mode: ModeId::source(), inputs: Vec::new(), params: None }];
        contract.params.rules = descriptor
            .params
            .params
            .iter()
            .map(|spec| ParamRule { affects: Affects { shape: true, ..Affects::default() }, ..ParamRule::implied(&spec.name) })
            .collect();
        contract
    }
    /// Run on its own, a folder gives its first file and name on the looping outputs and every
    /// file and name on the list outputs (one job per item, so the result is the list). Wired
    /// into other nodes, the planner reads [`items`] and [`values`] instead.
    fn plan(&self, ctx: &PlanCtx<'_>) -> Result<Vec<JobSpec>, Error> {
        if !ctx.inputs.is_empty() {
            return Err(invalid("source nodes do not accept connected inputs"));
        }
        self.validate_instance(ctx.node, true)?;
        let node = ctx.node;
        let entries = selected(node)?;
        let first = entries.first().ok_or_else(|| empty(node))?;
        let source = |output: &str, value: Value| JobSpec::Source { node: node.id, output: output.into(), value };
        let mut jobs = Vec::with_capacity(entries.len().saturating_mul(2).saturating_add(2));
        jobs.push(source(FILE, first.value.clone()));
        jobs.push(source(NAME, Value::Text(display_name(node, first)?)));
        for entry in &entries {
            jobs.push(source(ALL_FILES, entry.value.clone()));
        }
        for entry in &entries {
            jobs.push(source(ALL_NAMES, Value::Text(display_name(node, entry)?)));
        }
        Ok(jobs)
    }
}

#[cfg(test)]
mod tests;
