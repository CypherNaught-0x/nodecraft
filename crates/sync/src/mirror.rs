//! A project mirrored into a shared CRDT document. Every part is one map entry, a JSON string
//! under a key naming it, so two people changing different parts both keep their change, and
//! two changing one part end with the same one of them everywhere. A card is split further,
//! into its position, each of its parameters and the rest, so one person can move a card while
//! another edits its prompt.
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use nodecraft_graph::assemble::{self, Assembled, CanvasPart, Part, Parts};
use nodecraft_graph::{Canvas, CanvasKind, Comment, Edge, Field, Node, Position, Project, Skill, Tag};
use nodecraft_types::{CanvasId, Error, ErrorKind, ProjectId, Value};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Any, Doc, Map, MapRef, Out, ReadTxn, StateVector, Transact, TransactionMut, Update, WriteTxn};

const META: &str = "meta";
const CANVASES: &str = "canvases";
const NODES: &str = "nodes";
const POSITIONS: &str = "positions";
const PARAMS: &str = "params";
const EDGES: &str = "edges";
const FIELDS: &str = "fields";
const TAGS: &str = "tags";
const COMMENTS: &str = "comments";
const SKILLS: &str = "skills";
const ID: &str = "id";
const TITLE: &str = "title";

/// A canvas's own entry: an editable canvas's header, or a technique canvas whole.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CanvasEntry {
    Header { title: String, kind: CanvasKind },
    Frozen(Box<Canvas>),
}

/// One entry: which map, which key.
type Key = (&'static str, String);

/// What the shared document holds now, as a valid project.
#[derive(Debug)]
pub struct Materialized {
    pub project: Project,
    /// Parts the document held that the project couldn't (two wires into one single input, a
    /// cycle, a card on a canvas someone removed): taken out of the document too, so this
    /// update goes to the other peers.
    pub repairs: Option<Vec<u8>>,
    pub dropped: usize,
}

/// One peer's copy of the shared document, and the project it last agreed with.
pub struct Mirror {
    doc: Doc,
    shadow: Option<Project>,
    damaged: bool,
}

impl Mirror {
    /// Shares `project`: a new document holding all of it, and that document as one update.
    pub fn share(project: &Project) -> Result<(Self, Vec<u8>), Error> {
        let mut mirror = Self { doc: Doc::new(), shadow: None, damaged: false };
        let update = mirror.write(|txn| {
            let meta = txn.get_or_insert_map(META);
            meta.insert(txn, ID, Any::from(project.id().to_string()));
            let writes = changed(&empty(project.id())?, project)?;
            apply(txn, writes);
            Ok(())
        })?;
        mirror.shadow = Some(project.clone());
        Ok((mirror, update))
    }

    /// Joins a shared document: empty until the first update arrives.
    pub fn join() -> Self {
        Self { doc: Doc::new(), shadow: None, damaged: false }
    }

    /// The shared project's id, once the update that names it has arrived.
    pub fn project_id(&self) -> Result<Option<ProjectId>, Error> {
        self.check()?;
        let txn = self.read()?;
        let mut bad = Vec::new();
        Ok(entries(&txn, META, &mut bad).into_iter().find(|(key, _)| key == ID).and_then(|(_, id)| id.parse().ok()))
    }

    /// What this copy holds, for the other side to send only what it lacks.
    pub fn state_vector(&self) -> Result<Vec<u8>, Error> {
        self.check()?;
        Ok(self.read()?.state_vector().encode_v1())
    }

    /// What this copy holds that one at `state_vector` doesn't, as one update.
    pub fn update_for(&self, state_vector: &[u8]) -> Result<Vec<u8>, Error> {
        self.check()?;
        let sv = StateVector::decode_v1(state_vector).map_err(|_| invalid("the state vector isn't readable"))?;
        Ok(self.read()?.encode_diff_v1(&sv))
    }

    /// Merges an update from another peer.
    pub fn receive(&mut self, update: &[u8]) -> Result<(), Error> {
        self.check()?;
        let update = Update::decode_v1(update).map_err(|_| invalid("the update isn't readable"))?;
        let doc = &self.doc;
        let applied = catch_unwind(AssertUnwindSafe(|| -> Result<(), Error> {
            let mut txn = doc.try_transact_mut().map_err(|_| busy())?;
            txn.apply_update(update).map_err(|_| invalid("the update doesn't fit the shared document"))
        }));
        match applied {
            Ok(result) => result,
            Err(_) => {
                self.damaged = true;
                Err(damaged())
            }
        }
    }

    /// Writes what changed in `project` since the last publish or adoption, and returns it as
    /// an update for the other peers (`None` when nothing did).
    pub fn publish(&mut self, project: &Project) -> Result<Option<Vec<u8>>, Error> {
        self.check()?;
        let shadow = self.shadow.as_ref().ok_or_else(|| Error::new(ErrorKind::Conflict, "the shared document hasn't arrived yet"))?;
        if shadow.id() != project.id() {
            return Err(Error::new(ErrorKind::Conflict, "this is a different project from the one shared"));
        }
        let writes = changed(shadow, project)?;
        if writes.is_empty() {
            return Ok(None);
        }
        let update = self.write(|txn| {
            apply(txn, writes);
            Ok(())
        })?;
        self.shadow = Some(project.clone());
        Ok(Some(update))
    }

    /// Records that the local document now is `project`: what later publishes compare with.
    pub fn adopt(&mut self, project: &Project) {
        self.shadow = Some(project.clone());
    }

    /// The project the shared document holds now, or `None` while it is still arriving (no
    /// project yet, or an update waiting for one it builds on).
    pub fn materialize(&mut self) -> Result<Option<Materialized>, Error> {
        self.check()?;
        let read = {
            let txn = self.read()?;
            if txn.has_missing_updates() {
                return Ok(None);
            }
            read(&txn)?
        };
        let Some(Read { id, parts, bad }) = read else { return Ok(None) };
        let Assembled { project, dropped, repaired } = assemble::assemble(id, parts)?;
        let mut writes: BTreeMap<Key, Option<String>> = bad.into_iter().map(|key| (key, None)).collect();
        let params = if dropped.iter().any(|part| matches!(part, Part::Node(..))) { params_by_card(&self.doc)? } else { BTreeMap::new() };
        for part in &dropped {
            for key in part_keys(&params, part) {
                writes.insert(key, None);
            }
        }
        for part in &repaired {
            writes.extend(part_entries(&project, part)?);
        }
        let writes = effective(&self.doc, writes)?;
        let repairs = if writes.is_empty() {
            None
        } else {
            Some(self.write(|txn| {
                apply(txn, writes);
                Ok(())
            })?)
        };
        Ok(Some(Materialized { project, repairs, dropped: dropped.len() }))
    }

    fn check(&self) -> Result<(), Error> {
        if self.damaged { Err(damaged()) } else { Ok(()) }
    }

    fn read(&self) -> Result<yrs::Transaction<'_>, Error> {
        self.doc.try_transact().map_err(|_| busy())
    }

    /// Runs `edit` in one transaction and returns what it changed as an update.
    fn write(&mut self, edit: impl FnOnce(&mut TransactionMut<'_>) -> Result<(), Error>) -> Result<Vec<u8>, Error> {
        let doc = &self.doc;
        let written = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<u8>, Error> {
            let mut txn = doc.try_transact_mut().map_err(|_| busy())?;
            edit(&mut txn)?;
            Ok(txn.encode_update_v1())
        }));
        written.unwrap_or_else(|_| {
            self.damaged = true;
            Err(damaged())
        })
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidInput, message)
}
fn busy() -> Error {
    Error::new(ErrorKind::Busy, "the shared document is in use")
}
fn damaged() -> Error {
    Error::new(ErrorKind::Internal, "the shared document is damaged: leave the session and join it again")
}

/// An untitled project with nothing in it: what sharing writes the whole project against.
fn empty(id: ProjectId) -> Result<Project, Error> {
    Project::new(id, "")
}

fn json(value: &impl Serialize) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|_| Error::new(ErrorKind::Internal, "a part can't be written for sharing"))
}

fn apply(txn: &mut TransactionMut<'_>, writes: BTreeMap<Key, Option<String>>) {
    let mut maps: BTreeMap<&'static str, MapRef> = BTreeMap::new();
    for ((name, key), value) in writes {
        let map = maps.entry(name).or_insert_with(|| txn.get_or_insert_map(name)).clone();
        match value {
            Some(value) => {
                map.insert(txn, key, Any::String(Arc::from(value)));
            }
            None => {
                map.remove(txn, &key);
            }
        }
    }
}

fn in_canvas(canvas: CanvasId, id: impl std::fmt::Display) -> String {
    format!("{canvas}/{id}")
}

/// The entries one part writes.
fn node_entries(canvas: CanvasId, node: &Node) -> Result<BTreeMap<Key, String>, Error> {
    let at = in_canvas(canvas, node.id);
    let mut entries = BTreeMap::new();
    let mut core = serde_json::to_value(node).map_err(|_| Error::new(ErrorKind::Internal, "a card can't be written for sharing"))?;
    if let Json::Object(fields) = &mut core {
        fields.remove("pos");
        fields.remove("params");
    }
    entries.insert((NODES, at.clone()), json(&core)?);
    entries.insert((POSITIONS, at.clone()), json(&node.pos)?);
    for (name, value) in &node.params {
        entries.insert((PARAMS, format!("{at}/{name}")), json(value)?);
    }
    Ok(entries)
}

/// The entries that differ between `old` and `new`, as writes (`None` removes one).
fn diff_entries(old: BTreeMap<Key, String>, new: BTreeMap<Key, String>, writes: &mut BTreeMap<Key, Option<String>>) {
    for key in old.keys() {
        if !new.contains_key(key) {
            writes.insert(key.clone(), None);
        }
    }
    for (key, value) in new {
        if old.get(&key) != Some(&value) {
            writes.insert(key, Some(value));
        }
    }
}

/// Calls `each` for every id whose entity differs between two maps.
fn each_changed<K: Ord + Copy, V: PartialEq>(
    old: Option<&BTreeMap<K, V>>,
    new: Option<&BTreeMap<K, V>>,
    mut each: impl FnMut(Option<&V>, Option<&V>) -> Result<(), Error>,
) -> Result<(), Error> {
    let keys: BTreeSet<K> = old.into_iter().flat_map(|m| m.keys().copied()).chain(new.into_iter().flat_map(|m| m.keys().copied())).collect();
    for key in keys {
        let (a, b) = (old.and_then(|m| m.get(&key)), new.and_then(|m| m.get(&key)));
        if a != b {
            each(a, b)?;
        }
    }
    Ok(())
}

fn canvas_entry(canvas: &Canvas) -> CanvasEntry {
    if canvas.kind() == CanvasKind::Technique {
        CanvasEntry::Frozen(Box::new(canvas.clone()))
    } else {
        CanvasEntry::Header { title: canvas.title().to_owned(), kind: canvas.kind() }
    }
}

/// The canvas, if its parts have entries of their own (a frozen canvas is all in one).
fn open(canvas: Option<&Canvas>) -> Option<&Canvas> {
    canvas.filter(|c| c.kind() != CanvasKind::Technique)
}

/// The writes that turn the entries of `old` into those of `new`.
fn changed(old: &Project, new: &Project) -> Result<BTreeMap<Key, Option<String>>, Error> {
    let mut writes = BTreeMap::new();
    if old.title() != new.title() {
        writes.insert((META, TITLE.to_owned()), Some(json(&new.title())?));
    }
    each_changed(Some(old.skills()), Some(new.skills()), |a, b| {
        let key = (SKILLS, a.or(b).map(|s| s.id.to_string()).unwrap_or_default());
        writes.insert(key, b.map(json).transpose()?);
        Ok(())
    })?;
    each_changed(Some(old.canvases()), Some(new.canvases()), |a, b| {
        let id = a.or(b).map(Canvas::id).ok_or_else(|| invalid("a canvas without an id"))?;
        let (a_entry, b_entry) = (a.map(canvas_entry).map(|e| json(&e)).transpose()?, b.map(canvas_entry).map(|e| json(&e)).transpose()?);
        if a_entry != b_entry {
            writes.insert((CANVASES, id.to_string()), b_entry);
        }
        // A frozen canvas is all in its own entry.
        let (a, b) = (open(a), open(b));
        each_changed(a.map(Canvas::nodes), b.map(Canvas::nodes), |x, y| {
            let old = x.map(|n| node_entries(id, n)).transpose()?.unwrap_or_default();
            let new = y.map(|n| node_entries(id, n)).transpose()?.unwrap_or_default();
            diff_entries(old, new, &mut writes);
            Ok(())
        })?;
        let mut simple = |map: &'static str, key: Option<String>, value: Option<String>| {
            if let Some(key) = key {
                writes.insert((map, key), value);
            }
        };
        each_changed(a.map(Canvas::edges), b.map(Canvas::edges), |x, y| {
            simple(EDGES, x.or(y).map(|e: &Edge| in_canvas(id, e.id)), y.map(json).transpose()?);
            Ok(())
        })?;
        each_changed(a.map(Canvas::fields), b.map(Canvas::fields), |x, y| {
            simple(FIELDS, x.or(y).map(|f: &Field| in_canvas(id, f.id)), y.map(json).transpose()?);
            Ok(())
        })?;
        each_changed(a.map(Canvas::tags), b.map(Canvas::tags), |x, y| {
            simple(TAGS, x.or(y).map(|t: &Tag| in_canvas(id, t.id)), y.map(json).transpose()?);
            Ok(())
        })?;
        each_changed(a.map(Canvas::comments), b.map(Canvas::comments), |x, y| {
            simple(COMMENTS, x.or(y).map(|c: &Comment| in_canvas(id, c.id)), y.map(json).transpose()?);
            Ok(())
        })?;
        Ok(())
    })?;
    Ok(writes)
}

/// What a document read gave: the project's parts, and entries nothing can read.
struct Read {
    id: ProjectId,
    parts: Parts,
    bad: Vec<Key>,
}

/// The string entries of one map.
fn entries<T: ReadTxn>(txn: &T, name: &'static str, bad: &mut Vec<Key>) -> Vec<(String, String)> {
    let Some(map) = txn.get_map(name) else { return Vec::new() };
    let mut out = Vec::new();
    for (key, value) in map.iter(txn) {
        match value {
            Out::Any(Any::String(text)) => out.push((key.to_owned(), text.to_string())),
            _ => bad.push((name, key.to_owned())),
        }
    }
    out
}

/// `canvas/id` keys, and what the id is. Only the one spelling an id is written in is read:
/// another (upper case, braces) would be a second entry for the same part.
fn split<I: std::str::FromStr + std::fmt::Display>(key: &str) -> Option<(CanvasId, I)> {
    let (canvas, id) = key.split_once('/')?;
    let (canvas, id): (CanvasId, I) = (canvas.parse().ok()?, id.parse().ok()?);
    (in_canvas(canvas, &id) == key).then_some((canvas, id))
}

fn read<T: ReadTxn>(txn: &T) -> Result<Option<Read>, Error> {
    let mut bad = Vec::new();
    let meta: BTreeMap<String, String> = entries(txn, META, &mut bad).into_iter().collect();
    let Some(id) = meta.get(ID).and_then(|id| id.parse::<ProjectId>().ok()) else { return Ok(None) };
    let mut parts = Parts { title: meta.get(TITLE).and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default(), ..Parts::default() };
    for (key, value) in entries(txn, CANVASES, &mut bad) {
        let Some(canvas) = key.parse::<CanvasId>().ok().filter(|canvas| canvas.to_string() == key) else {
            bad.push((CANVASES, key));
            continue;
        };
        match serde_json::from_str::<CanvasEntry>(&value) {
            Ok(CanvasEntry::Header { title, kind }) => parts.canvases.push(CanvasPart::Header { id: canvas, title, kind }),
            Ok(CanvasEntry::Frozen(frozen)) if frozen.id() == canvas => parts.canvases.push(CanvasPart::Frozen(frozen)),
            _ => bad.push((CANVASES, key)),
        }
    }
    let mut positions: BTreeMap<String, Position> = BTreeMap::new();
    for (key, value) in entries(txn, POSITIONS, &mut bad) {
        match serde_json::from_str(&value) {
            Ok(pos) => {
                positions.insert(key, pos);
            }
            Err(_) => bad.push((POSITIONS, key)),
        }
    }
    let mut params: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    let mut params_keys = Vec::new();
    let mut cards = BTreeSet::new();
    for (key, value) in entries(txn, PARAMS, &mut bad) {
        let mut pieces = key.splitn(3, '/');
        let (Some(canvas), Some(node), Some(name)) = (pieces.next(), pieces.next(), pieces.next()) else {
            bad.push((PARAMS, key));
            continue;
        };
        match serde_json::from_str::<Value>(&value) {
            Ok(value) => {
                let card = format!("{canvas}/{node}");
                params.entry(card.clone()).or_default().insert(name.to_owned(), value);
                params_keys.push((card, key));
            }
            Err(_) => bad.push((PARAMS, key)),
        }
    }
    for (key, value) in entries(txn, NODES, &mut bad) {
        let node = split::<nodecraft_types::NodeId>(&key).and_then(|(canvas, id)| {
            let mut core: Json = serde_json::from_str(&value).ok()?;
            let fields = core.as_object_mut()?;
            fields.insert("pos".into(), serde_json::to_value(positions.get(&key).copied().unwrap_or_default()).ok()?);
            fields.insert("params".into(), serde_json::to_value(params.get(&key).cloned().unwrap_or_default()).ok()?);
            let node: Node = serde_json::from_value(core).ok()?;
            (node.id == id).then_some((canvas, node))
        });
        match node {
            Some(node) => {
                cards.insert(key);
                parts.nodes.push(node);
            }
            None => bad.push((NODES, key)),
        }
    }
    // A position or parameter written while someone removed its card belongs to nothing.
    bad.extend(positions.keys().filter(|key| !cards.contains(*key)).map(|key| (POSITIONS, key.clone())));
    bad.extend(params_keys.into_iter().filter(|(card, _)| !cards.contains(card)).map(|(_, key)| (PARAMS, key)));
    simple(txn, EDGES, &mut bad, &mut parts.edges, |e: &Edge| e.id)?;
    simple(txn, FIELDS, &mut bad, &mut parts.fields, |f: &Field| f.id)?;
    simple(txn, TAGS, &mut bad, &mut parts.tags, |t: &Tag| t.id)?;
    simple(txn, COMMENTS, &mut bad, &mut parts.comments, |c: &Comment| c.id)?;
    for (key, value) in entries(txn, SKILLS, &mut bad) {
        match serde_json::from_str::<Skill>(&value) {
            Ok(skill) if skill.id.to_string() == key => parts.skills.push(skill),
            _ => bad.push((SKILLS, key)),
        }
    }
    Ok(Some(Read { id, parts, bad }))
}

/// Reads one map of `canvas/id` entries whose value is the part itself.
fn simple<T: ReadTxn, P: serde::de::DeserializeOwned, I: std::str::FromStr + std::fmt::Display + PartialEq>(
    txn: &T,
    name: &'static str,
    bad: &mut Vec<Key>,
    into: &mut Vec<(CanvasId, P)>,
    id: impl Fn(&P) -> I,
) -> Result<(), Error> {
    for (key, value) in entries(txn, name, bad) {
        let part = split::<I>(&key).and_then(|(canvas, at)| {
            let part: P = serde_json::from_str(&value).ok()?;
            (id(&part) == at).then_some((canvas, part))
        });
        match part {
            Some(part) => into.push(part),
            None => bad.push((name, key)),
        }
    }
    Ok(())
}

/// Every parameter entry in the document, by its card's `canvas/node` key.
fn params_by_card(doc: &Doc) -> Result<BTreeMap<String, Vec<String>>, Error> {
    let txn = doc.try_transact().map_err(|_| busy())?;
    let mut by_card: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(map) = txn.get_map(PARAMS) {
        for key in map.keys(&txn) {
            let mut pieces = key.splitn(3, '/');
            if let (Some(canvas), Some(node), Some(_)) = (pieces.next(), pieces.next(), pieces.next()) {
                by_card.entry(format!("{canvas}/{node}")).or_default().push(key.to_owned());
            }
        }
    }
    Ok(by_card)
}

/// Every entry a part has in the document now; `params` from [`params_by_card`].
fn part_keys(params: &BTreeMap<String, Vec<String>>, part: &Part) -> Vec<Key> {
    match part {
        Part::Title => vec![(META, TITLE.to_owned())],
        Part::Canvas(canvas) => vec![(CANVASES, canvas.to_string())],
        Part::Node(canvas, node) => {
            let at = in_canvas(*canvas, node);
            let mut keys: Vec<Key> = params.get(&at).into_iter().flatten().map(|key| (PARAMS, key.clone())).collect();
            keys.extend([(NODES, at.clone()), (POSITIONS, at)]);
            keys
        }
        Part::Edge(canvas, id) => vec![(EDGES, in_canvas(*canvas, id))],
        Part::Field(canvas, id) => vec![(FIELDS, in_canvas(*canvas, id))],
        Part::Tag(canvas, id) => vec![(TAGS, in_canvas(*canvas, id))],
        Part::Comment(canvas, id) => vec![(COMMENTS, in_canvas(*canvas, id))],
        Part::Skill(id) => vec![(SKILLS, id.to_string())],
    }
}

/// The writes that would change the document: a value it doesn't hold yet, or a removal of an
/// entry it has. A repair that changes nothing sends nothing, so peers can't trade empty
/// repairs back and forth.
fn effective(doc: &Doc, writes: BTreeMap<Key, Option<String>>) -> Result<BTreeMap<Key, Option<String>>, Error> {
    let txn = doc.try_transact().map_err(|_| busy())?;
    Ok(writes
        .into_iter()
        .filter(|((name, key), value)| match (txn.get_map(*name).and_then(|map| map.get(&txn, key)), value) {
            (None, None) => false,
            (Some(Out::Any(Any::String(now))), Some(value)) => &*now != value.as_str(),
            _ => true,
        })
        .collect())
}

/// The entries a repaired part writes, from the assembled project.
fn part_entries(project: &Project, part: &Part) -> Result<BTreeMap<Key, Option<String>>, Error> {
    let mut writes = BTreeMap::new();
    match part {
        Part::Title => {
            writes.insert((META, TITLE.to_owned()), Some(json(&project.title())?));
        }
        Part::Node(canvas, id) => {
            if let Some(node) = project.canvases().get(canvas).and_then(|c| c.nodes().get(id)) {
                // Only the card's own entry changes (its field or tag went).
                for (key, value) in node_entries(*canvas, node)? {
                    if key.0 == NODES {
                        writes.insert(key, Some(value));
                    }
                }
            }
        }
        Part::Comment(canvas, id) => {
            if let Some(comment) = project.canvases().get(canvas).and_then(|c| c.comments().get(id)) {
                writes.insert((COMMENTS, in_canvas(*canvas, id)), Some(json(comment)?));
            }
        }
        _ => {}
    }
    Ok(writes)
}

#[cfg(test)]
mod tests;
