//! Bounded tag-filter view state; no document mutation or recursive node inspection.
use nodecraft_graph::Project;
use nodecraft_types::{CanvasId, NodeId, ProjectId, TagId};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Bound::{Excluded, Unbounded},
};
pub const FRAME_NODES: usize = 256;
pub const PALETTE: [(&str, &str); 8] = [
    ("red", "Red"),
    ("orange", "Orange"),
    ("yellow", "Yellow"),
    ("green", "Green"),
    ("teal", "Teal"),
    ("blue", "Blue"),
    ("purple", "Purple"),
    ("pink", "Pink"),
];
#[derive(Default)]
pub struct TagFilter {
    pub open: bool,
    pub tags: BTreeSet<TagId>,
    pub untagged: bool,
    pub presentation: Vec<TagId>,
    pub results: Vec<NodeId>,
    pub current: Option<NodeId>,
    pub frame_request: Option<NodeId>,
    pub scanning: bool,
    pub last_work: usize,
    index_cursor: Option<TagId>,
    index_draining: bool,
    index_order: BTreeMap<(String, TagId), ()>,
    index_build: Vec<TagId>,
    pub indexing: bool,
    pub index_work: usize,
    stamp: Option<(ProjectId, CanvasId, u64)>,
    building_results: Vec<NodeId>,
    cursor: Option<NodeId>,
    pending: Option<bool>,
}
impl TagFilter {
    pub fn active(&self) -> bool {
        !self.tags.is_empty() || self.untagged
    }
    pub fn set(&mut self, tags: BTreeSet<TagId>, untagged: bool) {
        if self.tags != tags || self.untagged != untagged {
            self.tags = tags;
            self.untagged = untagged;
            self.restart();
        }
    }
    fn restart(&mut self) {
        self.results.clear();
        self.current = None;
        self.begin_scan();
    }
    fn begin_scan(&mut self) {
        self.building_results.clear();
        self.frame_request = None;
        self.cursor = None;
        self.pending = None;
        self.scanning = self.active();
    }
    pub fn sync(&mut self, p: &Project, c: CanvasId, revision: u64) {
        let stamp = (p.id(), c, revision);
        if self.stamp != Some(stamp) {
            if self.stamp.is_some_and(|s| s.0 != p.id() || s.1 != c) {
                self.tags.clear();
                self.untagged = false;
            }
            if let Some(canvas) = p.canvases().get(&c) {
                self.tags.retain(|id| canvas.tags().contains_key(id));
            } else {
                self.tags.clear();
                self.untagged = false;
            }
            self.index_cursor = None;
            self.index_draining = false;
            self.index_order.clear();
            self.index_build.clear();
            self.indexing = true;
            self.stamp = Some(stamp);
            if !self.active() {
                self.results.clear();
                self.current = None;
            } else if self.current.is_some_and(|id| p.canvases().get(&c).is_none_or(|canvas| !canvas.nodes().contains_key(&id))) {
                self.current = None;
            }
            self.begin_scan();
        }
    }
    pub fn cancel_navigation(&mut self) {
        self.pending = None;
        self.frame_request = None;
    }
    pub fn advance(&mut self, previous: bool) {
        if self.scanning {
            self.pending = Some(previous);
        } else {
            self.choose(previous);
        }
    }
    fn choose(&mut self, previous: bool) {
        let len = self.results.len();
        if len == 0 {
            return;
        }
        let index = self.current.and_then(|id| self.results.binary_search(&id).ok());
        let next = match (index, previous) {
            (Some(0), true) | (None, true) => len - 1,
            (Some(i), true) => i - 1,
            (Some(i), false) => (i + 1) % len,
            (None, false) => 0,
        };
        self.current = self.results.get(next).copied();
        self.frame_request = self.current;
    }
    pub fn tick(&mut self, p: &Project, c: CanvasId, revision: u64) {
        self.sync(p, c, revision);
        self.last_work = 0;
        self.index_work = 0;
        if self.indexing {
            if let Some(canvas) = p.canvases().get(&c) {
                if !self.index_draining {
                    let start = self.index_cursor.map_or(Unbounded, Excluded);
                    let mut iter = canvas.tags().range((start, Unbounded)).peekable();
                    for _ in 0..FRAME_NODES {
                        let Some((id, tag)) = iter.next() else {
                            break;
                        };
                        self.index_cursor = Some(*id);
                        self.index_work += 1;
                        self.index_order.insert((label(&tag.title), *id), ());
                    }
                    if iter.peek().is_none() {
                        self.index_draining = true;
                    }
                } else {
                    for _ in 0..FRAME_NODES {
                        let Some(((_, id), ())) = self.index_order.pop_first() else {
                            break;
                        };
                        self.index_build.push(id);
                        self.index_work += 1;
                    }
                    if self.index_order.is_empty() {
                        self.presentation = std::mem::take(&mut self.index_build);
                        self.indexing = false;
                    }
                }
            } else {
                self.presentation.clear();
                self.indexing = false;
            }
        }
        if !self.scanning {
            return;
        }
        let Some(canvas) = p.canvases().get(&c) else {
            self.scanning = false;
            return;
        };
        let start = self.cursor.map_or(Unbounded, Excluded);
        let mut iter = canvas.nodes().range((start, Unbounded)).peekable();
        for _ in 0..FRAME_NODES {
            let Some((id, node)) = iter.next() else {
                break;
            };
            self.cursor = Some(*id);
            self.last_work += 1;
            if node.tag.map_or(self.untagged, |id| self.tags.contains(&id)) {
                self.building_results.push(*id);
            }
        }
        self.scanning = iter.peek().is_some();
        if !self.scanning {
            self.results = std::mem::take(&mut self.building_results);
            self.current = self.current.filter(|id| self.results.binary_search(id).is_ok());
            if let Some(previous) = self.pending.take() {
                self.choose(previous);
            }
        }
    }
    pub fn matches(&self, id: NodeId) -> bool {
        self.results.binary_search(&id).is_ok()
    }
}
/// Display text is bounded separately from the preserved document title.
pub fn label(title: &str) -> String {
    let mut iter = title.chars();
    let mut s: String = iter.by_ref().take(128).map(|c| if c.is_control() { ' ' } else { c }).collect();
    if iter.next().is_some() {
        s.push('…');
    }
    if s.trim().is_empty() {
        s = "Untitled tag".into();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodecraft_graph::{Canvas, Change, Changeset, Node, Tag};
    fn fixture(count: usize) -> (Project, CanvasId, TagId) {
        let c = CanvasId::from_bytes([1; 16]);
        let t = TagId::from_bytes([2; 16]);
        let mut p = Project::new(ProjectId::from_bytes([0; 16]), "Tags").unwrap();
        Change::AddCanvas { canvas: Canvas::new(c, "Canvas") }.apply(&mut p).unwrap();
        Change::PutTag { canvas: c, tag: Tag { id: t, title: "Blue".into(), color: Some("blue".into()) } }.apply(&mut p).unwrap();
        let changes = (0..count)
            .map(|n| {
                let mut node = Node::new(NodeId::from_bytes((n as u128).to_be_bytes()), "test.text");
                if n % 2 == 0 {
                    node.tag = Some(t);
                }
                Change::AddNode { canvas: c, node }
            })
            .collect();
        Changeset::new(changes).apply(&mut p).unwrap();
        (p, c, t)
    }
    #[test]
    fn ten_thousand_node_scan_has_bounded_work_and_defers_wrapping_jump() {
        let (p, c, t) = fixture(10000);
        let mut f = TagFilter::default();
        f.sync(&p, c, 0);
        f.set(BTreeSet::from([t]), false);
        f.advance(false);
        let mut frames = 0;
        while f.scanning {
            f.tick(&p, c, 0);
            assert!(f.last_work <= FRAME_NODES);
            if f.scanning {
                assert_eq!(f.frame_request, None);
            }
            frames += 1;
        }
        assert_eq!(frames, 40);
        assert_eq!(f.results.len(), 5000);
        assert_eq!(f.current, Some(NodeId::from_bytes(0u128.to_be_bytes())));
        f.advance(true);
        assert_eq!(f.current, Some(NodeId::from_bytes(9998u128.to_be_bytes())));
    }
    #[test]
    fn or_untagged_clear_and_revision_cancel_stale_navigation() {
        let (mut p, c, t) = fixture(5);
        let mut f = TagFilter::default();
        f.sync(&p, c, 0);
        f.set(BTreeSet::from([t]), true);
        f.advance(false);
        f.tick(&p, c, 0);
        assert_eq!(f.results.len(), 5);
        assert!(f.current.is_some());
        Change::RemoveNode { canvas: c, id: NodeId::from_bytes([0; 16]) }.apply(&mut p).unwrap();
        f.sync(&p, c, 1);
        assert_eq!(f.frame_request, None);
        assert_eq!(f.current, None);
        f.tick(&p, c, 1);
        assert_eq!(f.results.len(), 4);
        f.set(BTreeSet::new(), false);
        assert!(!f.active());
        assert!(f.results.is_empty());
    }
    #[test]
    fn revision_refresh_publishes_atomically_and_preserves_navigation() {
        let (mut p, c, t) = fixture(1000);
        let mut f = TagFilter::default();
        f.sync(&p, c, 0);
        f.set(BTreeSet::from([t]), false);
        while f.scanning {
            f.tick(&p, c, 0);
        }
        for _ in 0..5 {
            f.advance(false);
        }
        let current = f.current;
        let results = f.results.clone();
        Change::PutTag { canvas: c, tag: Tag { id: t, title: "Renamed".into(), color: Some("blue".into()) } }.apply(&mut p).unwrap();
        f.tick(&p, c, 1);
        assert!(f.scanning);
        assert_eq!(f.results, results);
        assert_eq!(f.current, current);
        assert_eq!(f.frame_request, None);
        f.advance(false);
        while f.scanning {
            f.tick(&p, c, 1);
        }
        assert_eq!(f.results, results);
        assert_eq!(f.current, Some(NodeId::from_bytes(10u128.to_be_bytes())));
    }
    #[test]
    fn title_formatting_is_unicode_bounded() {
        let text = "界".repeat(4096);
        assert_eq!(label(&text).chars().count(), 129);
        assert_eq!(label(""), "Untitled tag");
    }
    #[test]
    fn presentation_rebuild_admits_and_drains_at_most_256_bounded_labels_per_frame() {
        let (mut p, c, _) = fixture(0);
        let changes = (3u128..=10001)
            .map(|n| Change::PutTag {
                canvas: c,
                tag: Tag { id: TagId::from_bytes(n.to_be_bytes()), title: format!("Same\n{}", "x".repeat(500)), color: None },
            })
            .collect();
        Changeset::new(changes).apply(&mut p).unwrap();
        let mut f = TagFilter::default();
        f.sync(&p, c, 0);
        let mut frames = 0;
        while f.indexing {
            f.tick(&p, c, 0);
            assert!(f.index_work <= FRAME_NODES);
            frames += 1;
        }
        assert_eq!(f.presentation.len(), 10000);
        assert_eq!(frames, 80);
        for rev in 1..=5 {
            f.tick(&p, c, rev);
            assert!(f.index_work <= FRAME_NODES);
            assert_eq!(f.index_order.len(), FRAME_NODES);
            assert_eq!(f.presentation.len(), 10000);
        }
    }
    #[test]
    fn multiple_tag_ids_or_with_untagged_and_zero_match_filter() {
        let (mut p, c, a) = fixture(6);
        let b = TagId::from_bytes([7; 16]);
        let empty = TagId::from_bytes([8; 16]);
        Changeset::new(vec![
            Change::PutTag { canvas: c, tag: Tag { id: b, title: "Other".into(), color: None } },
            Change::PutTag { canvas: c, tag: Tag { id: empty, title: "Empty".into(), color: None } },
            Change::SetNodeTags {
                canvas: c,
                assignments: vec![nodecraft_graph::NodeTagAssignment { node: NodeId::from_bytes(1u128.to_be_bytes()), tag: Some(b) }],
            },
        ])
        .apply(&mut p)
        .unwrap();
        let mut f = TagFilter::default();
        f.sync(&p, c, 0);
        f.set(BTreeSet::from([a, b]), false);
        f.tick(&p, c, 0);
        assert_eq!(f.results.len(), 4);
        f.set(BTreeSet::from([a, b]), true);
        f.tick(&p, c, 0);
        assert_eq!(f.results.len(), 6);
        f.set(BTreeSet::from([empty]), false);
        f.advance(false);
        f.tick(&p, c, 0);
        assert!(f.results.is_empty());
        assert_eq!(f.frame_request, None);
        assert_eq!(f.current, None);
    }
}
