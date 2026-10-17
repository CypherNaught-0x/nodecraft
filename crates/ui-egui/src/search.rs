//! Incremental literal search over borrowed persisted text. No prompt/index copies.
use nodecraft_graph::{Canvas, Node, Project};
use nodecraft_types::{CanvasId, Error, ErrorKind, NodeId, ProjectId, Value};
use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

pub const MAX_QUERY_BYTES: usize = nodecraft_engine::search_cmds::MAX_QUERY_BYTES;
pub const FRAME_BYTES: usize = 32 * 1024;
pub const FRAME_VISITS: usize = 4096;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    First,
    Next,
    Previous,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct Work {
    pub bytes: usize,
    pub visits: usize,
}
#[derive(Default)]
struct Pattern {
    chars: Vec<char>,
    prefix: Vec<usize>,
}
impl Pattern {
    fn new(query: &str) -> Self {
        let chars: Vec<_> = query.chars().flat_map(char::to_lowercase).collect();
        let mut prefix = vec![0; chars.len()];
        let mut matched = 0;
        for (index, ch) in chars.iter().enumerate().skip(1) {
            while matched > 0 && chars.get(matched) != Some(ch) {
                matched = prefix.get(matched - 1).copied().unwrap_or(0);
            }
            if chars.get(matched) == Some(ch) {
                matched += 1;
            }
            if let Some(slot) = prefix.get_mut(index) {
                *slot = matched;
            }
        }
        Self { chars, prefix }
    }
    fn feed(&self, matched: &mut usize, ch: char) -> bool {
        for folded in ch.to_lowercase() {
            while *matched > 0 && self.chars.get(*matched) != Some(&folded) {
                *matched = self.prefix.get(*matched - 1).copied().unwrap_or(0);
            }
            if self.chars.get(*matched) == Some(&folded) {
                *matched += 1;
            }
            if *matched == self.chars.len() {
                return true;
            }
        }
        false
    }
}
#[derive(Default)]
struct Cursor {
    id: Option<NodeId>,
    phase: u8,
    key: Option<String>,
    path: Vec<usize>,
    offset: usize,
    matched: usize,
}
impl Cursor {
    fn next_phase(&mut self) {
        self.phase = self.phase.saturating_add(1);
        self.key = None;
        self.path.clear();
        self.offset = 0;
        self.matched = 0;
    }
    fn map<'a>(&self, node: &'a Node) -> &'a BTreeMap<String, Value> {
        if self.phase == 3 { &node.params } else { &node.config }
    }
    fn value<'a>(&self, map: &'a BTreeMap<String, Value>, path: &[usize]) -> Option<&'a Value> {
        let mut value = map.get(self.key.as_ref()?)?;
        for index in path {
            let Value::List(items) = value else { return None };
            value = items.get(*index)?;
        }
        Some(value)
    }
    fn next_value(&mut self, map: &BTreeMap<String, Value>) {
        self.offset = 0;
        self.matched = 0;
        while let Some(last) = self.path.last().copied() {
            let parent = self.path.get(..self.path.len().saturating_sub(1)).and_then(|path| self.value(map, path));
            if let Some(Value::List(items)) = parent
                && last.checked_add(1).is_some_and(|next| next < items.len())
            {
                if let Some(index) = self.path.last_mut() {
                    *index = last.saturating_add(1);
                }
                return;
            }
            self.path.pop();
        }
        let next = self.key.as_ref().and_then(|key| map.range::<str, _>((Excluded(key.as_str()), Unbounded)).next()).map(|(key, _)| key.clone());
        self.key = next;
        if self.key.is_none() {
            self.next_phase();
        }
    }
    fn text(&mut self, text: &str, pattern: &Pattern, work: &mut Work) -> TextProgress {
        let Some(suffix) = text.get(self.offset..) else { return TextProgress::Done };
        for ch in suffix.chars() {
            if work.bytes.saturating_add(ch.len_utf8()) > FRAME_BYTES {
                return TextProgress::Yield;
            }
            work.bytes += ch.len_utf8();
            self.offset += ch.len_utf8();
            if pattern.feed(&mut self.matched, ch) {
                return TextProgress::Match;
            }
        }
        TextProgress::Done
    }
    fn step(&mut self, node: &Node, canvas: &Canvas, labels: &BTreeMap<NodeId, String>, pattern: &Pattern, work: &mut Work) -> TextProgress {
        // A DFS sibling advance can walk the bounded path repeatedly. Charge its square so
        // empty/nested values and lookups cannot hide unbounded work behind a zero-byte field.
        let cost = self.path.len().saturating_mul(self.path.len()).saturating_add(4);
        if work.visits.saturating_add(cost) > FRAME_VISITS {
            return TextProgress::Yield;
        }
        work.visits += cost;
        if self.phase < 3 {
            let text = match self.phase {
                0 => node.title.as_str(),
                1 => labels.get(&node.id).map_or("", String::as_str),
                2 => node.tag.and_then(|id| canvas.tags().get(&id)).map_or("", |tag| tag.title.as_str()),
                _ => "",
            };
            let progress = self.text(text, pattern, work);
            if matches!(progress, TextProgress::Done) {
                self.next_phase();
            }
            return progress;
        }
        if self.phase > 4 {
            return TextProgress::NodeDone;
        }
        let map = self.map(node);
        if self.key.is_none() {
            self.key = map.first_key_value().map(|(key, _)| key.clone());
            if self.key.is_none() {
                self.next_phase();
                return TextProgress::Done;
            }
        }
        match self.value(map, &self.path) {
            Some(Value::Text(text)) => {
                let progress = self.text(text, pattern, work);
                if matches!(progress, TextProgress::Done) {
                    self.next_value(map);
                }
                progress
            }
            Some(Value::List(items)) if !items.is_empty() => {
                self.path.push(0);
                TextProgress::Done
            }
            _ => {
                self.next_value(map);
                TextProgress::Done
            }
        }
    }
}
enum TextProgress {
    Done,
    Yield,
    Match,
    NodeDone,
}
/// Mirrors card::title's fallback decision without copying/truncating searchable prompt text.
pub(crate) fn fallback_label(node: &Node) -> bool {
    let text = if node.title.trim().is_empty() {
        ["prompt", "text"].into_iter().find_map(|key| match node.params.get(key) {
            Some(Value::Text(text)) => Some(text.as_str()),
            _ => None,
        })
    } else {
        Some(node.title.as_str())
    };
    text.is_none_or(|text| !text.chars().take(256).any(|ch| !ch.is_whitespace()))
}
#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub draft: String,
    pub query: String,
    pub results: Vec<NodeId>,
    pub current: Option<NodeId>,
    pub scanning: bool,
    pub frame_request: Option<NodeId>,
    pub last_work: Work,
    pub scanned_nodes: usize,
    stamp: Option<(ProjectId, CanvasId, u64, usize)>,
    pattern: Pattern,
    cursor: Cursor,
    after: Option<NodeId>,
    generation: u64,
    pending: Option<(u64, Direction)>,
}
impl Search {
    pub fn set_query(&mut self, query: &str) -> Result<(), Error> {
        if query.len() > MAX_QUERY_BYTES {
            return Err(Error::new(ErrorKind::InvalidInput, "search query exceeds 1024 UTF-8 bytes"));
        }
        self.draft = query.to_owned();
        let query = query.trim();
        if self.query == query {
            return Ok(());
        }
        self.query = query.to_owned();
        self.pattern = Pattern::new(query);
        self.current = None;
        self.restart();
        self.pending = (!query.is_empty()).then_some((self.generation, Direction::First));
        Ok(())
    }
    fn restart(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        let direction = self.pending.map(|(_, direction)| direction);
        self.pending = direction.map(|direction| (self.generation, direction));
        self.results.clear();
        self.cursor = Cursor::default();
        self.after = None;
        self.scanned_nodes = 0;
        self.scanning = !self.query.is_empty();
        self.frame_request = None;
        self.stamp = None;
    }
    pub fn close(&mut self) {
        self.open = false;
        self.frame_request = None;
        self.pending = None;
    }
    pub fn advance(&mut self, direction: Direction) {
        if self.scanning {
            self.pending = Some((self.generation, direction));
            return;
        }
        self.choose(direction);
    }
    fn choose(&mut self, direction: Direction) {
        if self.results.is_empty() {
            self.current = None;
            self.frame_request = None;
            return;
        }
        let index = self.current.and_then(|id| self.results.iter().position(|candidate| *candidate == id));
        let next = match direction {
            Direction::First => 0,
            Direction::Next => index.map_or(0, |i| if i.saturating_add(1) == self.results.len() { 0 } else { i + 1 }),
            Direction::Previous => index.map_or(self.results.len() - 1, |i| if i == 0 { self.results.len() - 1 } else { i - 1 }),
        };
        self.current = self.results.get(next).copied();
        self.frame_request = self.current;
    }
    pub fn sync(&mut self, project: &Project, canvas_id: CanvasId, revision: u64) {
        let Some(canvas) = project.canvases().get(&canvas_id) else {
            self.restart();
            self.scanning = false;
            self.current = None;
            return;
        };
        let identity = canvas.nodes().first_key_value().map_or(0, |(_, node)| std::ptr::from_ref(node) as usize);
        let stamp = (project.id(), canvas_id, revision, identity);
        if self.stamp != Some(stamp) {
            self.restart();
            self.stamp = Some(stamp);
        }
    }
    pub fn tick(&mut self, project: &Project, canvas_id: CanvasId, revision: u64, labels: &BTreeMap<NodeId, String>) {
        self.last_work = Work::default();
        if !self.open {
            return;
        }
        self.sync(project, canvas_id, revision);
        let Some(canvas) = project.canvases().get(&canvas_id) else {
            return;
        };
        if !self.scanning {
            return;
        }
        while self.last_work.bytes < FRAME_BYTES && self.last_work.visits < FRAME_VISITS {
            if self.cursor.id.is_none() {
                let next = match self.after {
                    Some(id) => canvas.nodes().range((Excluded(id), Unbounded)).next(),
                    None => canvas.nodes().first_key_value(),
                };
                let Some((id, _)) = next else {
                    self.scanning = false;
                    if let Some((generation, direction)) = self.pending.take() {
                        if generation == self.generation {
                            self.choose(direction);
                        }
                    } else if self.current.is_none_or(|id| !self.results.contains(&id)) {
                        self.choose(Direction::First);
                    }
                    return;
                };
                self.cursor = Cursor { id: Some(*id), ..Cursor::default() };
            }
            let Some(id) = self.cursor.id else {
                return;
            };
            let Some(node) = canvas.nodes().get(&id) else {
                self.cursor = Cursor::default();
                self.after = Some(id);
                continue;
            };
            match self.cursor.step(node, canvas, labels, &self.pattern, &mut self.last_work) {
                TextProgress::Yield => return,
                TextProgress::Match => {
                    self.results.push(id);
                    self.after = Some(id);
                    self.cursor = Cursor::default();
                    self.scanned_nodes += 1;
                }
                TextProgress::NodeDone => {
                    self.after = Some(id);
                    self.cursor = Cursor::default();
                    self.scanned_nodes += 1;
                }
                TextProgress::Done => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Tag};
    use nodecraft_types::TagId;
    fn fixture() -> (Project, CanvasId) {
        let canvas = CanvasId::from_bytes([1; 16]);
        let mut project = Project::new(ProjectId::from_bytes([2; 16]), "Search").unwrap();
        Change::AddCanvas { canvas: Canvas::new(canvas, "Canvas") }.apply(&mut project).unwrap();
        (project, canvas)
    }
    fn add(project: &mut Project, canvas: CanvasId, n: u8, title: &str, params: Vec<(&str, Value)>) {
        let mut node = Node::new(NodeId::from_bytes([n; 16]), "source.text");
        node.title = title.into();
        node.params = params.into_iter().map(|(k, v)| (k.into(), v)).collect();
        Change::AddNode { canvas, node }.apply(project).unwrap();
    }
    fn finish(search: &mut Search, p: &Project, c: CanvasId, revision: u64, labels: &BTreeMap<NodeId, String>) -> usize {
        for frame in 1..10000 {
            search.tick(p, c, revision, labels);
            assert!(search.last_work.bytes <= FRAME_BYTES);
            assert!(search.last_work.visits <= FRAME_VISITS);
            if !search.scanning {
                return frame;
            }
        }
        panic!("search failed to make bounded progress")
    }
    #[test]
    fn searches_all_stored_fields_unicode_expansions_and_wraps_stably() {
        let (mut p, c) = fixture();
        add(&mut p, c, 3, "ÉTOILE", vec![]);
        add(&mut p, c, 4, "", vec![("text", Value::Text("x".repeat(1_048_550) + " İSTANBUL tail"))]);
        let mut node = Node::new(NodeId::from_bytes([5; 16]), "source.text");
        node.config.insert("provider.model".into(), Value::List(vec![Value::Bool(false), Value::Text("vendor/STAR-model".into())]));
        let tag = TagId::from_bytes([6; 16]);
        Change::PutTag { canvas: c, tag: Tag { id: tag, title: "STAR tag".into() } }.apply(&mut p).unwrap();
        node.tag = Some(tag);
        Change::AddNode { canvas: c, node }.apply(&mut p).unwrap();
        let labels = BTreeMap::from([(NodeId::from_bytes([3; 16]), "Dynamic STAR label".into())]);
        let mut s = Search { open: true, ..Search::default() };
        for (query, expected) in [
            (" étoile ", vec![3]),
            ("i\u{307}stanbul tail", vec![4]),
            ("STAR", vec![3, 5]),
            ("vendor/star-model", vec![5]),
            ("star tag", vec![5]),
            ("not found", vec![]),
        ] {
            s.set_query(query).unwrap();
            finish(&mut s, &p, c, 0, &labels);
            assert_eq!(s.results, expected.into_iter().map(|n| NodeId::from_bytes([n; 16])).collect::<Vec<_>>());
        }
        s.set_query("star").unwrap();
        s.advance(Direction::Previous);
        finish(&mut s, &p, c, 0, &labels);
        assert_eq!(s.current, Some(NodeId::from_bytes([5; 16])));
        s.advance(Direction::Next);
        assert_eq!(s.current, Some(NodeId::from_bytes([3; 16])));
        s.advance(Direction::Previous);
        assert_eq!(s.current, Some(NodeId::from_bytes([5; 16])));
        s.tick(&p, c, 0, &labels);
        assert_eq!((s.last_work.bytes, s.last_work.visits), (0, 0));
        s.set_query(" ").unwrap();
        finish(&mut s, &p, c, 0, &labels);
        assert!(s.results.is_empty());
        assert!(s.set_query(&"é".repeat(513)).is_err());
        assert!(s.query.is_empty());
    }
    #[test]
    fn chunks_do_not_join_fields_and_empty_structures_are_budgeted() {
        let (mut p, c) = fixture();
        add(&mut p, c, 3, "prefix", vec![("a", Value::Text("suffix".into())), ("b", Value::List((0..4000).map(|_| Value::Text(String::new())).collect()))]);
        let mut s = Search { open: true, ..Search::default() };
        s.set_query("prefixsuffix").unwrap();
        let frames = finish(&mut s, &p, c, 0, &BTreeMap::new());
        assert!(frames > 1);
        assert!(s.results.is_empty());
        s.set_query("suffix").unwrap();
        finish(&mut s, &p, c, 0, &BTreeMap::new());
        assert_eq!(s.results.len(), 1);
    }
    #[test]
    fn changed_generation_cancels_old_navigation_and_removed_reused_ids_rescan() {
        let (mut p, c) = fixture();
        add(&mut p, c, 3, "old", vec![("text", Value::Text("x".repeat(100_000) + " needle"))]);
        let mut s = Search { open: true, ..Search::default() };
        s.set_query("needle").unwrap();
        s.tick(&p, c, 0, &BTreeMap::new());
        assert!(s.scanning);
        s.advance(Direction::Next);
        s.set_query("absent").unwrap();
        finish(&mut s, &p, c, 0, &BTreeMap::new());
        assert!(s.current.is_none());
        assert!(s.frame_request.is_none());
        Change::RemoveNode { canvas: c, id: NodeId::from_bytes([3; 16]) }.apply(&mut p).unwrap();
        add(&mut p, c, 3, "absent", vec![]);
        finish(&mut s, &p, c, 1, &BTreeMap::new());
        assert_eq!(s.results.len(), 1);
        Change::RemoveNode { canvas: c, id: NodeId::from_bytes([3; 16]) }.apply(&mut p).unwrap();
        finish(&mut s, &p, c, 2, &BTreeMap::new());
        assert!(s.results.is_empty());
        assert!(s.current.is_none());
    }
    #[test]
    fn partial_kmp_prefix_and_non_ascii_scalar_survive_a_byte_chunk_boundary() {
        let (mut p, c) = fixture();
        add(&mut p, c, 3, "", vec![("text", Value::Text("x".repeat(FRAME_BYTES - 3) + "AbÉZ"))]);
        let mut s = Search { open: true, ..Search::default() };
        s.set_query("abéz").unwrap();
        s.tick(&p, c, 0, &BTreeMap::new());
        assert!(s.scanning);
        assert!(s.results.is_empty());
        assert_eq!(s.last_work.bytes, FRAME_BYTES - 1, "a two-byte scalar cannot consume the final one-byte budget");
        finish(&mut s, &p, c, 0, &BTreeMap::new());
        assert_eq!(s.results, vec![NodeId::from_bytes([3; 16])]);
    }
    #[test]
    fn fallback_decision_matches_the_painted_prefix_even_for_long_leading_title_whitespace() {
        let mut node = Node::new(NodeId::from_bytes([3; 16]), "source.text");
        assert!(fallback_label(&node));
        node.title = " ".repeat(256) + "later explicit title";
        assert!(fallback_label(&node));
        node.title = " Visible title ".into();
        assert!(!fallback_label(&node));
        node.title.clear();
        node.params.insert("text".into(), Value::Text(" ".repeat(256) + "late prompt"));
        assert!(fallback_label(&node));
        node.params.insert("text".into(), Value::Text("visible prompt".into()));
        assert!(!fallback_label(&node));
    }
}
